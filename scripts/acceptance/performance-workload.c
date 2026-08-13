#define _DARWIN_C_SOURCE

#include <CommonCrypto/CommonDigest.h>
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <mach/mach_time.h>
#include <mach-o/dyld.h>
#include <poll.h>
#include <signal.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/stat.h>
#include <termios.h>
#include <unistd.h>

#ifndef O_CLOEXEC
#define O_CLOEXEC 0
#endif
#ifndef O_NOFOLLOW
#define O_NOFOLLOW 0
#endif

#define OUTPUT_CHUNK_BYTES 16384U
#define RESIZE_BUFFER_BYTES 65536U
#define MAX_INPUT_FRAME_BYTES 96U
#define MIN_SEED_ROWS UINT64_C(10000)
#define MAX_SEED_ROWS UINT64_C(200000)
#define MAX_DURATION_SECONDS UINT64_C(86400)
#define MAX_ITERATIONS UINT64_C(1000000000)
#define STANDARD_WARMUP_SECONDS UINT64_C(60)

static const unsigned char POST_TERMIOS_SENTINEL[] =
    "\033[0mSPACETERM-PERF-END";
static const char INPUT_PREFIX[] = "SPACETERM-PERF-INPUT ";

static volatile sig_atomic_t termination_signal = 0;
static volatile sig_atomic_t window_size_changed = 0;

enum scenario {
    SCENARIO_ASCII,
    SCENARIO_UNICODE_STYLES,
    SCENARIO_RESIZE_SEED,
    SCENARIO_SCROLLED,
    SCENARIO_HIDDEN_OCCLUDED,
    SCENARIO_RESIZE,
};

struct options {
    enum scenario scenario;
    const char *scenario_name;
    const char *events_path;
    const char *metrics_path;
    uint64_t duration_seconds;
    uint64_t iterations;
    uint64_t seed_rows;
    bool has_duration;
    bool has_iterations;
    bool has_seed_rows;
};

struct geometry {
    unsigned int rows;
    unsigned int columns;
    unsigned int pixel_width;
    unsigned int pixel_height;
};

struct signal_state {
    int numbers[7];
    struct sigaction previous[7];
    size_t count;
    struct sigaction previous_winch;
    bool winch_installed;
};

struct run_state {
    struct options options;
    struct geometry geometry;
    struct termios original_termios;
    struct signal_state signals;
    CC_SHA256_CTX seed_hash;
    int events_fd;
    uint64_t emitted_bytes;
    uint64_t seed_bytes;
    uint64_t input_events;
    uint64_t event_sequence;
    char last_input_event_id[MAX_INPUT_FRAME_BYTES + 1];
    uint64_t started_continuous_ns;
    unsigned char input_frame[MAX_INPUT_FRAME_BYTES];
    size_t input_frame_length;
    bool termios_owned;
    bool event_log_failed;
    bool runtime_failed;
};

static void usage(FILE *stream) {
    fputs(
        "Usage:\n"
        "  performance-workload --scenario NAME --events ABSOLUTE_PATH\n"
        "    --metrics ABSOLUTE_PATH [RUN_LIMIT] [--resize-lines N]\n\n"
        "Scenarios:\n"
        "  ascii             Sustained deterministic printable ASCII.\n"
        "  unicode-styles    Sustained Unicode, ANSI styles, links, and symbols.\n"
        "  resize-seed       Emit only a deterministic seed of at least 10,000 rows.\n"
        "  scrolled          Seed at least 10,000 mixed rows, then sustain output.\n"
        "  hidden-occluded   Sustain deterministic output for hidden/occluded checks.\n"
        "  resize            Seed at least 10,000 mixed rows, then sustain resize output.\n\n"
        "Run limits (all except resize-seed; choose exactly one):\n"
        "  --duration-seconds N   Run for at least N continuous seconds.\n"
        "  --iterations N         Emit exactly N sustained chunks.\n\n"
        "Options:\n"
        "  --resize-lines N       Seed rows for resize-seed, scrolled, or resize.\n"
        "                         Default/minimum: 10000; maximum: 200000.\n\n"
        "Input records must be exactly: SPACETERM-PERF-INPUT <safe-event-id>\\n\n"
        "Input contents are never logged. After restoring captured termios, the\n"
        "producer writes fixed final bytes ESC [ 0 m SPACETERM-PERF-END.\n",
        stream);
}

static void handle_termination(int signal_number) {
    if (termination_signal == 0) {
        termination_signal = signal_number;
    }
}

static void handle_window_size_change(int signal_number) {
    (void)signal_number;
    window_size_changed = 1;
}

static int parse_positive_u64(const char *text, uint64_t maximum,
                              uint64_t *value) {
    char *end = NULL;
    errno = 0;
    unsigned long long parsed = strtoull(text, &end, 10);
    if (errno != 0 || end == text || *end != '\0' || parsed == 0 ||
        parsed > maximum || text[0] == '+') {
        return -1;
    }
    *value = (uint64_t)parsed;
    return 0;
}

static int parse_scenario(const char *name, struct options *options) {
    if (strcmp(name, "ascii") == 0) {
        options->scenario = SCENARIO_ASCII;
    } else if (strcmp(name, "unicode-styles") == 0) {
        options->scenario = SCENARIO_UNICODE_STYLES;
    } else if (strcmp(name, "resize-seed") == 0) {
        options->scenario = SCENARIO_RESIZE_SEED;
    } else if (strcmp(name, "scrolled") == 0) {
        options->scenario = SCENARIO_SCROLLED;
    } else if (strcmp(name, "hidden-occluded") == 0) {
        options->scenario = SCENARIO_HIDDEN_OCCLUDED;
    } else if (strcmp(name, "resize") == 0) {
        options->scenario = SCENARIO_RESIZE;
    } else {
        return -1;
    }
    options->scenario_name = name;
    return 0;
}

static bool path_is_valid(const char *path) {
    return path != NULL && path[0] == '/' && strcmp(path, "/") != 0 &&
           strchr(path, '\n') == NULL && strchr(path, '\t') == NULL;
}

static int target_does_not_exist(const char *path) {
    struct stat metadata;
    if (lstat(path, &metadata) == 0) {
        errno = EEXIST;
        return -1;
    }
    return errno == ENOENT ? 0 : -1;
}

static int parse_options(int argument_count, char **arguments,
                         struct options *options) {
    memset(options, 0, sizeof(*options));
    bool has_scenario = false;
    bool has_events = false;
    bool has_metrics = false;

    for (int index = 1; index < argument_count; ++index) {
        const char *argument = arguments[index];
        if (strcmp(argument, "--scenario") == 0) {
            if (has_scenario || ++index >= argument_count ||
                parse_scenario(arguments[index], options) != 0) {
                return -1;
            }
            has_scenario = true;
        } else if (strcmp(argument, "--events") == 0) {
            if (has_events || ++index >= argument_count) {
                return -1;
            }
            options->events_path = arguments[index];
            has_events = true;
        } else if (strcmp(argument, "--metrics") == 0) {
            if (has_metrics || ++index >= argument_count) {
                return -1;
            }
            options->metrics_path = arguments[index];
            has_metrics = true;
        } else if (strcmp(argument, "--duration-seconds") == 0) {
            if (options->has_duration || ++index >= argument_count ||
                parse_positive_u64(arguments[index], MAX_DURATION_SECONDS,
                                   &options->duration_seconds) != 0) {
                return -1;
            }
            options->has_duration = true;
        } else if (strcmp(argument, "--iterations") == 0) {
            if (options->has_iterations || ++index >= argument_count ||
                parse_positive_u64(arguments[index], MAX_ITERATIONS,
                                   &options->iterations) != 0) {
                return -1;
            }
            options->has_iterations = true;
        } else if (strcmp(argument, "--resize-lines") == 0) {
            if (options->has_seed_rows || ++index >= argument_count ||
                parse_positive_u64(arguments[index], MAX_SEED_ROWS,
                                   &options->seed_rows) != 0) {
                return -1;
            }
            options->has_seed_rows = true;
        } else {
            return -1;
        }
    }

    if (!has_scenario || !has_events || !has_metrics ||
        !path_is_valid(options->events_path) ||
        !path_is_valid(options->metrics_path) ||
        strcmp(options->events_path, options->metrics_path) == 0 ||
        target_does_not_exist(options->events_path) != 0 ||
        target_does_not_exist(options->metrics_path) != 0) {
        return -1;
    }

    bool seeds_rows = options->scenario == SCENARIO_RESIZE_SEED ||
                      options->scenario == SCENARIO_SCROLLED ||
                      options->scenario == SCENARIO_RESIZE;
    if (seeds_rows) {
        if (!options->has_seed_rows) {
            options->seed_rows = MIN_SEED_ROWS;
        }
        if (options->seed_rows < MIN_SEED_ROWS) {
            return -1;
        }
    } else if (options->has_seed_rows) {
        return -1;
    }

    if (options->scenario == SCENARIO_RESIZE_SEED) {
        if (options->has_duration || options->has_iterations) {
            return -1;
        }
    } else if (options->has_duration == options->has_iterations) {
        return -1;
    }
    return 0;
}

static int continuous_nanoseconds(uint64_t *value) {
    static mach_timebase_info_data_t timebase;
    static bool initialized = false;
    if (!initialized) {
        kern_return_t result = mach_timebase_info(&timebase);
        if (result != KERN_SUCCESS || timebase.denom == 0) {
            errno = EIO;
            return -1;
        }
        initialized = true;
    }

    uint64_t ticks = mach_continuous_time();
    uint64_t quotient = ticks / timebase.denom;
    uint64_t remainder = ticks % timebase.denom;
    if (quotient > UINT64_MAX / timebase.numer) {
        errno = EOVERFLOW;
        return -1;
    }
    uint64_t whole = quotient * timebase.numer;
    uint64_t fraction = (remainder * timebase.numer) / timebase.denom;
    if (whole > UINT64_MAX - fraction) {
        errno = EOVERFLOW;
        return -1;
    }
    *value = whole + fraction;
    return 0;
}

static int write_all_fd(int descriptor, const unsigned char *bytes,
                        size_t length) {
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

static int write_output(struct run_state *state, const unsigned char *bytes,
                        size_t length, bool seed_bytes) {
    while (length > 0) {
        ssize_t written = write(STDOUT_FILENO, bytes, length);
        if (written < 0) {
            if (errno == EINTR && termination_signal == 0) {
                continue;
            }
            state->runtime_failed = true;
            return -1;
        }
        if (written == 0) {
            errno = EIO;
            state->runtime_failed = true;
            return -1;
        }
        size_t count = (size_t)written;
        if (state->emitted_bytes > UINT64_MAX - count ||
            (seed_bytes && state->seed_bytes > UINT64_MAX - count)) {
            errno = EOVERFLOW;
            state->runtime_failed = true;
            return -1;
        }
        state->emitted_bytes += count;
        if (seed_bytes) {
            CC_SHA256_Update(&state->seed_hash, bytes, (CC_LONG)count);
            state->seed_bytes += count;
        }
        bytes += count;
        length -= count;
    }
    return 0;
}

static int record_event_at(struct run_state *state, uint64_t timestamp,
                           const char *kind, const char *event_id,
                           uint64_t byte_count, const char *status) {
    char row[384];
    int length = snprintf(
        row, sizeof(row),
        "%" PRIu64 "\t%" PRIu64 "\t%s\t%s\t%" PRIu64
        "\t%u\t%u\t%u\t%u\t%s\n",
        state->event_sequence, timestamp, kind, event_id, byte_count,
        state->geometry.rows,
        state->geometry.columns, state->geometry.pixel_width,
        state->geometry.pixel_height, status);
    if (length < 0 || (size_t)length >= sizeof(row) ||
        write_all_fd(state->events_fd, (const unsigned char *)row,
                     (size_t)length) != 0) {
        state->event_log_failed = true;
        state->runtime_failed = true;
        return -1;
    }
    state->event_sequence += 1;
    return 0;
}

static int record_event(struct run_state *state, const char *kind,
                        const char *event_id, uint64_t byte_count,
                        const char *status, uint64_t *timestamp_out) {
    uint64_t timestamp = 0;
    if (continuous_nanoseconds(&timestamp) != 0 ||
        record_event_at(state, timestamp, kind, event_id, byte_count,
                        status) != 0) {
        state->runtime_failed = true;
        return -1;
    }
    if (timestamp_out != NULL) {
        *timestamp_out = timestamp;
    }
    return 0;
}

static int read_geometry(struct run_state *state) {
    struct winsize size;
    memset(&size, 0, sizeof(size));
    if (ioctl(STDIN_FILENO, TIOCGWINSZ, &size) != 0) {
        state->runtime_failed = true;
        return -1;
    }
    state->geometry.rows = size.ws_row;
    state->geometry.columns = size.ws_col;
    state->geometry.pixel_width = size.ws_xpixel;
    state->geometry.pixel_height = size.ws_ypixel;
    return 0;
}

static int record_geometry(struct run_state *state) {
    if (read_geometry(state) != 0 ||
        record_event(state, "geometry", "none", 0, "ok", NULL) != 0) {
        return -1;
    }
    return 0;
}

static int parse_input_event_id(const unsigned char *frame, size_t length,
                                char event_id[MAX_INPUT_FRAME_BYTES + 1]) {
    size_t prefix_length = sizeof(INPUT_PREFIX) - 1;
    if (length <= prefix_length ||
        length - prefix_length > MAX_INPUT_FRAME_BYTES ||
        memcmp(frame, INPUT_PREFIX, prefix_length) != 0) {
        return -1;
    }
    size_t identifier_length = length - prefix_length;
    for (size_t index = 0; index < identifier_length; ++index) {
        unsigned char byte = frame[prefix_length + index];
        bool alphanumeric = (byte >= 'a' && byte <= 'z') ||
                            (byte >= 'A' && byte <= 'Z') ||
                            (byte >= '0' && byte <= '9');
        if ((!alphanumeric && byte != '.' && byte != '_' && byte != ':' &&
             byte != '-') ||
            (index == 0 && !alphanumeric)) {
            return -1;
        }
    }
    memcpy(event_id, frame + prefix_length, identifier_length);
    event_id[identifier_length] = '\0';
    return 0;
}

static int acknowledge_input_frame(struct run_state *state) {
    char event_id[MAX_INPUT_FRAME_BYTES + 1];
    memset(event_id, 0, sizeof(event_id));
    if (parse_input_event_id(state->input_frame, state->input_frame_length,
                             event_id) != 0 ||
        (state->last_input_event_id[0] != '\0' &&
         strcmp(event_id, state->last_input_event_id) <= 0)) {
        memset(event_id, 0, sizeof(event_id));
        errno = EPROTO;
        state->runtime_failed = true;
        return -1;
    }
    uint64_t frame_bytes = state->input_frame_length + 1;
    if (record_event(state, "input-read", event_id, frame_bytes, "ok",
                     NULL) != 0) {
        memset(event_id, 0, sizeof(event_id));
        return -1;
    }

    char acknowledgement[192];
    int length = snprintf(acknowledgement, sizeof(acknowledgement),
                          "\r\nSPACETERM-PERF-INPUT-ACK %s\r\n",
                          event_id);
    if (length < 0 || (size_t)length >= sizeof(acknowledgement)) {
        memset(event_id, 0, sizeof(event_id));
        errno = EOVERFLOW;
        state->runtime_failed = true;
        return -1;
    }
    if (write_output(state, (const unsigned char *)acknowledgement,
                     (size_t)length, false) != 0 ||
        record_event(state, "input-ack-written", event_id, (uint64_t)length,
                     "ok", NULL) != 0) {
        memset(event_id, 0, sizeof(event_id));
        return -1;
    }
    memcpy(state->last_input_event_id, event_id, strlen(event_id) + 1);
    memset(event_id, 0, sizeof(event_id));
    state->input_events += 1;
    state->input_frame_length = 0;
    return 0;
}

static int consume_input_byte(struct run_state *state, unsigned char byte) {
    if (byte == '\n') {
        if (state->input_frame_length == 0) {
            errno = EPROTO;
            state->runtime_failed = true;
            return -1;
        }
        return acknowledge_input_frame(state);
    }
    if (state->input_frame_length >= sizeof(state->input_frame)) {
        memset(state->input_frame, 0, sizeof(state->input_frame));
        state->input_frame_length = 0;
        errno = EOVERFLOW;
        state->runtime_failed = true;
        return -1;
    }
    state->input_frame[state->input_frame_length++] = byte;
    return 0;
}

static int consume_available_input(struct run_state *state) {
    struct pollfd descriptor = {
        .fd = STDIN_FILENO,
        .events = POLLIN,
        .revents = 0,
    };
    int ready;
    do {
        ready = poll(&descriptor, 1, 0);
    } while (ready < 0 && errno == EINTR && termination_signal == 0 &&
             window_size_changed == 0);
    if (ready < 0) {
        if (errno == EINTR) {
            return 0;
        }
        state->runtime_failed = true;
        return -1;
    }
    if (ready == 0) {
        return 0;
    }
    if ((descriptor.revents & (POLLERR | POLLNVAL)) != 0 ||
        ((descriptor.revents & POLLHUP) != 0 &&
         (descriptor.revents & POLLIN) == 0)) {
        errno = EIO;
        state->runtime_failed = true;
        return -1;
    }
    if ((descriptor.revents & POLLIN) == 0) {
        return 0;
    }

    unsigned char input[256];
    ssize_t count = read(STDIN_FILENO, input, sizeof(input));
    if (count < 0) {
        if (errno == EINTR || errno == EAGAIN || errno == EWOULDBLOCK) {
            return 0;
        }
        state->runtime_failed = true;
        return -1;
    }
    for (ssize_t index = 0; index < count; ++index) {
        if (consume_input_byte(state, input[index]) != 0) {
            memset(input, 0, sizeof(input));
            return -1;
        }
    }
    memset(input, 0, sizeof(input));
    return 0;
}

static int process_controls(struct run_state *state) {
    if (termination_signal != 0) {
        return -1;
    }
    if (window_size_changed != 0) {
        window_size_changed = 0;
        if (record_geometry(state) != 0) {
            return -1;
        }
    }
    if (consume_available_input(state) != 0) {
        return -1;
    }
    return termination_signal == 0 ? 0 : -1;
}

static void fill_repeated(unsigned char *output, size_t output_length,
                          const unsigned char *pattern,
                          size_t pattern_length) {
    size_t offset = 0;
    while (offset < output_length) {
        size_t remaining = output_length - offset;
        size_t copied = pattern_length < remaining ? pattern_length : remaining;
        memcpy(output + offset, pattern, copied);
        offset += copied;
    }
}

static int emit_resize_seed(struct run_state *state) {
    unsigned char buffer[RESIZE_BUFFER_BYTES];
    size_t buffered = 0;
    static const char long_text[] =
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    for (uint64_t row = 0; row < state->options.seed_rows; ++row) {
        char line[512];
        int length;
        switch (row % 5) {
        case 0:
            length =
                snprintf(line, sizeof(line), "short-%05" PRIu64 "\r\n", row);
            break;
        case 1:
            length = snprintf(line, sizeof(line),
                              "soft-wrap-%05" PRIu64 " %s\r\n", row,
                              long_text);
            break;
        case 2:
            length = snprintf(line, sizeof(line),
                              "\033[1;38;5;33;48;5;235mstyled-%05" PRIu64
                              "\033[0m\r\n",
                              row);
            break;
        case 3:
            length = snprintf(line, sizeof(line), "\r\n");
            break;
        default:
            length = snprintf(
                line, sizeof(line),
                "wide-%05" PRIu64
                " \347\225\214\347\225\214 \360\237\230\200 e\314\201 "
                "\342\224\200\342\224\202\342\224\214\342\224\230\r\n",
                row);
            break;
        }
        if (length < 0 || (size_t)length >= sizeof(line)) {
            errno = EOVERFLOW;
            state->runtime_failed = true;
            return -1;
        }
        if (buffered + (size_t)length > sizeof(buffer)) {
            if (write_output(state, buffer, buffered, true) != 0 ||
                process_controls(state) != 0) {
                return -1;
            }
            buffered = 0;
        }
        memcpy(buffer + buffered, line, (size_t)length);
        buffered += (size_t)length;
    }
    if (buffered > 0 && write_output(state, buffer, buffered, true) != 0) {
        return -1;
    }
    return process_controls(state);
}

static int emit_initial_seed(struct run_state *state,
                             const unsigned char *continuous_chunk,
                             size_t chunk_length) {
    if (state->options.scenario == SCENARIO_RESIZE_SEED ||
        state->options.scenario == SCENARIO_SCROLLED ||
        state->options.scenario == SCENARIO_RESIZE) {
        return emit_resize_seed(state);
    }
    return write_output(state, continuous_chunk, chunk_length, true);
}

static int emit_sustained_output(struct run_state *state,
                                 const unsigned char *chunk,
                                 size_t chunk_length) {
    if (state->options.scenario == SCENARIO_RESIZE_SEED) {
        return 0;
    }
    if (state->options.has_iterations) {
        for (uint64_t iteration = 0; iteration < state->options.iterations;
             ++iteration) {
            if (write_output(state, chunk, chunk_length, false) != 0 ||
                process_controls(state) != 0) {
                return -1;
            }
        }
        return 0;
    }

    uint64_t started = 0;
    if (continuous_nanoseconds(&started) != 0) {
        state->runtime_failed = true;
        return -1;
    }
    uint64_t duration_ns =
        state->options.duration_seconds * UINT64_C(1000000000);
    if (started > UINT64_MAX - duration_ns) {
        errno = EOVERFLOW;
        state->runtime_failed = true;
        return -1;
    }
    uint64_t deadline = started + duration_ns;
    for (;;) {
        uint64_t now = 0;
        if (continuous_nanoseconds(&now) != 0) {
            state->runtime_failed = true;
            return -1;
        }
        if (now >= deadline) {
            break;
        }
        if (write_output(state, chunk, chunk_length, false) != 0 ||
            process_controls(state) != 0) {
            return -1;
        }
    }
    return 0;
}

static uint64_t scenario_warmup_seconds(const struct run_state *state) {
    if (state->options.scenario == SCENARIO_RESIZE ||
        state->options.scenario == SCENARIO_RESIZE_SEED) {
        return 0;
    }
    return STANDARD_WARMUP_SECONDS;
}

static int emit_warmup_output(struct run_state *state,
                              const unsigned char *chunk,
                              size_t chunk_length) {
    uint64_t warmup_seconds = scenario_warmup_seconds(state);
    if (warmup_seconds == 0 || state->options.has_iterations) {
        return 0;
    }
    uint64_t measured_duration = state->options.duration_seconds;
    state->options.duration_seconds = warmup_seconds;
    int result = emit_sustained_output(state, chunk, chunk_length);
    state->options.duration_seconds = measured_duration;
    return result;
}

static int install_signal_handlers(struct run_state *state) {
    int numbers[] = {SIGHUP, SIGINT, SIGQUIT, SIGTERM, SIGPIPE, SIGTSTP};
    struct sigaction action;
    memset(&action, 0, sizeof(action));
    action.sa_handler = handle_termination;
    if (sigemptyset(&action.sa_mask) != 0) {
        return -1;
    }
    for (size_t index = 0; index < sizeof(numbers) / sizeof(numbers[0]);
         ++index) {
        int number = numbers[index];
        if (sigaction(number, &action,
                      &state->signals.previous[state->signals.count]) != 0) {
            return -1;
        }
        state->signals.numbers[state->signals.count++] = number;
    }

    struct sigaction winch_action;
    memset(&winch_action, 0, sizeof(winch_action));
    winch_action.sa_handler = handle_window_size_change;
    if (sigemptyset(&winch_action.sa_mask) != 0 ||
        sigaction(SIGWINCH, &winch_action,
                  &state->signals.previous_winch) != 0) {
        return -1;
    }
    state->signals.winch_installed = true;
    return 0;
}

static void restore_signal_handlers(struct run_state *state) {
    if (state->signals.winch_installed) {
        (void)sigaction(SIGWINCH, &state->signals.previous_winch, NULL);
        state->signals.winch_installed = false;
    }
    while (state->signals.count > 0) {
        state->signals.count -= 1;
        (void)sigaction(state->signals.numbers[state->signals.count],
                        &state->signals.previous[state->signals.count], NULL);
    }
}

static int take_termios_ownership(struct run_state *state) {
    if (!isatty(STDIN_FILENO) ||
        tcgetattr(STDIN_FILENO, &state->original_termios) != 0) {
        return -1;
    }
    struct termios raw = state->original_termios;
    raw.c_lflag &= (tcflag_t)~(ICANON | ECHO | ECHONL | IEXTEN);
    raw.c_oflag &= (tcflag_t)~OPOST;
    raw.c_cc[VMIN] = 0;
    raw.c_cc[VTIME] = 0;
    if (tcsetattr(STDIN_FILENO, TCSANOW, &raw) != 0) {
        return -1;
    }
    state->termios_owned = true;
    return 0;
}

static int restore_termios(struct run_state *state) {
    if (!state->termios_owned) {
        return 0;
    }
    if (tcsetattr(STDIN_FILENO, TCSANOW, &state->original_termios) != 0) {
        state->runtime_failed = true;
        return -1;
    }
    state->termios_owned = false;
    return 0;
}

static int open_event_log(struct run_state *state) {
    mode_t previous_umask = umask(077);
    int descriptor = open(state->options.events_path,
                          O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC | O_NOFOLLOW,
                          0600);
    int saved_errno = errno;
    umask(previous_umask);
    errno = saved_errno;
    if (descriptor < 0) {
        return -1;
    }
    static const unsigned char header[] =
        "sequence\tcontinuous_ns\tkind\tevent_id\tbyte_count\trows\tcolumns\t"
        "pixel_width\tpixel_height\tstatus\n";
    if (write_all_fd(descriptor, header, sizeof(header) - 1) != 0) {
        saved_errno = errno;
        close(descriptor);
        unlink(state->options.events_path);
        errno = saved_errno;
        return -1;
    }
    state->events_fd = descriptor;
    return 0;
}

static int sha256_executable(
    unsigned char digest[CC_SHA256_DIGEST_LENGTH]) {
    uint32_t capacity = 1024;
    char *path = malloc(capacity);
    if (path == NULL) {
        return -1;
    }
    if (_NSGetExecutablePath(path, &capacity) != 0) {
        char *larger = realloc(path, capacity);
        if (larger == NULL) {
            free(path);
            return -1;
        }
        path = larger;
        if (_NSGetExecutablePath(path, &capacity) != 0) {
            free(path);
            errno = EIO;
            return -1;
        }
    }
    int descriptor = open(path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    int saved_errno = errno;
    free(path);
    errno = saved_errno;
    if (descriptor < 0) {
        return -1;
    }

    CC_SHA256_CTX hash;
    CC_SHA256_Init(&hash);
    unsigned char buffer[65536];
    for (;;) {
        ssize_t count = read(descriptor, buffer, sizeof(buffer));
        if (count < 0 && errno == EINTR) {
            continue;
        }
        if (count < 0) {
            saved_errno = errno;
            close(descriptor);
            errno = saved_errno;
            return -1;
        }
        if (count == 0) {
            break;
        }
        CC_SHA256_Update(&hash, buffer, (CC_LONG)count);
    }
    if (close(descriptor) != 0) {
        return -1;
    }
    CC_SHA256_Final(digest, &hash);
    return 0;
}

static void hex_digest(
    const unsigned char digest[CC_SHA256_DIGEST_LENGTH],
    char output[CC_SHA256_DIGEST_LENGTH * 2 + 1]) {
    static const char digits[] = "0123456789abcdef";
    for (size_t index = 0; index < CC_SHA256_DIGEST_LENGTH; ++index) {
        output[index * 2] = digits[digest[index] >> 4];
        output[index * 2 + 1] = digits[digest[index] & 0x0f];
    }
    output[CC_SHA256_DIGEST_LENGTH * 2] = '\0';
}

static int publish_private_metrics(
    const char *target,
    const unsigned char producer_digest[CC_SHA256_DIGEST_LENGTH],
    const unsigned char seed_digest[CC_SHA256_DIGEST_LENGTH],
    const struct run_state *state, uint64_t ended_continuous_ns,
    const char *status) {
    char producer_hex[CC_SHA256_DIGEST_LENGTH * 2 + 1];
    char seed_hex[CC_SHA256_DIGEST_LENGTH * 2 + 1];
    hex_digest(producer_digest, producer_hex);
    hex_digest(seed_digest, seed_hex);

    char contents[1536];
    uint64_t requested_duration_ms =
        state->options.has_duration
            ? state->options.duration_seconds * UINT64_C(1000)
            : 0;
    uint64_t warmup_ms =
        state->options.has_duration
            ? scenario_warmup_seconds(state) * UINT64_C(1000)
            : 0;
    uint64_t requested_iterations =
        state->options.has_iterations ? state->options.iterations : 0;
    bool row_seed = state->options.scenario == SCENARIO_RESIZE_SEED ||
                    state->options.scenario == SCENARIO_SCROLLED ||
                    state->options.scenario == SCENARIO_RESIZE;
    uint64_t requested_seed_rows = row_seed ? state->options.seed_rows : 0;
    int length = snprintf(
        contents, sizeof(contents),
        "format_version\t2\n"
        "scenario\t%s\n"
        "producer_sha256\t%s\n"
        "seed_sha256\t%s\n"
        "seed_bytes\t%" PRIu64 "\n"
        "requested_duration_ms\t%" PRIu64 "\n"
        "warmup_ms\t%" PRIu64 "\n"
        "requested_iterations\t%" PRIu64 "\n"
        "requested_seed_rows\t%" PRIu64 "\n"
        "emitted_bytes\t%" PRIu64 "\n"
        "input_events\t%" PRIu64 "\n"
        "started_continuous_ns\t%" PRIu64 "\n"
        "ended_continuous_ns\t%" PRIu64 "\n"
        "status\t%s\n",
        state->options.scenario_name, producer_hex, seed_hex,
        state->seed_bytes, requested_duration_ms, warmup_ms,
        requested_iterations,
        requested_seed_rows, state->emitted_bytes, state->input_events,
        state->started_continuous_ns, ended_continuous_ns, status);
    if (length < 0 || (size_t)length >= sizeof(contents)) {
        errno = EOVERFLOW;
        return -1;
    }

    size_t target_length = strlen(target);
    if (target_length > SIZE_MAX - 64) {
        errno = ENAMETOOLONG;
        return -1;
    }
    char *temporary = malloc(target_length + 64);
    if (temporary == NULL) {
        return -1;
    }
    int descriptor = -1;
    mode_t previous_umask = umask(077);
    for (unsigned int attempt = 0; attempt < 100; ++attempt) {
        int path_length =
            snprintf(temporary, target_length + 64, "%s.tmp.%ld.%u", target,
                     (long)getpid(), attempt);
        if (path_length < 0 ||
            (size_t)path_length >= target_length + 64) {
            errno = ENAMETOOLONG;
            break;
        }
        descriptor =
            open(temporary,
                 O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC | O_NOFOLLOW, 0600);
        if (descriptor >= 0 || errno != EEXIST) {
            break;
        }
    }
    int saved_errno = errno;
    umask(previous_umask);
    errno = saved_errno;
    if (descriptor < 0) {
        free(temporary);
        return -1;
    }

    int result = 0;
    if (write_all_fd(descriptor, (const unsigned char *)contents,
                     (size_t)length) != 0 ||
        fsync(descriptor) != 0 || close(descriptor) != 0) {
        result = -1;
    } else if (link(temporary, target) != 0) {
        result = -1;
    }
    saved_errno = errno;
    if (result != 0) {
        (void)close(descriptor);
    }
    (void)unlink(temporary);
    free(temporary);
    errno = saved_errno;
    return result;
}

int main(int argument_count, char **arguments) {
    if (argument_count == 2 &&
        (strcmp(arguments[1], "-h") == 0 ||
         strcmp(arguments[1], "--help") == 0)) {
        usage(stdout);
        return 0;
    }

    struct run_state state;
    memset(&state, 0, sizeof(state));
    state.events_fd = -1;
    if (parse_options(argument_count, arguments, &state.options) != 0) {
        usage(stderr);
        fputs("performance-workload: invalid or unsafe arguments\n", stderr);
        return 2;
    }

    unsigned char producer_digest[CC_SHA256_DIGEST_LENGTH];
    if (sha256_executable(producer_digest) != 0) {
        fputs("performance-workload: cannot hash the exact producer executable\n",
              stderr);
        return 1;
    }
    if (open_event_log(&state) != 0) {
        fputs("performance-workload: cannot create the private event file\n",
              stderr);
        return 1;
    }

    static const unsigned char ascii_pattern[] =
        "ASCII 0123456789 abcdefghijklmnopqrstuvwxyz "
        "ABCDEFGHIJKLMNOPQRSTUVWXYZ !@#$%^&*()[]{}\r\n";
    static const unsigned char unicode_pattern[] =
        "\033[1;3;38;2;120;180;255mSTYLE\033[0m "
        "combining=e\314\201 wide=\347\225\214 "
        "emoji=\360\237\221\251\342\200\215\360\237\222\273 "
        "\033[4:3;58;2;255;120;180mcurly\033[0m "
        "\033[9;53mstrike-overline\033[0m \033[5mblink\033[0m "
        "\033]8;;https://example.test/spaceterm/performance\033\\link"
        "\033]8;;\033\\ "
        "draw=\342\224\200\342\224\202\342\224\214\342\224\230 "
        "block=\342\226\210 braille=\342\240\277 powerline=\356\202\260\r\n";
    unsigned char ascii_chunk[OUTPUT_CHUNK_BYTES];
    unsigned char unicode_chunk[OUTPUT_CHUNK_BYTES];
    fill_repeated(ascii_chunk, sizeof(ascii_chunk), ascii_pattern,
                  sizeof(ascii_pattern) - 1);
    fill_repeated(unicode_chunk, sizeof(unicode_chunk), unicode_pattern,
                  sizeof(unicode_pattern) - 1);
    const unsigned char *continuous_chunk = ascii_chunk;
    if (state.options.scenario == SCENARIO_UNICODE_STYLES ||
        state.options.scenario == SCENARIO_RESIZE) {
        continuous_chunk = unicode_chunk;
    }

    CC_SHA256_Init(&state.seed_hash);
    uint64_t producer_started_continuous_ns = 0;
    bool setup_ok =
        install_signal_handlers(&state) == 0 &&
        take_termios_ownership(&state) == 0 &&
        read_geometry(&state) == 0 &&
        record_event(&state, "started", "none", 0, "ok",
                     &producer_started_continuous_ns) == 0 &&
        record_event(&state, "geometry", "none", 0, "ok", NULL) == 0;
    bool completed = false;
    if (setup_ok &&
        emit_initial_seed(&state, continuous_chunk, OUTPUT_CHUNK_BYTES) == 0 &&
        record_event(&state, "seed-complete", "none", state.seed_bytes, "ok",
                     NULL) == 0 &&
        emit_warmup_output(&state, continuous_chunk, OUTPUT_CHUNK_BYTES) == 0 &&
        continuous_nanoseconds(&state.started_continuous_ns) == 0 &&
        state.started_continuous_ns > producer_started_continuous_ns &&
        emit_sustained_output(&state, continuous_chunk, OUTPUT_CHUNK_BYTES) ==
            0 &&
        process_controls(&state) == 0 && state.input_frame_length == 0) {
        completed = true;
    } else if (state.input_frame_length != 0) {
        memset(state.input_frame, 0, sizeof(state.input_frame));
        state.input_frame_length = 0;
        errno = EPROTO;
        state.runtime_failed = true;
    }

    int observed_signal = termination_signal;
    if (!completed && observed_signal == 0 && !state.event_log_failed) {
        fputs("performance-workload: workload or evidence I/O failed\n",
              stderr);
    }

    sigset_t all_signals;
    (void)sigfillset(&all_signals);
    (void)sigprocmask(SIG_BLOCK, &all_signals, NULL);
    restore_signal_handlers(&state);
    if (restore_termios(&state) != 0) {
        completed = false;
    }

    unsigned char seed_digest[CC_SHA256_DIGEST_LENGTH];
    CC_SHA256_Final(seed_digest, &state.seed_hash);

    bool sentinel_written = false;
    if (!state.termios_owned && !state.event_log_failed &&
        write_output(&state, POST_TERMIOS_SENTINEL,
                     sizeof(POST_TERMIOS_SENTINEL) - 1, false) == 0) {
        sentinel_written = true;
    } else {
        completed = false;
    }

    const char *event_status =
        completed && sentinel_written
            ? "success"
            : (observed_signal != 0 ? "signal" : "failure");
    const char *metrics_status =
        completed && sentinel_written ? "complete" : "incomplete";
    uint64_t ended_continuous_ns = 0;
    bool final_evidence_ok =
        continuous_nanoseconds(&ended_continuous_ns) == 0 &&
        record_event_at(&state, ended_continuous_ns, "producer-end", "none",
                        state.emitted_bytes, event_status) == 0 &&
        fsync(state.events_fd) == 0 && close(state.events_fd) == 0;
    state.events_fd = -1;
    if (final_evidence_ok) {
        final_evidence_ok =
            publish_private_metrics(state.options.metrics_path,
                                    producer_digest, seed_digest, &state,
                                    ended_continuous_ns,
                                    metrics_status) == 0;
    }

    if (!final_evidence_ok || !completed || !sentinel_written) {
        return observed_signal != 0 ? 128 + observed_signal : 1;
    }
    return 0;
}
