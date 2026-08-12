#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <termios.h>
#include <time.h>
#include <unistd.h>

static volatile sig_atomic_t termination_signal = 0;
static struct termios original_termios;
static bool terminal_is_raw = false;
static bool focus_mode_owned = false;
static bool bracketed_paste_mode_owned = false;
static bool kitty_keyboard_mode_owned = false;
static bool mouse_normal_mode_owned = false;
static bool mouse_button_mode_owned = false;
static bool mouse_any_mode_owned = false;
static bool mouse_sgr_mode_owned = false;
static bool mouse_pixel_mode_owned = false;
static FILE *event_log = NULL;
static bool event_log_failed = false;
static int event_log_error = 0;
static const char *capture_result = "not-started";
static bool cleanup_complete = false;
static int cleanup_outcome = 0;

struct capture_options {
    const char *log_path;
    unsigned int timeout_seconds;
    bool focus;
    bool bracketed_paste;
    bool kitty_keyboard;
    bool pixel_mouse;
    enum {
        MOUSE_OFF,
        MOUSE_NORMAL,
        MOUSE_BUTTON,
        MOUSE_ANY,
    } mouse;
};

static void usage(FILE *stream) {
    fputs(
        "Usage:\n"
        "  native-probe geometry\n"
        "  native-probe capture --log PATH [OPTIONS]\n\n"
        "Capture options:\n"
        "  --focus               Enable DEC 1004 focus reporting.\n"
        "  --bracketed-paste     Enable bracketed paste mode.\n"
        "  --kitty-keyboard      Request disambiguated press, repeat, and release events.\n"
        "  --mouse=MODE          Enable SGR mouse reporting: normal, button, or any.\n"
        "  --pixel-mouse         Add SGR-pixel coordinates (DEC 1016).\n"
        "  --timeout=SECONDS     Stop after a bounded duration (default: no timeout).\n\n"
        "Capture enters raw mode and records exact PTY bytes. Press Control-] to stop.\n"
        "INT, TERM, HUP, QUIT, and TSTP terminate capture with a logged nonzero result.\n"
        "Run capture in a disposable shell: it restores termios and disables only requested modes.\n"
        "Timestamps are monotonic post-read logging times, not byte-arrival latency.\n"
        "Use only deterministic acceptance input; the log intentionally contains typed bytes.\n",
        stream);
}

static void request_termination(int signal_number) {
    if (termination_signal == 0) {
        termination_signal = signal_number;
    }
}

static int monotonic_nanoseconds(uint64_t *value) {
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0) {
        return -1;
    }
    *value = (uint64_t)now.tv_sec * UINT64_C(1000000000) + (uint64_t)now.tv_nsec;
    return 0;
}

static int write_all(int descriptor, const char *bytes, size_t length) {
    while (length > 0) {
        ssize_t written = write(descriptor, bytes, length);
        if (written < 0) {
            if (errno == EINTR) {
                continue;
            }
            return -1;
        }
        if (written == 0) {
            errno = EIO;
            return -1;
        }
        bytes += (size_t)written;
        length -= (size_t)written;
    }
    return 0;
}

static void remember_log_error(void) {
    if (!event_log_failed) {
        event_log_failed = true;
        event_log_error = errno != 0 ? errno : EIO;
    }
}

static int log_checked(const char *format, ...) {
    if (event_log == NULL || event_log_failed) {
        return -1;
    }
    va_list arguments;
    va_start(arguments, format);
    int written = vfprintf(event_log, format, arguments);
    va_end(arguments);
    if (written < 0) {
        remember_log_error();
        return -1;
    }
    return 0;
}

static int flush_log_checked(void) {
    if (event_log == NULL || event_log_failed || fflush(event_log) != 0) {
        remember_log_error();
        return -1;
    }
    return 0;
}

static int write_owned_mode(const char *sequence, bool *owned) {
    // Mark ownership before writing so a short/failed write still schedules a
    // matching reset for any complete prefix the terminal may have accepted.
    *owned = true;
    return write_all(STDOUT_FILENO, sequence, strlen(sequence));
}

static int disable_owned_mode(const char *sequence, bool *owned) {
    if (!*owned) {
        return 0;
    }
    *owned = false;
    return write_all(STDOUT_FILENO, sequence, strlen(sequence));
}

static int cleanup(void) {
    if (cleanup_complete) {
        return cleanup_outcome;
    }
    int cleanup_failed = 0;
    cleanup_failed |= disable_owned_mode("\033[<u", &kitty_keyboard_mode_owned) != 0;
    cleanup_failed |= disable_owned_mode("\033[?1016l", &mouse_pixel_mode_owned) != 0;
    cleanup_failed |= disable_owned_mode("\033[?1006l", &mouse_sgr_mode_owned) != 0;
    cleanup_failed |= disable_owned_mode("\033[?1003l", &mouse_any_mode_owned) != 0;
    cleanup_failed |= disable_owned_mode("\033[?1002l", &mouse_button_mode_owned) != 0;
    cleanup_failed |= disable_owned_mode("\033[?1000l", &mouse_normal_mode_owned) != 0;
    cleanup_failed |=
        disable_owned_mode("\033[?2004l", &bracketed_paste_mode_owned) != 0;
    cleanup_failed |= disable_owned_mode("\033[?1004l", &focus_mode_owned) != 0;
    if (terminal_is_raw) {
        if (tcsetattr(STDIN_FILENO, TCSAFLUSH, &original_termios) != 0) {
            cleanup_failed = 1;
        }
        terminal_is_raw = false;
    }
    if (event_log != NULL) {
        uint64_t now = 0;
        if (!event_log_failed && monotonic_nanoseconds(&now) == 0) {
            (void)log_checked("capture-end monotonic_ns=%llu result=%s\n",
                              (unsigned long long)now, capture_result);
        } else if (!event_log_failed) {
            errno = EIO;
            remember_log_error();
        }
        if (!event_log_failed) {
            (void)flush_log_checked();
        }
        if (fclose(event_log) != 0) {
            remember_log_error();
        }
        event_log = NULL;
    }
    if (event_log_failed) {
        fprintf(stderr, "native-probe: event log I/O failed: %s\n",
                strerror(event_log_error));
        cleanup_failed = 1;
    }
    if (cleanup_failed && !event_log_failed) {
        fputs("native-probe: terminal cleanup failed\n", stderr);
    }
    cleanup_complete = true;
    cleanup_outcome = cleanup_failed == 0 ? 0 : -1;
    return cleanup_outcome;
}

static void cleanup_at_exit(void) {
    (void)cleanup();
}

static int read_geometry(struct winsize *window_size) {
    memset(window_size, 0, sizeof(*window_size));
    if (!isatty(STDIN_FILENO)) {
        fputs("native-probe: stdin is not a PTY\n", stderr);
        return -1;
    }
    if (ioctl(STDIN_FILENO, TIOCGWINSZ, window_size) != 0) {
        fprintf(stderr, "native-probe: TIOCGWINSZ failed: %s\n", strerror(errno));
        return -1;
    }
    return 0;
}

static int report_geometry(FILE *stream, bool checked_event_log) {
    struct winsize window_size;
    struct termios attributes;
    if (read_geometry(&window_size) != 0) {
        return -1;
    }
    if (tcgetattr(STDIN_FILENO, &attributes) != 0) {
        fprintf(stderr, "native-probe: tcgetattr failed: %s\n", strerror(errno));
        return -1;
    }
#ifdef IUTF8
    int utf8 = (attributes.c_iflag & IUTF8) != 0;
#else
    int utf8 = -1;
#endif
    int written;
    if (checked_event_log) {
        written = log_checked(
            "geometry rows=%u cols=%u pixel_width=%u pixel_height=%u "
            "utf8=%d pid=%ld process_group=%ld foreground_group=%ld\n",
            window_size.ws_row, window_size.ws_col, window_size.ws_xpixel,
            window_size.ws_ypixel, utf8, (long)getpid(), (long)getpgrp(),
            (long)tcgetpgrp(STDIN_FILENO));
        return written == 0 && flush_log_checked() == 0 ? 0 : -1;
    }
    written = fprintf(stream,
                      "geometry rows=%u cols=%u pixel_width=%u pixel_height=%u "
                      "utf8=%d pid=%ld process_group=%ld foreground_group=%ld\n",
                      window_size.ws_row, window_size.ws_col, window_size.ws_xpixel,
                      window_size.ws_ypixel, utf8, (long)getpid(), (long)getpgrp(),
                      (long)tcgetpgrp(STDIN_FILENO));
    if (written < 0 || fflush(stream) != 0) {
        fprintf(stderr, "native-probe: geometry output failed: %s\n", strerror(errno));
        return -1;
    }
    return 0;
}

static int parse_positive_seconds(const char *value, unsigned int *parsed) {
    char *end = NULL;
    errno = 0;
    unsigned long number = strtoul(value, &end, 10);
    if (errno != 0 || end == value || *end != '\0' || number == 0 || number > 86400) {
        return -1;
    }
    *parsed = (unsigned int)number;
    return 0;
}

static int parse_capture_options(int argument_count, char **arguments,
                                 struct capture_options *options) {
    memset(options, 0, sizeof(*options));
    for (int index = 2; index < argument_count; ++index) {
        const char *argument = arguments[index];
        if (strcmp(argument, "--focus") == 0) {
            options->focus = true;
        } else if (strcmp(argument, "--bracketed-paste") == 0) {
            options->bracketed_paste = true;
        } else if (strcmp(argument, "--kitty-keyboard") == 0) {
            options->kitty_keyboard = true;
        } else if (strcmp(argument, "--pixel-mouse") == 0) {
            options->pixel_mouse = true;
        } else if (strncmp(argument, "--mouse=", 8) == 0) {
            const char *mode = argument + 8;
            if (strcmp(mode, "normal") == 0) {
                options->mouse = MOUSE_NORMAL;
            } else if (strcmp(mode, "button") == 0) {
                options->mouse = MOUSE_BUTTON;
            } else if (strcmp(mode, "any") == 0) {
                options->mouse = MOUSE_ANY;
            } else {
                fprintf(stderr, "native-probe: unsupported mouse mode: %s\n", mode);
                return -1;
            }
        } else if (strncmp(argument, "--timeout=", 10) == 0) {
            if (parse_positive_seconds(argument + 10, &options->timeout_seconds) != 0) {
                fprintf(stderr, "native-probe: invalid timeout: %s\n", argument + 10);
                return -1;
            }
        } else if (strcmp(argument, "--log") == 0) {
            if (++index >= argument_count) {
                fputs("native-probe: --log requires a path\n", stderr);
                return -1;
            }
            options->log_path = arguments[index];
        } else if (strcmp(argument, "--help") == 0 || strcmp(argument, "-h") == 0) {
            usage(stdout);
            exit(0);
        } else {
            fprintf(stderr, "native-probe: unknown capture option: %s\n", argument);
            return -1;
        }
    }
    if (options->log_path == NULL || options->log_path[0] == '\0') {
        fputs("native-probe: capture requires --log PATH\n", stderr);
        return -1;
    }
    if (options->pixel_mouse && options->mouse == MOUSE_OFF) {
        fputs("native-probe: --pixel-mouse requires --mouse=MODE\n", stderr);
        return -1;
    }
    return 0;
}

static int open_event_log(const char *path) {
    int descriptor = open(path, O_WRONLY | O_CREAT | O_EXCL, 0600);
    if (descriptor < 0) {
        fprintf(stderr, "native-probe: cannot create log %s: %s\n", path,
                strerror(errno));
        return -1;
    }
    event_log = fdopen(descriptor, "w");
    if (event_log == NULL) {
        fprintf(stderr, "native-probe: fdopen failed: %s\n", strerror(errno));
        close(descriptor);
        return -1;
    }
    return 0;
}

static int enable_capture_modes(const struct capture_options *options) {
    int failed = 0;
#define ENABLE_REQUESTED(requested, sequence, owned)                                \
    do {                                                                            \
        if ((requested) && write_owned_mode((sequence), &(owned)) != 0) {           \
            failed = 1;                                                             \
            goto finished;                                                          \
        }                                                                           \
    } while (0)

    ENABLE_REQUESTED(options->focus, "\033[?1004h", focus_mode_owned);
    ENABLE_REQUESTED(options->bracketed_paste, "\033[?2004h",
                     bracketed_paste_mode_owned);
    // Flags 1, 2, and 8 disambiguate keys, report event types, and report
    // every key as an escape sequence. Pushing preserves the prior mode.
    ENABLE_REQUESTED(options->kitty_keyboard, "\033[>11u", kitty_keyboard_mode_owned);
    ENABLE_REQUESTED(options->mouse == MOUSE_NORMAL, "\033[?1000h",
                     mouse_normal_mode_owned);
    ENABLE_REQUESTED(options->mouse == MOUSE_BUTTON, "\033[?1002h",
                     mouse_button_mode_owned);
    ENABLE_REQUESTED(options->mouse == MOUSE_ANY, "\033[?1003h", mouse_any_mode_owned);
    ENABLE_REQUESTED(options->mouse != MOUSE_OFF, "\033[?1006h", mouse_sgr_mode_owned);
    ENABLE_REQUESTED(options->pixel_mouse, "\033[?1016h", mouse_pixel_mode_owned);

finished:
#undef ENABLE_REQUESTED
    if (failed) {
        fprintf(stderr, "native-probe: failed to enable terminal modes: %s\n",
                strerror(errno));
        return -1;
    }
    return 0;
}

static int log_marker(const char *name, unsigned long long byte_sequence) {
    uint64_t now = 0;
    if (monotonic_nanoseconds(&now) != 0) {
        errno = EIO;
        remember_log_error();
        return -1;
    }
    return log_checked("marker name=%s ending_byte_sequence=%llu monotonic_ns=%llu\n",
                       name, byte_sequence, (unsigned long long)now);
}

struct marker_state {
    unsigned char candidate[64];
    size_t candidate_length;
    bool inside_bracketed_paste;
};

static int inspect_markers(unsigned char byte, struct marker_state *state,
                           unsigned long long byte_sequence) {
    static const unsigned char focus_in[] = {0x1b, '[', 'I'};
    static const unsigned char focus_out[] = {0x1b, '[', 'O'};
    static const unsigned char paste_begin[] = {0x1b, '[', '2', '0', '0', '~'};
    static const unsigned char paste_end[] = {0x1b, '[', '2', '0', '1', '~'};

    if (byte == 0x1b) {
        state->candidate_length = 0;
    }
    if (state->candidate_length < sizeof(state->candidate) - 1 &&
        (state->candidate_length > 0 || byte == 0x1b)) {
        state->candidate[state->candidate_length++] = byte;
    } else if (state->candidate_length > 0) {
        state->candidate_length = 0;
    }

    if (state->inside_bracketed_paste) {
        if (state->candidate_length == sizeof(paste_end) &&
            memcmp(state->candidate, paste_end, sizeof(paste_end)) == 0) {
            state->inside_bracketed_paste = false;
            state->candidate_length = 0;
            return log_marker("bracketed-paste-end", byte_sequence);
        }
        return 0;
    }
    if (state->candidate_length == sizeof(focus_in) &&
        memcmp(state->candidate, focus_in, sizeof(focus_in)) == 0) {
        state->candidate_length = 0;
        return log_marker("focus-in", byte_sequence);
    }
    if (state->candidate_length == sizeof(focus_out) &&
        memcmp(state->candidate, focus_out, sizeof(focus_out)) == 0) {
        state->candidate_length = 0;
        return log_marker("focus-out", byte_sequence);
    }
    if (state->candidate_length == sizeof(paste_begin) &&
        memcmp(state->candidate, paste_begin, sizeof(paste_begin)) == 0) {
        state->inside_bracketed_paste = true;
        state->candidate_length = 0;
        return log_marker("bracketed-paste-begin", byte_sequence);
    }
    if (state->candidate_length >= 4 && state->candidate[0] == 0x1b &&
        state->candidate[1] == '[' && state->candidate[2] == '<' &&
        (byte == 'M' || byte == 'm')) {
        state->candidate_length = 0;
        return log_marker(byte == 'M' ? "mouse-press-or-motion" : "mouse-release",
                          byte_sequence);
    }
    if (state->candidate_length >= 3 && state->candidate[0] == 0x1b &&
        state->candidate[1] == '[' && byte == 'u') {
        state->candidate_length = 0;
        return log_marker("kitty-keyboard", byte_sequence);
    }
    return 0;
}

static const char *signal_name(int signal_number) {
    switch (signal_number) {
    case SIGINT:
        return "SIGINT";
    case SIGTERM:
        return "SIGTERM";
    case SIGHUP:
        return "SIGHUP";
    case SIGQUIT:
        return "SIGQUIT";
    case SIGTSTP:
        return "SIGTSTP";
    case SIGPIPE:
        return "SIGPIPE";
    default:
        return "UNKNOWN";
    }
}

static int install_signal_handlers(void) {
    struct sigaction action;
    memset(&action, 0, sizeof(action));
    action.sa_handler = request_termination;
    sigemptyset(&action.sa_mask);
    const int handled[] = {SIGINT, SIGTERM, SIGHUP, SIGQUIT, SIGTSTP, SIGPIPE};
    for (size_t index = 0; index < sizeof(handled) / sizeof(handled[0]); ++index) {
        if (sigaction(handled[index], &action, NULL) != 0) {
            fprintf(stderr, "native-probe: cannot install handler for signal %d: %s\n",
                    handled[index], strerror(errno));
            return -1;
        }
    }
#ifdef SIGXFSZ
    action.sa_handler = SIG_IGN;
    if (sigaction(SIGXFSZ, &action, NULL) != 0) {
        fprintf(stderr, "native-probe: cannot ignore SIGXFSZ: %s\n", strerror(errno));
        return -1;
    }
#endif
    return 0;
}

static int block_capture_signals(sigset_t *prior_mask) {
    sigset_t blocked;
    sigemptyset(&blocked);
    const int handled[] = {SIGINT, SIGTERM, SIGHUP, SIGQUIT, SIGTSTP, SIGPIPE};
    for (size_t index = 0; index < sizeof(handled) / sizeof(handled[0]); ++index) {
        sigaddset(&blocked, handled[index]);
    }
    if (sigprocmask(SIG_BLOCK, &blocked, prior_mask) != 0) {
        fprintf(stderr, "native-probe: cannot block capture signals: %s\n",
                strerror(errno));
        return -1;
    }
    return 0;
}

static int restore_signal_mask(const sigset_t *prior_mask) {
    if (sigprocmask(SIG_SETMASK, prior_mask, NULL) != 0) {
        fprintf(stderr, "native-probe: cannot restore capture signal mask: %s\n",
                strerror(errno));
        return -1;
    }
    return 0;
}

static int log_termination_signal(unsigned long long byte_sequence) {
    int signal_number = termination_signal;
    capture_result = "signal";
    uint64_t now = 0;
    if (monotonic_nanoseconds(&now) != 0) {
        errno = EIO;
        remember_log_error();
        return -1;
    }
    return log_checked(
        "termination signal=%d name=%s ending_byte_sequence=%llu monotonic_ns=%llu\n",
        signal_number, signal_name(signal_number), byte_sequence,
        (unsigned long long)now);
}

static int capture(const struct capture_options *options) {
    struct winsize window_size;
    if (install_signal_handlers() != 0 || read_geometry(&window_size) != 0 ||
        open_event_log(options->log_path) != 0) {
        return -1;
    }
    uint64_t started = 0;
    if (monotonic_nanoseconds(&started) != 0) {
        errno = EIO;
        remember_log_error();
        return -1;
    }
    capture_result = "running";
    if (log_checked(
            "capture-start monotonic_ns=%llu timestamp_semantics=post-read-log-time "
            "focus=%d bracketed_paste=%d kitty_keyboard=%d mouse=%d "
            "pixel_mouse=%d timeout_seconds=%u pid=%ld\n",
            (unsigned long long)started, options->focus, options->bracketed_paste,
            options->kitty_keyboard, options->mouse, options->pixel_mouse,
            options->timeout_seconds, (long)getpid()) != 0) {
        return -1;
    }
    if (report_geometry(event_log, true) != 0) {
        return -1;
    }
    if (flush_log_checked() != 0) {
        return -1;
    }
    if (termination_signal != 0) {
        return log_termination_signal(0) == 0 ? 1 : -1;
    }

    sigset_t prior_signal_mask;
    if (block_capture_signals(&prior_signal_mask) != 0) {
        return -1;
    }
    if (termination_signal != 0) {
        if (restore_signal_mask(&prior_signal_mask) != 0) {
            return -1;
        }
        return log_termination_signal(0) == 0 ? 1 : -1;
    }
    int transition_failed = 0;
    if (tcgetattr(STDIN_FILENO, &original_termios) != 0) {
        fprintf(stderr, "native-probe: tcgetattr failed: %s\n", strerror(errno));
        transition_failed = 1;
    } else {
        struct termios raw = original_termios;
        cfmakeraw(&raw);
        if (tcsetattr(STDIN_FILENO, TCSAFLUSH, &raw) != 0) {
            fprintf(stderr, "native-probe: cannot enter raw mode: %s\n", strerror(errno));
            transition_failed = 1;
        } else {
            terminal_is_raw = true;
            if (enable_capture_modes(options) != 0) {
                transition_failed = 1;
            }
        }
    }
    if (restore_signal_mask(&prior_signal_mask) != 0) {
        return -1;
    }
    if (termination_signal != 0) {
        return log_termination_signal(0) == 0 ? 1 : -1;
    }
    if (transition_failed) {
        return -1;
    }
    unsigned long long byte_sequence = 0;
    struct marker_state marker_state = {0};
    while (termination_signal == 0) {
        uint64_t now = 0;
        if (monotonic_nanoseconds(&now) != 0) {
            errno = EIO;
            remember_log_error();
            return -1;
        }
        if (options->timeout_seconds > 0 &&
            now - started >= (uint64_t)options->timeout_seconds * UINT64_C(1000000000)) {
            capture_result = "timeout";
            if (log_marker("timeout", byte_sequence) != 0) {
                return -1;
            }
            break;
        }
        struct pollfd input = {.fd = STDIN_FILENO, .events = POLLIN, .revents = 0};
        int ready = poll(&input, 1, 100);
        if (ready < 0) {
            if (errno == EINTR) {
                continue;
            }
            fprintf(stderr, "native-probe: poll failed: %s\n", strerror(errno));
            return -1;
        }
        if (ready == 0) {
            continue;
        }
        unsigned char bytes[4096];
        ssize_t count = read(STDIN_FILENO, bytes, sizeof(bytes));
        if (count < 0) {
            if (errno == EINTR) {
                continue;
            }
            fprintf(stderr, "native-probe: read failed: %s\n", strerror(errno));
            return -1;
        }
        if (count == 0) {
            capture_result = "end-of-file";
            if (log_marker("end-of-file", byte_sequence) != 0) {
                return -1;
            }
            break;
        }
        for (ssize_t index = 0; index < count; ++index) {
            unsigned char byte = bytes[index];
            ++byte_sequence;
            uint64_t logged_at = 0;
            if (monotonic_nanoseconds(&logged_at) != 0) {
                errno = EIO;
                remember_log_error();
                return -1;
            }
            if (log_checked("byte sequence=%llu monotonic_ns=%llu hex=%02x\n",
                            byte_sequence, (unsigned long long)logged_at, byte) != 0 ||
                inspect_markers(byte, &marker_state, byte_sequence) != 0) {
                return -1;
            }
            if (byte == 0x1d) {
                capture_result = "control-right-bracket-stop";
                if (log_marker("control-right-bracket-stop", byte_sequence) != 0) {
                    return -1;
                }
                if (flush_log_checked() != 0) {
                    return -1;
                }
                return 0;
            }
        }
        if (flush_log_checked() != 0) {
            return -1;
        }
    }
    if (termination_signal != 0) {
        return log_termination_signal(byte_sequence) == 0 ? 1 : -1;
    }
    return 0;
}

int main(int argument_count, char **arguments) {
    if (atexit(cleanup_at_exit) != 0) {
        fputs("native-probe: cannot register cleanup\n", stderr);
        return 1;
    }
    if (argument_count < 2 || strcmp(arguments[1], "--help") == 0 ||
        strcmp(arguments[1], "-h") == 0) {
        usage(argument_count < 2 ? stderr : stdout);
        return argument_count < 2 ? 2 : 0;
    }
    if (strcmp(arguments[1], "geometry") == 0) {
        if (argument_count != 2) {
            usage(stderr);
            return 2;
        }
        return report_geometry(stdout, false) == 0 ? 0 : 1;
    }
    if (strcmp(arguments[1], "capture") == 0) {
        struct capture_options options;
        if (parse_capture_options(argument_count, arguments, &options) != 0) {
            usage(stderr);
            return 2;
        }
        int capture_status = capture(&options);
        int signal_number = termination_signal;
        if (capture_status < 0) {
            capture_result = "error";
        }
        int cleanup_status = cleanup();
        if (capture_status < 0 || cleanup_status != 0) {
            return 1;
        }
        if (signal_number != 0) {
            return 128 + signal_number;
        }
        return 0;
    }
    fprintf(stderr, "native-probe: unknown command: %s\n", arguments[1]);
    usage(stderr);
    return 2;
}
