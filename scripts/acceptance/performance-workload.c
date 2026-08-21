#define _DARWIN_C_SOURCE

#include <CommonCrypto/CommonDigest.h>
#include <CommonCrypto/CommonHMAC.h>
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
#include <time.h>
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
#define MAX_IDENTITY_BYTES (64U * 1024U)
#define MAX_SECRET_BYTES 4096U
#define MAX_BINDING_FIELD_BYTES 4096U
#define MIN_SEED_ROWS UINT64_C(10000)
#define MAX_SEED_ROWS UINT64_C(200000)
#define MAX_DURATION_SECONDS UINT64_C(86400)
#define MAX_ITERATIONS UINT64_C(1000000000)
#define STANDARD_WARMUP_SECONDS UINT64_C(60)
#define PROGRESS_INTERVAL_NS UINT64_C(1000000000)
#define MAX_PROGRESS_LATENESS_NS UINT64_C(1000000000)
#define MAX_PROGRESS_EVENTS UINT64_C(86403)
#define MAX_START_GATE_WAIT_NS UINT64_C(120000000000)
#define MIN_START_GATE_LEAD_NS UINT64_C(2000000000)
#define MAX_START_GATE_LEAD_NS UINT64_C(30000000000)
#define MAX_START_LATENESS_NS UINT64_C(100000000)

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
    const char *subject_identity_path;
    const char *campaign_secret_path;
    const char *plan_start_gate_path;
    const char *ready_receipt_path;
    const char *campaign_id;
    const char *session_id;
    const char *nonce;
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
    uint64_t last_event_continuous_ns;
    uint64_t next_progress_deadline_ns;
    uint64_t progress_events;
    uint64_t last_progress_bytes;
    uint64_t producer_started_continuous_ns;
    pid_t producer_pid;
    pid_t producer_session_id;
    pid_t producer_process_group;
    uint64_t tty_device;
    uint64_t tty_inode;
    uint64_t tty_rdev;
    uint64_t subject_process_pid;
    char subject_process_start_identity[MAX_BINDING_FIELD_BYTES + 1];
    unsigned char subject_identity_digest[CC_SHA256_DIGEST_LENGTH];
    unsigned char campaign_secret[MAX_SECRET_BYTES];
    size_t campaign_secret_length;
    unsigned char ready_receipt_digest[CC_SHA256_DIGEST_LENGTH];
    char last_input_event_id[MAX_INPUT_FRAME_BYTES + 1];
    uint64_t started_continuous_ns;
    uint64_t plan_start_continuous_ns;
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
        "    --metrics ABSOLUTE_PATH --campaign-id ID --session-id ID\n"
        "    --nonce 64_LOWER_HEX --subject-identity ABSOLUTE_PATH\n"
        "    --campaign-secret-file ABSOLUTE_PATH\n"
        "    --ready-receipt ABSENT_ABSOLUTE_PATH\n"
        "    --plan-start-gate ABSENT_ABSOLUTE_PATH [RUN_LIMIT]\n"
        "    [--resize-lines N]\n\n"
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
        "The owner-private subject identity and campaign secret authenticate\n"
        "content-free one-second cumulative progress events.\n\n"
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

static bool safe_label(const char *value) {
    if (value == NULL) {
        return false;
    }
    size_t length = strlen(value);
    if (length == 0 || length > 80) {
        return false;
    }
    for (size_t index = 0; index < length; ++index) {
        unsigned char byte = (unsigned char)value[index];
        bool alphanumeric = (byte >= 'a' && byte <= 'z') ||
                            (byte >= 'A' && byte <= 'Z') ||
                            (byte >= '0' && byte <= '9');
        if ((!alphanumeric && byte != '.' && byte != '_' && byte != '-') ||
            (index == 0 && !alphanumeric)) {
            return false;
        }
    }
    return true;
}

static bool lower_hex_64(const char *value) {
    if (value == NULL || strlen(value) != CC_SHA256_DIGEST_LENGTH * 2) {
        return false;
    }
    for (size_t index = 0; index < CC_SHA256_DIGEST_LENGTH * 2; ++index) {
        unsigned char byte = (unsigned char)value[index];
        if (!((byte >= '0' && byte <= '9') ||
              (byte >= 'a' && byte <= 'f'))) {
            return false;
        }
    }
    return true;
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
    bool has_subject_identity = false;
    bool has_campaign_secret = false;
    bool has_plan_start_gate = false;
    bool has_ready_receipt = false;
    bool has_campaign_id = false;
    bool has_session_id = false;
    bool has_nonce = false;

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
        } else if (strcmp(argument, "--subject-identity") == 0) {
            if (has_subject_identity || ++index >= argument_count) {
                return -1;
            }
            options->subject_identity_path = arguments[index];
            has_subject_identity = true;
        } else if (strcmp(argument, "--campaign-secret-file") == 0) {
            if (has_campaign_secret || ++index >= argument_count) {
                return -1;
            }
            options->campaign_secret_path = arguments[index];
            has_campaign_secret = true;
        } else if (strcmp(argument, "--plan-start-gate") == 0) {
            if (has_plan_start_gate || ++index >= argument_count) {
                return -1;
            }
            options->plan_start_gate_path = arguments[index];
            has_plan_start_gate = true;
        } else if (strcmp(argument, "--ready-receipt") == 0) {
            if (has_ready_receipt || ++index >= argument_count) {
                return -1;
            }
            options->ready_receipt_path = arguments[index];
            has_ready_receipt = true;
        } else if (strcmp(argument, "--campaign-id") == 0) {
            if (has_campaign_id || ++index >= argument_count) {
                return -1;
            }
            options->campaign_id = arguments[index];
            has_campaign_id = true;
        } else if (strcmp(argument, "--session-id") == 0) {
            if (has_session_id || ++index >= argument_count) {
                return -1;
            }
            options->session_id = arguments[index];
            has_session_id = true;
        } else if (strcmp(argument, "--nonce") == 0) {
            if (has_nonce || ++index >= argument_count) {
                return -1;
            }
            options->nonce = arguments[index];
            has_nonce = true;
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
        !has_subject_identity || !has_campaign_secret ||
        !has_plan_start_gate || !has_ready_receipt || !has_campaign_id ||
        !has_session_id || !has_nonce ||
        !path_is_valid(options->events_path) ||
        !path_is_valid(options->metrics_path) ||
        !path_is_valid(options->subject_identity_path) ||
        !path_is_valid(options->campaign_secret_path) ||
        !path_is_valid(options->plan_start_gate_path) ||
        !path_is_valid(options->ready_receipt_path) ||
        !safe_label(options->campaign_id) || !safe_label(options->session_id) ||
        !lower_hex_64(options->nonce) ||
        strcmp(options->events_path, options->metrics_path) == 0 ||
        strcmp(options->events_path, options->subject_identity_path) == 0 ||
        strcmp(options->events_path, options->campaign_secret_path) == 0 ||
        strcmp(options->metrics_path, options->subject_identity_path) == 0 ||
        strcmp(options->metrics_path, options->campaign_secret_path) == 0 ||
        strcmp(options->events_path, options->plan_start_gate_path) == 0 ||
        strcmp(options->events_path, options->ready_receipt_path) == 0 ||
        strcmp(options->metrics_path, options->plan_start_gate_path) == 0 ||
        strcmp(options->metrics_path, options->ready_receipt_path) == 0 ||
        strcmp(options->subject_identity_path,
               options->plan_start_gate_path) == 0 ||
        strcmp(options->subject_identity_path,
               options->ready_receipt_path) == 0 ||
        strcmp(options->campaign_secret_path,
               options->plan_start_gate_path) == 0 ||
        strcmp(options->campaign_secret_path,
               options->ready_receipt_path) == 0 ||
        strcmp(options->plan_start_gate_path,
               options->ready_receipt_path) == 0 ||
        target_does_not_exist(options->events_path) != 0 ||
        target_does_not_exist(options->metrics_path) != 0 ||
        target_does_not_exist(options->plan_start_gate_path) != 0 ||
        target_does_not_exist(options->ready_receipt_path) != 0) {
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

static bool same_file_identity(const struct stat *left,
                               const struct stat *right) {
    return left->st_dev == right->st_dev && left->st_ino == right->st_ino &&
           left->st_mode == right->st_mode && left->st_uid == right->st_uid &&
           left->st_size == right->st_size &&
           left->st_mtimespec.tv_sec == right->st_mtimespec.tv_sec &&
           left->st_mtimespec.tv_nsec == right->st_mtimespec.tv_nsec &&
           left->st_ctimespec.tv_sec == right->st_ctimespec.tv_sec &&
           left->st_ctimespec.tv_nsec == right->st_ctimespec.tv_nsec;
}

static int read_stable_file(const char *path, unsigned char *contents,
                            size_t minimum, size_t maximum, bool private_file,
                            size_t *length_out) {
    struct stat path_before;
    if (lstat(path, &path_before) != 0) {
        return -1;
    }
    if (!S_ISREG(path_before.st_mode) || S_ISLNK(path_before.st_mode) ||
        path_before.st_uid != geteuid() ||
        (private_file && path_before.st_nlink != 1) ||
        (path_before.st_mode & 0022) != 0 ||
        (private_file && (path_before.st_mode & 0077) != 0) ||
        path_before.st_size < (off_t)minimum ||
        path_before.st_size > (off_t)maximum) {
        errno = EACCES;
        return -1;
    }
    int descriptor = open(path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    if (descriptor < 0) {
        return -1;
    }
    struct stat opened;
    size_t offset = 0;
    int result = 0;
    if (fstat(descriptor, &opened) != 0 ||
        !same_file_identity(&path_before, &opened)) {
        result = -1;
        errno = EAGAIN;
    }
    while (result == 0 && offset < (size_t)opened.st_size) {
        ssize_t count = read(descriptor, contents + offset,
                             (size_t)opened.st_size - offset);
        if (count < 0 && errno == EINTR) {
            continue;
        }
        if (count <= 0) {
            result = -1;
            errno = EIO;
            break;
        }
        offset += (size_t)count;
    }
    unsigned char extra = 0;
    ssize_t extra_count = result == 0 ? read(descriptor, &extra, 1) : 0;
    struct stat after;
    struct stat path_after;
    if (result == 0 &&
        (extra_count != 0 || fstat(descriptor, &after) != 0 ||
         lstat(path, &path_after) != 0 ||
         !same_file_identity(&path_before, &after) ||
         !same_file_identity(&path_before, &path_after))) {
        result = -1;
        errno = EAGAIN;
    }
    int saved_errno = errno;
    if (close(descriptor) != 0 && result == 0) {
        result = -1;
        saved_errno = errno;
    }
    errno = saved_errno;
    if (result == 0) {
        *length_out = offset;
    }
    return result;
}

static int exact_subject_value(const unsigned char *contents, size_t length,
                               const char *key, char *output,
                               size_t output_capacity) {
    size_t key_length = strlen(key);
    size_t offset = 0;
    unsigned int matches = 0;
    while (offset < length) {
        const unsigned char *newline =
            memchr(contents + offset, '\n', length - offset);
        if (newline == NULL) {
            return -1;
        }
        size_t line_length = (size_t)(newline - (contents + offset));
        if (line_length > key_length + 1 &&
            memcmp(contents + offset, key, key_length) == 0 &&
            contents[offset + key_length] == '\t') {
            size_t value_length = line_length - key_length - 1;
            if (++matches != 1 || value_length >= output_capacity ||
                memchr(contents + offset + key_length + 1, '\t',
                       value_length) != NULL ||
                memchr(contents + offset + key_length + 1, '\0',
                       value_length) != NULL ||
                memchr(contents + offset + key_length + 1, '\r',
                       value_length) != NULL) {
                return -1;
            }
            memcpy(output, contents + offset + key_length + 1, value_length);
            output[value_length] = '\0';
        }
        offset += line_length + 1;
    }
    return matches == 1 ? 0 : -1;
}

static int load_authentication_bindings(struct run_state *state) {
    unsigned char *identity = malloc(MAX_IDENTITY_BYTES);
    if (identity == NULL) {
        return -1;
    }
    size_t identity_length = 0;
    int result = read_stable_file(state->options.subject_identity_path,
                                  identity, 1, MAX_IDENTITY_BYTES, false,
                                  &identity_length);
    char pid_text[32];
    memset(pid_text, 0, sizeof(pid_text));
    if (result == 0) {
        CC_SHA256(identity, (CC_LONG)identity_length,
                  state->subject_identity_digest);
        result = exact_subject_value(identity, identity_length, "process_pid",
                                     pid_text, sizeof(pid_text));
    }
    if (result == 0) {
        uint64_t parsed_pid = 0;
        result = parse_positive_u64(pid_text, INT32_MAX, &parsed_pid);
        state->subject_process_pid = parsed_pid;
    }
    if (result == 0) {
        result = exact_subject_value(
            identity, identity_length, "process_start_identity",
            state->subject_process_start_identity,
            sizeof(state->subject_process_start_identity));
    }
    memset(identity, 0, MAX_IDENTITY_BYTES);
    free(identity);
    if (result != 0) {
        errno = EPROTO;
        return -1;
    }
    if (read_stable_file(state->options.campaign_secret_path,
                         state->campaign_secret, 32, MAX_SECRET_BYTES, true,
                         &state->campaign_secret_length) != 0) {
        return -1;
    }
    struct stat tty;
    struct stat output_tty;
    state->producer_pid = getpid();
    state->producer_session_id = getsid(0);
    state->producer_process_group = getpgrp();
    if (state->producer_pid <= 0 || state->producer_session_id <= 0 ||
        state->producer_process_group <= 0 ||
        !isatty(STDIN_FILENO) || !isatty(STDOUT_FILENO) ||
        fstat(STDIN_FILENO, &tty) != 0 || !S_ISCHR(tty.st_mode) ||
        fstat(STDOUT_FILENO, &output_tty) != 0 ||
        !S_ISCHR(output_tty.st_mode) ||
        tty.st_dev != output_tty.st_dev || tty.st_ino != output_tty.st_ino ||
        tty.st_rdev != output_tty.st_rdev ||
        tcgetsid(STDIN_FILENO) != state->producer_session_id ||
        tcgetsid(STDOUT_FILENO) != state->producer_session_id ||
        tcgetpgrp(STDIN_FILENO) != state->producer_process_group ||
        tcgetpgrp(STDOUT_FILENO) != state->producer_process_group ||
        tty.st_dev == 0 || tty.st_ino == 0 || tty.st_rdev == 0 ||
        continuous_nanoseconds(&state->producer_started_continuous_ns) != 0) {
        errno = EPROTO;
        return -1;
    }
    state->tty_device = (uint64_t)tty.st_dev;
    state->tty_inode = (uint64_t)tty.st_ino;
    state->tty_rdev = (uint64_t)tty.st_rdev;
    return 0;
}

static int campaign_secret_unchanged(const struct run_state *state) {
    unsigned char current[MAX_SECRET_BYTES];
    size_t current_length = 0;
    int result = read_stable_file(state->options.campaign_secret_path, current,
                                  32, MAX_SECRET_BYTES, true,
                                  &current_length);
    if (result == 0 &&
        (current_length != state->campaign_secret_length ||
         memcmp(current, state->campaign_secret, current_length) != 0)) {
        result = -1;
        errno = EAUTH;
    }
    memset(current, 0, sizeof(current));
    return result;
}

static void encode_u64_be(uint64_t value, unsigned char bytes[8]);
static void hex_digest(
    const unsigned char digest[CC_SHA256_DIGEST_LENGTH],
    char output[CC_SHA256_DIGEST_LENGTH * 2 + 1]);

static void hmac_sha256_hex(const unsigned char *key, size_t key_length,
                            const unsigned char *bytes, size_t length,
                            char output[CC_SHA256_DIGEST_LENGTH * 2 + 1]) {
    unsigned char digest[CC_SHA256_DIGEST_LENGTH];
    CCHmac(kCCHmacAlgSHA256, key, key_length, bytes, length, digest);
    static const char digits[] = "0123456789abcdef";
    for (size_t index = 0; index < CC_SHA256_DIGEST_LENGTH; ++index) {
        output[index * 2] = digits[digest[index] >> 4];
        output[index * 2 + 1] = digits[digest[index] & 0x0f];
    }
    output[CC_SHA256_DIGEST_LENGTH * 2] = '\0';
    memset(digest, 0, sizeof(digest));
}

static int wait_for_plan_start_gate(struct run_state *state) {
    uint64_t wait_started = 0;
    if (continuous_nanoseconds(&wait_started) != 0) {
        return -1;
    }
    unsigned char gate[4096];
    size_t gate_length = 0;
    for (;;) {
        if (read_stable_file(state->options.plan_start_gate_path, gate,
                             1, sizeof(gate), true, &gate_length) == 0) {
            break;
        }
        if (errno != ENOENT) {
            return -1;
        }
        uint64_t now = 0;
        if (termination_signal != 0 || continuous_nanoseconds(&now) != 0 ||
            now - wait_started > MAX_START_GATE_WAIT_NS) {
            errno = ETIMEDOUT;
            return -1;
        }
        struct timespec pause = {.tv_sec = 0, .tv_nsec = 10000000};
        while (nanosleep(&pause, &pause) != 0 && errno == EINTR &&
               termination_signal == 0) {
        }
    }

    char start_text[32];
    char ready_hash[CC_SHA256_DIGEST_LENGTH * 2 + 1];
    memset(start_text, 0, sizeof(start_text));
    memset(ready_hash, 0, sizeof(ready_hash));
    if (exact_subject_value(gate, gate_length,
                            "plan_start_continuous_ns", start_text,
                            sizeof(start_text)) != 0 ||
        exact_subject_value(gate, gate_length, "ready_receipt_sha256",
                            ready_hash, sizeof(ready_hash)) != 0 ||
        !lower_hex_64(ready_hash) ||
        parse_positive_u64(start_text, UINT64_MAX,
                           &state->plan_start_continuous_ns) != 0) {
        errno = EPROTO;
        return -1;
    }
    char unsigned_gate[1024];
    char expected_ready_hash[CC_SHA256_DIGEST_LENGTH * 2 + 1];
    hex_digest(state->ready_receipt_digest, expected_ready_hash);
    if (strcmp(ready_hash, expected_ready_hash) != 0) {
        errno = EAUTH;
        return -1;
    }
    int unsigned_length = snprintf(
        unsigned_gate, sizeof(unsigned_gate),
        "format_version\t1\n"
        "campaign_id\t%s\n"
        "session_id\t%s\n"
        "nonce\t%s\n"
        "ready_receipt_sha256\t%s\n"
        "plan_start_continuous_ns\t%s\n",
        state->options.campaign_id, state->options.session_id,
        state->options.nonce, ready_hash, start_text);
    if (unsigned_length < 0 ||
        (size_t)unsigned_length >= sizeof(unsigned_gate)) {
        errno = EOVERFLOW;
        return -1;
    }
    static const unsigned char magic[] =
        "spaceterm.performance.plan-start-gate/v1";
    unsigned char authenticated[sizeof(magic) + 8 + sizeof(unsigned_gate)];
    size_t authenticated_length = 0;
    memcpy(authenticated, magic, sizeof(magic));
    authenticated_length += sizeof(magic);
    unsigned char encoded_length[8];
    encode_u64_be((uint64_t)unsigned_length, encoded_length);
    memcpy(authenticated + authenticated_length, encoded_length,
           sizeof(encoded_length));
    authenticated_length += sizeof(encoded_length);
    memcpy(authenticated + authenticated_length, unsigned_gate,
           (size_t)unsigned_length);
    authenticated_length += (size_t)unsigned_length;
    char signature[CC_SHA256_DIGEST_LENGTH * 2 + 1];
    hmac_sha256_hex(state->campaign_secret, state->campaign_secret_length,
                    authenticated, authenticated_length, signature);
    char expected[1280];
    int expected_length = snprintf(expected, sizeof(expected),
                                   "%sstart_gate_hmac_sha256\t%s\n",
                                   unsigned_gate, signature);
    memset(authenticated, 0, sizeof(authenticated));
    memset(signature, 0, sizeof(signature));
    if (expected_length < 0 || (size_t)expected_length != gate_length) {
        errno = EAUTH;
        return -1;
    }
    unsigned char difference = 0;
    for (size_t index = 0; index < gate_length; ++index) {
        difference |= gate[index] ^ (unsigned char)expected[index];
    }
    memset(gate, 0, sizeof(gate));
    memset(expected, 0, sizeof(expected));
    if (difference != 0) {
        errno = EAUTH;
        return -1;
    }

    uint64_t verified_at = 0;
    if (continuous_nanoseconds(&verified_at) != 0 ||
        state->plan_start_continuous_ns <
            verified_at + MIN_START_GATE_LEAD_NS ||
        state->plan_start_continuous_ns >
            verified_at + MAX_START_GATE_LEAD_NS) {
        errno = EPROTO;
        return -1;
    }
    uint64_t observed_start = 0;
    while (termination_signal == 0) {
        uint64_t now = 0;
        if (continuous_nanoseconds(&now) != 0) {
            return -1;
        }
        if (now >= state->plan_start_continuous_ns) {
            observed_start = now;
            break;
        }
        uint64_t remaining = state->plan_start_continuous_ns - now;
        uint64_t slice = remaining < UINT64_C(10000000)
                             ? remaining
                             : UINT64_C(10000000);
        struct timespec pause = {
            .tv_sec = (time_t)(slice / UINT64_C(1000000000)),
            .tv_nsec = (long)(slice % UINT64_C(1000000000)),
        };
        while (nanosleep(&pause, &pause) != 0 && errno == EINTR &&
               termination_signal == 0) {
        }
    }
    if (termination_signal != 0 ||
        observed_start < state->plan_start_continuous_ns ||
        observed_start - state->plan_start_continuous_ns >
            MAX_START_LATENESS_NS) {
        errno = ETIMEDOUT;
        return -1;
    }
    return campaign_secret_unchanged(state);
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
    if ((state->event_sequence > 0 &&
         timestamp <= state->last_event_continuous_ns) ||
        state->event_sequence == UINT64_MAX) {
        errno = EPROTO;
        state->event_log_failed = true;
        state->runtime_failed = true;
        return -1;
    }
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
    state->last_event_continuous_ns = timestamp;
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

static int record_progress(struct run_state *state, uint64_t timestamp) {
    if (state->progress_events >= MAX_PROGRESS_EVENTS ||
        read_geometry(state) != 0 ||
        (state->progress_events > 0 &&
         state->emitted_bytes <= state->last_progress_bytes)) {
        errno = EOVERFLOW;
        state->runtime_failed = true;
        return -1;
    }
    char event_id[32];
    int length = snprintf(event_id, sizeof(event_id), "progress-%06" PRIu64,
                          state->progress_events);
    if (length < 0 || (size_t)length >= sizeof(event_id) ||
        record_event_at(state, timestamp, "progress", event_id,
                        state->emitted_bytes, "ok") != 0) {
        state->runtime_failed = true;
        return -1;
    }
    state->last_progress_bytes = state->emitted_bytes;
    state->progress_events += 1;
    return 0;
}

static int record_due_progress(struct run_state *state) {
    uint64_t now = 0;
    if (continuous_nanoseconds(&now) != 0) {
        state->runtime_failed = true;
        return -1;
    }
    if (now < state->next_progress_deadline_ns) {
        return 0;
    }
    if (now - state->next_progress_deadline_ns >
        MAX_PROGRESS_LATENESS_NS) {
        errno = ETIMEDOUT;
        state->runtime_failed = true;
        return -1;
    }
    if (record_progress(state, now) != 0 ||
        state->next_progress_deadline_ns >
            UINT64_MAX - PROGRESS_INTERVAL_NS) {
        errno = EOVERFLOW;
        state->runtime_failed = true;
        return -1;
    }
    state->next_progress_deadline_ns += PROGRESS_INTERVAL_NS;
    return 0;
}

static int begin_measured_progress(struct run_state *state) {
    if (state->started_continuous_ns >
        UINT64_MAX - PROGRESS_INTERVAL_NS) {
        errno = EOVERFLOW;
        state->runtime_failed = true;
        return -1;
    }
    state->next_progress_deadline_ns =
        state->started_continuous_ns + PROGRESS_INTERVAL_NS;
    return record_progress(state, state->started_continuous_ns);
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
                process_controls(state) != 0 ||
                record_due_progress(state) != 0) {
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
            process_controls(state) != 0 ||
            record_due_progress(state) != 0) {
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
    uint64_t warmup_seconds = state->options.has_iterations
                                  ? 0
                                  : scenario_warmup_seconds(state);
    uint64_t warmup_ns = warmup_seconds * UINT64_C(1000000000);
    if (state->plan_start_continuous_ns > UINT64_MAX - warmup_ns) {
        errno = EOVERFLOW;
        return -1;
    }
    uint64_t deadline = state->plan_start_continuous_ns + warmup_ns;
    uint64_t now = 0;
    while (termination_signal == 0) {
        if (continuous_nanoseconds(&now) != 0) {
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
    if (termination_signal != 0 ||
        continuous_nanoseconds(&state->started_continuous_ns) != 0 ||
        state->started_continuous_ns < deadline ||
        state->started_continuous_ns - deadline > MAX_START_LATENESS_NS) {
        errno = ETIMEDOUT;
        return -1;
    }
    return 0;
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

static int sha256_file(const char *path,
                       unsigned char digest[CC_SHA256_DIGEST_LENGTH],
                       uint64_t *size_out) {
    int descriptor = open(path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    if (descriptor < 0) {
        return -1;
    }
    struct stat before;
    if (fstat(descriptor, &before) != 0 || !S_ISREG(before.st_mode) ||
        before.st_uid != geteuid() || (before.st_mode & 0022) != 0 ||
        before.st_size <= 0 || before.st_size > 64 * 1024 * 1024) {
        close(descriptor);
        errno = EACCES;
        return -1;
    }
    CC_SHA256_CTX hash;
    CC_SHA256_Init(&hash);
    unsigned char buffer[65536];
    uint64_t total = 0;
    for (;;) {
        ssize_t count = read(descriptor, buffer, sizeof(buffer));
        if (count < 0 && errno == EINTR) {
            continue;
        }
        if (count < 0) {
            int saved_errno = errno;
            close(descriptor);
            errno = saved_errno;
            return -1;
        }
        if (count == 0) {
            break;
        }
        CC_SHA256_Update(&hash, buffer, (CC_LONG)count);
        total += (uint64_t)count;
    }
    struct stat after;
    int result = fstat(descriptor, &after) == 0 &&
                         same_file_identity(&before, &after) &&
                         total == (uint64_t)before.st_size
                     ? 0
                     : -1;
    int saved_errno = errno;
    if (close(descriptor) != 0 && result == 0) {
        result = -1;
        saved_errno = errno;
    }
    errno = saved_errno;
    if (result != 0) {
        return -1;
    }
    CC_SHA256_Final(digest, &hash);
    *size_out = total;
    return 0;
}

static int publish_exclusive_private(const char *path,
                                     const unsigned char *contents,
                                     size_t length) {
    size_t path_length = strlen(path);
    if (path_length > SIZE_MAX - 64) {
        errno = ENAMETOOLONG;
        return -1;
    }
    char *temporary = malloc(path_length + 64);
    if (temporary == NULL) {
        return -1;
    }
    int descriptor = -1;
    mode_t previous_umask = umask(077);
    for (unsigned int attempt = 0; attempt < 100; ++attempt) {
        int printed = snprintf(temporary, path_length + 64, "%s.tmp.%ld.%u",
                               path, (long)getpid(), attempt);
        if (printed < 0 || (size_t)printed >= path_length + 64) {
            errno = ENAMETOOLONG;
            break;
        }
        descriptor = open(temporary,
                          O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC | O_NOFOLLOW,
                          0600);
        if (descriptor >= 0 || errno != EEXIST) {
            break;
        }
    }
    int saved_errno = errno;
    umask(previous_umask);
    errno = saved_errno;
    int result = descriptor >= 0 ? 0 : -1;
    if (result == 0 &&
        (write_all_fd(descriptor, contents, length) != 0 ||
         fchmod(descriptor, S_IRUSR) != 0 || fsync(descriptor) != 0 ||
         close(descriptor) != 0)) {
        result = -1;
    }
    if (result == 0 && link(temporary, path) != 0) {
        result = -1;
    }
    saved_errno = errno;
    if (result != 0 && descriptor >= 0) {
        (void)close(descriptor);
    }
    (void)unlink(temporary);
    free(temporary);
    errno = saved_errno;
    return result;
}

static int publish_ready_receipt(struct run_state *state,
                                 uint64_t ready_continuous_ns) {
    if (fsync(state->events_fd) != 0) {
        return -1;
    }
    struct stat events_stat;
    unsigned char events_digest[CC_SHA256_DIGEST_LENGTH];
    uint64_t events_bytes = 0;
    if (fstat(state->events_fd, &events_stat) != 0 ||
        events_stat.st_dev == 0 || events_stat.st_ino == 0 ||
        sha256_file(state->options.events_path, events_digest,
                    &events_bytes) != 0) {
        return -1;
    }
    char subject_hex[CC_SHA256_DIGEST_LENGTH * 2 + 1];
    char events_hex[CC_SHA256_DIGEST_LENGTH * 2 + 1];
    hex_digest(state->subject_identity_digest, subject_hex);
    hex_digest(events_digest, events_hex);
    char unsigned_receipt[4096];
    int unsigned_length = snprintf(
        unsigned_receipt, sizeof(unsigned_receipt),
        "format_version\t1\n"
        "campaign_id\t%s\n"
        "session_id\t%s\n"
        "nonce\t%s\n"
        "subject_identity_sha256\t%s\n"
        "producer_pid\t%d\n"
        "producer_started_continuous_ns\t%" PRIu64 "\n"
        "producer_session_id\t%d\n"
        "producer_process_group\t%d\n"
        "tty_device\t%" PRIu64 "\n"
        "tty_inode\t%" PRIu64 "\n"
        "tty_rdev\t%" PRIu64 "\n"
        "events_device\t%" PRIu64 "\n"
        "events_inode\t%" PRIu64 "\n"
        "events_prefix_bytes\t%" PRIu64 "\n"
        "events_prefix_sha256\t%s\n"
        "measurement_ready_continuous_ns\t%" PRIu64 "\n"
        "measurement_ready_byte_count\t%" PRIu64 "\n"
        "auth_algorithm\thmac-sha256\n",
        state->options.campaign_id, state->options.session_id,
        state->options.nonce, subject_hex, state->producer_pid,
        state->producer_started_continuous_ns, state->producer_session_id,
        state->producer_process_group, state->tty_device, state->tty_inode,
        state->tty_rdev, (uint64_t)events_stat.st_dev,
        (uint64_t)events_stat.st_ino, events_bytes, events_hex,
        ready_continuous_ns, state->emitted_bytes);
    if (unsigned_length < 0 ||
        (size_t)unsigned_length >= sizeof(unsigned_receipt)) {
        errno = EOVERFLOW;
        return -1;
    }
    static const unsigned char magic[] =
        "spaceterm.performance.workload-ready/v1";
    unsigned char authenticated[sizeof(magic) + 8 + sizeof(unsigned_receipt)];
    size_t authenticated_length = 0;
    memcpy(authenticated, magic, sizeof(magic));
    authenticated_length += sizeof(magic);
    unsigned char encoded_length[8];
    encode_u64_be((uint64_t)unsigned_length, encoded_length);
    memcpy(authenticated + authenticated_length, encoded_length, 8);
    authenticated_length += 8;
    memcpy(authenticated + authenticated_length, unsigned_receipt,
           (size_t)unsigned_length);
    authenticated_length += (size_t)unsigned_length;
    char signature[CC_SHA256_DIGEST_LENGTH * 2 + 1];
    hmac_sha256_hex(state->campaign_secret, state->campaign_secret_length,
                    authenticated, authenticated_length, signature);
    char receipt[4608];
    int receipt_length = snprintf(receipt, sizeof(receipt),
                                  "%sready_hmac_sha256\t%s\n",
                                  unsigned_receipt, signature);
    memset(authenticated, 0, sizeof(authenticated));
    memset(signature, 0, sizeof(signature));
    if (receipt_length < 0 || (size_t)receipt_length >= sizeof(receipt) ||
        publish_exclusive_private(state->options.ready_receipt_path,
                                  (const unsigned char *)receipt,
                                  (size_t)receipt_length) != 0) {
        return -1;
    }
    uint64_t receipt_bytes = 0;
    return sha256_file(state->options.ready_receipt_path,
                       state->ready_receipt_digest, &receipt_bytes);
}

static void encode_u64_be(uint64_t value, unsigned char bytes[8]) {
    for (size_t index = 0; index < 8; ++index) {
        bytes[7 - index] = (unsigned char)(value & 0xffU);
        value >>= 8;
    }
}

static int authenticated_events_hmac(
    const char *events_path, const unsigned char *metadata,
    size_t metadata_length, uint64_t events_length,
    const struct run_state *state,
    unsigned char digest[CC_SHA256_DIGEST_LENGTH]) {
    static const unsigned char magic[] =
        "spaceterm.performance.workload-auth/v1";
    unsigned char encoded_length[8];
    CCHmacContext context;
    CCHmacInit(&context, kCCHmacAlgSHA256, state->campaign_secret,
               state->campaign_secret_length);
    CCHmacUpdate(&context, magic, sizeof(magic));
    encode_u64_be((uint64_t)metadata_length, encoded_length);
    CCHmacUpdate(&context, encoded_length, sizeof(encoded_length));
    CCHmacUpdate(&context, metadata, metadata_length);
    encode_u64_be(events_length, encoded_length);
    CCHmacUpdate(&context, encoded_length, sizeof(encoded_length));

    int descriptor = open(events_path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    if (descriptor < 0) {
        memset(&context, 0, sizeof(context));
        return -1;
    }
    unsigned char buffer[65536];
    uint64_t total = 0;
    int result = 0;
    while (total < events_length) {
        size_t remaining = (size_t)(events_length - total);
        size_t requested = remaining < sizeof(buffer) ? remaining
                                                       : sizeof(buffer);
        ssize_t count = read(descriptor, buffer, requested);
        if (count < 0 && errno == EINTR) {
            continue;
        }
        if (count <= 0) {
            result = -1;
            errno = EIO;
            break;
        }
        CCHmacUpdate(&context, buffer, (size_t)count);
        total += (uint64_t)count;
    }
    unsigned char extra = 0;
    ssize_t extra_count = result == 0 ? read(descriptor, &extra, 1) : 0;
    if (extra_count != 0 || total != events_length) {
        result = -1;
        errno = EIO;
    }
    int saved_errno = errno;
    if (close(descriptor) != 0 && result == 0) {
        result = -1;
        saved_errno = errno;
    }
    if (result == 0) {
        CCHmacFinal(&context, digest);
    }
    memset(&context, 0, sizeof(context));
    memset(buffer, 0, sizeof(buffer));
    errno = saved_errno;
    return result;
}

static int publish_private_metrics(
    const char *target,
    const unsigned char producer_digest[CC_SHA256_DIGEST_LENGTH],
    const unsigned char seed_digest[CC_SHA256_DIGEST_LENGTH],
    const struct run_state *state, uint64_t ended_continuous_ns,
    const char *status) {
    char producer_hex[CC_SHA256_DIGEST_LENGTH * 2 + 1];
    char seed_hex[CC_SHA256_DIGEST_LENGTH * 2 + 1];
    char subject_hex[CC_SHA256_DIGEST_LENGTH * 2 + 1];
    char ready_receipt_hex[CC_SHA256_DIGEST_LENGTH * 2 + 1];
    char events_hex[CC_SHA256_DIGEST_LENGTH * 2 + 1];
    hex_digest(producer_digest, producer_hex);
    hex_digest(seed_digest, seed_hex);
    hex_digest(state->subject_identity_digest, subject_hex);
    hex_digest(state->ready_receipt_digest, ready_receipt_hex);

    unsigned char events_digest[CC_SHA256_DIGEST_LENGTH];
    uint64_t events_length = 0;
    if (sha256_file(state->options.events_path, events_digest,
                    &events_length) != 0) {
        return -1;
    }
    hex_digest(events_digest, events_hex);

    char contents[8192];
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
        "format_version\t3\n"
        "scenario\t%s\n"
        "campaign_id\t%s\n"
        "session_id\t%s\n"
        "nonce\t%s\n"
        "subject_identity_sha256\t%s\n"
        "subject_process_pid\t%" PRIu64 "\n"
        "subject_process_start_identity\t%s\n"
        "producer_sha256\t%s\n"
        "producer_pid\t%d\n"
        "producer_started_continuous_ns\t%" PRIu64 "\n"
        "producer_session_id\t%d\n"
        "producer_process_group\t%d\n"
        "tty_device\t%" PRIu64 "\n"
        "tty_inode\t%" PRIu64 "\n"
        "tty_rdev\t%" PRIu64 "\n"
        "ready_receipt_sha256\t%s\n"
        "events_sha256\t%s\n"
        "auth_algorithm\thmac-sha256\n"
        "seed_sha256\t%s\n"
        "seed_bytes\t%" PRIu64 "\n"
        "requested_duration_ms\t%" PRIu64 "\n"
        "warmup_ms\t%" PRIu64 "\n"
        "requested_iterations\t%" PRIu64 "\n"
        "requested_seed_rows\t%" PRIu64 "\n"
        "emitted_bytes\t%" PRIu64 "\n"
        "input_events\t%" PRIu64 "\n"
        "plan_start_continuous_ns\t%" PRIu64 "\n"
        "started_continuous_ns\t%" PRIu64 "\n"
        "ended_continuous_ns\t%" PRIu64 "\n"
        "status\t%s\n",
        state->options.scenario_name, state->options.campaign_id,
        state->options.session_id, state->options.nonce, subject_hex,
        state->subject_process_pid, state->subject_process_start_identity,
        producer_hex, state->producer_pid,
        state->producer_started_continuous_ns, state->producer_session_id,
        state->producer_process_group, state->tty_device, state->tty_inode,
        state->tty_rdev, ready_receipt_hex, events_hex, seed_hex,
        state->seed_bytes, requested_duration_ms, warmup_ms,
        requested_iterations,
        requested_seed_rows, state->emitted_bytes, state->input_events,
        state->plan_start_continuous_ns,
        state->started_continuous_ns, ended_continuous_ns, status);
    if (length < 0 || (size_t)length >= sizeof(contents)) {
        errno = EOVERFLOW;
        return -1;
    }

    unsigned char hmac_digest[CC_SHA256_DIGEST_LENGTH];
    char hmac_hex[CC_SHA256_DIGEST_LENGTH * 2 + 1];
    if (authenticated_events_hmac(
            state->options.events_path, (const unsigned char *)contents,
            (size_t)length, events_length, state, hmac_digest) != 0) {
        return -1;
    }
    hex_digest(hmac_digest, hmac_hex);
    int hmac_length = snprintf(contents + length, sizeof(contents) - (size_t)length,
                               "events_hmac_sha256\t%s\n", hmac_hex);
    if (hmac_length < 0 ||
        (size_t)hmac_length >= sizeof(contents) - (size_t)length) {
        errno = EOVERFLOW;
        return -1;
    }
    length += hmac_length;

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
        fchmod(descriptor, S_IRUSR) != 0 || fsync(descriptor) != 0 ||
        close(descriptor) != 0) {
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
    if (load_authentication_bindings(&state) != 0) {
        memset(state.campaign_secret, 0, sizeof(state.campaign_secret));
        fputs("performance-workload: invalid authentication bindings\n",
              stderr);
        return 1;
    }
    if (open_event_log(&state) != 0) {
        memset(state.campaign_secret, 0, sizeof(state.campaign_secret));
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
    uint64_t measurement_ready_continuous_ns = 0;
    if (setup_ok &&
        emit_initial_seed(&state, continuous_chunk, OUTPUT_CHUNK_BYTES) == 0 &&
        record_event(&state, "seed-complete", "none", state.seed_bytes, "ok",
                     NULL) == 0 &&
        record_event(&state, "measurement-ready", "none", state.emitted_bytes,
                     "ok", &measurement_ready_continuous_ns) == 0 &&
        publish_ready_receipt(&state, measurement_ready_continuous_ns) == 0 &&
        wait_for_plan_start_gate(&state) == 0 &&
        emit_warmup_output(&state, continuous_chunk, OUTPUT_CHUNK_BYTES) == 0 &&
        state.started_continuous_ns > producer_started_continuous_ns &&
        begin_measured_progress(&state) == 0 &&
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

    uint64_t final_progress_time = 0;
    if (completed && sentinel_written &&
        (continuous_nanoseconds(&final_progress_time) != 0 ||
         record_progress(&state, final_progress_time) != 0)) {
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
        fchmod(state.events_fd, S_IRUSR) == 0 &&
        fsync(state.events_fd) == 0 && close(state.events_fd) == 0;
    state.events_fd = -1;
    if (final_evidence_ok) {
        final_evidence_ok =
            campaign_secret_unchanged(&state) == 0 &&
            publish_private_metrics(state.options.metrics_path,
                                    producer_digest, seed_digest, &state,
                                    ended_continuous_ns,
                                    metrics_status) == 0;
    }

    memset(state.campaign_secret, 0, sizeof(state.campaign_secret));
    if (!final_evidence_ok || !completed || !sentinel_written) {
        return observed_signal != 0 ? 128 + observed_signal : 1;
    }
    return 0;
}
