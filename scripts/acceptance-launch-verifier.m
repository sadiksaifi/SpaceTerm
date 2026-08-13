#import <AppKit/AppKit.h>
#import <CommonCrypto/CommonDigest.h>
#import <Foundation/Foundation.h>
#import <Security/Security.h>

#include <bsm/libbsm.h>
#include <errno.h>
#include <fcntl.h>
#include <libproc.h>
#include <limits.h>
#include <mach/message.h>
#include <signal.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <unistd.h>

static const uint32_t kMaximumFrameBytes = 16 * 1024;
static const uint64_t kMaximumRuntimeSamples = 43201;
static const uint64_t kMaximumRuntimeEvents = 65536;
static const NSUInteger kMaximumRuntimeSampleBytes = 32 * 1024 * 1024;
static const NSUInteger kMaximumRuntimeEventBytes = 16 * 1024 * 1024;
static const uint64_t kRuntimeSampleIntervalMilliseconds = 1000;
static const uint64_t kRuntimeTransitionCapacity = 64;
static const NSTimeInterval kProofTimeoutSeconds = 30.0;
static volatile sig_atomic_t interrupted = 0;

static NSString *const kRuntimeSchema = @"spaceterm.acceptance.runtime-stream/v1";
static NSString *const kRuntimeTickSchema = @"spaceterm.acceptance.runtime-tick/v1";
static NSString *const kRuntimeCompleteSchema = @"spaceterm.acceptance.runtime-complete/v1";
static NSString *const kRuntimeAckSchema = @"spaceterm.acceptance.runtime-ack/v1";
static NSString *const kRuntimeClosedSchema = @"spaceterm.acceptance.runtime-closed/v1";

static NSString *const kRuntimeSampleHeader =
    @"sequence\tcontinuous_ns\tworker_generation\tscreens_published\tscreens_enqueued\t"
     "screens_superseded\tevent_queue_length\tevent_queue_high_water\tui_dispatches\t"
     "ui_screen_events\tui_drain_high_water\tui_latest_generation\trender_latest_generation\t"
     "next_frame_generation\tnext_frame_count\tpresentable\tminimized\toccluded\t"
     "workspace_visible\tpane_visible\tlive_resize\tviewport_total_rows\t"
     "viewport_visible_rows\tviewport_offset_rows\tselection_present\tresize_requests\t"
     "resize_notifications\tresize_applied\tresize_coalesced\tpty_rows\tpty_columns\t"
     "pty_pixel_width\tpty_pixel_height\tterminal_inputs_accepted\tlifecycle\tobserver_drops\n";
static NSString *const kRuntimeEventHeader =
    @"sequence\tcontinuous_ns\tkind\tgeneration\taux0\taux1\n";

typedef struct {
    __strong NSString *app;
    __strong NSString *executable;
    __strong NSString *runID;
    __strong NSString *appSHA256;
    __strong NSString *cdhash;
    __strong NSString *identifier;
    __strong NSString *teamIdentifier;
    __strong NSString *home;
    __strong NSString *output;
    bool replay;
} Options;

typedef struct {
    __strong NSMutableData *samples;
    __strong NSMutableData *events;
    uint64_t sampleCount;
    uint64_t eventCount;
    uint64_t firstContinuousNS;
    uint64_t lastContinuousNS;
    uint64_t lastEventContinuousNS;
    uint64_t pendingSampleIntervalNS;
    uint64_t previousSample[35];
    bool hasSample;
    bool hasEvent;
    bool hasPendingSampleInterval;
    bool observedFailure;
} RuntimeCapture;

static void handle_signal(int signal_number) {
    (void)signal_number;
    interrupted = 1;
}

static bool report(NSString *message) {
    fprintf(stderr, "acceptance launch verifier: %s\n", message.UTF8String);
    return false;
}

static bool set_close_on_exec(int fd) {
    int flags = fcntl(fd, F_GETFD);
    return flags >= 0 && fcntl(fd, F_SETFD, flags | FD_CLOEXEC) == 0;
}

static bool is_lower_hex(NSString *value, NSUInteger length) {
    if (value.length != length) {
        return false;
    }
    NSCharacterSet *invalid =
        [[NSCharacterSet characterSetWithCharactersInString:@"0123456789abcdef"] invertedSet];
    return [value rangeOfCharacterFromSet:invalid].location == NSNotFound;
}

static bool is_run_id(NSString *value) {
    if (value.length == 0 || value.length > 80) {
        return false;
    }
    NSCharacterSet *first = [NSCharacterSet alphanumericCharacterSet];
    NSCharacterSet *rest = [NSCharacterSet characterSetWithCharactersInString:
        @"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789._-"];
    return [first characterIsMember:[value characterAtIndex:0]] &&
        [value rangeOfCharacterFromSet:rest.invertedSet].location == NSNotFound;
}

static NSString *canonical_path(NSString *path) {
    char resolved[PATH_MAX];
    if (realpath(path.fileSystemRepresentation, resolved) == NULL) {
        return nil;
    }
    return [[NSFileManager defaultManager]
        stringWithFileSystemRepresentation:resolved
        length:strlen(resolved)];
}

static bool parse_options(int argc, const char *argv[], Options *options) {
    NSMutableDictionary<NSString *, NSString *> *values = [NSMutableDictionary dictionary];
    if (argc != 21) {
        return report(@"expected ten named option/value pairs");
    }
    for (int index = 1; index < argc; index += 2) {
        NSString *key = [NSString stringWithUTF8String:argv[index]];
        NSString *value = [NSString stringWithUTF8String:argv[index + 1]];
        if (key == nil || value == nil || ![key hasPrefix:@"--"] || values[key] != nil) {
            return report(@"invalid or duplicated command-line option");
        }
        values[key] = value;
    }
    NSArray<NSString *> *keys = @[
        @"--app", @"--executable", @"--run-id", @"--app-sha256", @"--cdhash",
        @"--identifier", @"--team-identifier", @"--home", @"--output", @"--mode"
    ];
    for (NSString *key in keys) {
        if (values[key] == nil) {
            return report([NSString stringWithFormat:@"missing option %@", key]);
        }
    }
    if (values.count != keys.count) {
        return report(@"unknown command-line option");
    }
    NSString *mode = values[@"--mode"];
    if (![mode isEqualToString:@"campaign"] && ![mode isEqualToString:@"replay"]) {
        return report(@"mode must be campaign or replay");
    }
    if (![values[@"--app"] isAbsolutePath] || ![values[@"--executable"] isAbsolutePath] ||
        ![values[@"--home"] isAbsolutePath] || ![values[@"--output"] isAbsolutePath] ||
        !is_run_id(values[@"--run-id"]) || !is_lower_hex(values[@"--app-sha256"], 64)) {
        return report(@"invalid path, run ID, or application hash");
    }
    NSCharacterSet *not_hex =
        [[NSCharacterSet characterSetWithCharactersInString:@"0123456789abcdefABCDEF"] invertedSet];
    if (values[@"--cdhash"].length == 0 ||
        [values[@"--cdhash"] rangeOfCharacterFromSet:not_hex].location != NSNotFound ||
        values[@"--identifier"].length == 0) {
        return report(@"invalid expected signing identity");
    }
    options->app = values[@"--app"];
    options->executable = values[@"--executable"];
    options->runID = values[@"--run-id"];
    options->appSHA256 = values[@"--app-sha256"];
    options->cdhash = values[@"--cdhash"].uppercaseString;
    options->identifier = values[@"--identifier"];
    options->teamIdentifier = values[@"--team-identifier"];
    options->home = values[@"--home"];
    options->output = values[@"--output"];
    options->replay = [mode isEqualToString:@"replay"];
    return true;
}

static bool wait_for_fd(int fd, bool writing, NSTimeInterval timeout) {
    NSDate *deadline = [NSDate dateWithTimeIntervalSinceNow:timeout];
    while (!interrupted) {
        NSTimeInterval remaining = [deadline timeIntervalSinceNow];
        if (remaining <= 0) {
            errno = ETIMEDOUT;
            return false;
        }
        struct timeval interval = {
            .tv_sec = (time_t)remaining,
            .tv_usec = (suseconds_t)((remaining - (time_t)remaining) * 1000000.0),
        };
        fd_set set;
        FD_ZERO(&set);
        FD_SET(fd, &set);
        int result = select(fd + 1, writing ? NULL : &set, writing ? &set : NULL, NULL, &interval);
        if (result > 0) {
            return true;
        }
        if (result < 0 && errno == EINTR) {
            continue;
        }
        return false;
    }
    errno = EINTR;
    return false;
}

static bool wait_for_launch(dispatch_semaphore_t launched) {
    NSDate *deadline = [NSDate dateWithTimeIntervalSinceNow:kProofTimeoutSeconds];
    while (!interrupted && [deadline timeIntervalSinceNow] > 0) {
        if (dispatch_semaphore_wait(
                launched, dispatch_time(DISPATCH_TIME_NOW, 100 * NSEC_PER_MSEC)) == 0) {
            return true;
        }
    }
    return false;
}

static bool write_all(int fd, const uint8_t *bytes, size_t length) {
    size_t offset = 0;
    NSDate *deadline = [NSDate dateWithTimeIntervalSinceNow:kProofTimeoutSeconds];
    while (offset < length && !interrupted) {
        NSTimeInterval remaining = [deadline timeIntervalSinceNow];
        if (remaining <= 0 || !wait_for_fd(fd, true, remaining)) {
            return false;
        }
        ssize_t written = write(fd, bytes + offset, length - offset);
        if (written > 0) {
            offset += (size_t)written;
        } else if (written < 0 && errno == EINTR) {
            continue;
        } else {
            return false;
        }
    }
    return offset == length;
}

static bool read_all(int fd, uint8_t *bytes, size_t length) {
    size_t offset = 0;
    NSDate *deadline = [NSDate dateWithTimeIntervalSinceNow:kProofTimeoutSeconds];
    while (offset < length && !interrupted) {
        NSTimeInterval remaining = [deadline timeIntervalSinceNow];
        if (remaining <= 0 || !wait_for_fd(fd, false, remaining)) {
            return false;
        }
        ssize_t count = read(fd, bytes + offset, length - offset);
        if (count > 0) {
            offset += (size_t)count;
        } else if (count < 0 && errno == EINTR) {
            continue;
        } else {
            return false;
        }
    }
    return offset == length;
}

static bool write_frame(int fd, NSData *payload) {
    if (payload.length == 0 || payload.length > kMaximumFrameBytes) {
        return false;
    }
    uint32_t length = CFSwapInt32HostToBig((uint32_t)payload.length);
    return write_all(fd, (const uint8_t *)&length, sizeof(length)) &&
        write_all(fd, payload.bytes, payload.length);
}

static NSData *read_frame(int fd) {
    uint32_t encoded_length = 0;
    if (!read_all(fd, (uint8_t *)&encoded_length, sizeof(encoded_length))) {
        return nil;
    }
    uint32_t length = CFSwapInt32BigToHost(encoded_length);
    if (length == 0 || length > kMaximumFrameBytes) {
        return nil;
    }
    NSMutableData *data = [NSMutableData dataWithLength:length];
    return read_all(fd, data.mutableBytes, length) ? data : nil;
}

static NSString *hex_data(NSData *data) {
    const uint8_t *bytes = data.bytes;
    NSMutableString *result = [NSMutableString stringWithCapacity:data.length * 2];
    for (NSUInteger index = 0; index < data.length; index++) {
        [result appendFormat:@"%02X", bytes[index]];
    }
    return result;
}

static NSString *encode_value(NSString *value) {
    return [[[[value stringByReplacingOccurrencesOfString:@"%" withString:@"%25"]
        stringByReplacingOccurrencesOfString:@"\t" withString:@"%09"]
        stringByReplacingOccurrencesOfString:@"\r" withString:@"%0D"]
        stringByReplacingOccurrencesOfString:@"\n" withString:@"%0A"];
}

static NSString *decode_value(NSString *value) {
    NSData *input = [value dataUsingEncoding:NSUTF8StringEncoding];
    const uint8_t *bytes = input.bytes;
    NSMutableData *decoded = [NSMutableData dataWithCapacity:input.length];
    for (NSUInteger index = 0; index < input.length; index++) {
        uint8_t byte = bytes[index];
        if (byte != '%') {
            [decoded appendBytes:&byte length:1];
            continue;
        }
        if (index + 2 >= input.length) {
            return nil;
        }
        const uint8_t *escape = &bytes[index + 1];
        if (escape[0] == '2' && escape[1] == '5') byte = '%';
        else if (escape[0] == '0' && escape[1] == '9') byte = '\t';
        else if (escape[0] == '0' && escape[1] == 'D') byte = '\r';
        else if (escape[0] == '0' && escape[1] == 'A') byte = '\n';
        else return nil;
        [decoded appendBytes:&byte length:1];
        index += 2;
    }
    return [[NSString alloc] initWithData:decoded encoding:NSUTF8StringEncoding];
}

static NSDictionary<NSString *, NSString *> *parse_records(
    NSData *data,
    NSArray<NSString *> *expected_keys
) {
    NSString *text = [[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding];
    if (text == nil || ![text hasSuffix:@"\n"]) {
        return nil;
    }
    NSArray<NSString *> *lines = [[text substringToIndex:text.length - 1]
        componentsSeparatedByString:@"\n"];
    if (lines.count != expected_keys.count) {
        return nil;
    }
    NSMutableDictionary<NSString *, NSString *> *records = [NSMutableDictionary dictionary];
    for (NSUInteger index = 0; index < lines.count; index++) {
        NSArray<NSString *> *parts = [lines[index] componentsSeparatedByString:@"\t"];
        if (parts.count != 2 || ![parts[0] isEqualToString:expected_keys[index]]) {
            return nil;
        }
        NSString *decoded = decode_value(parts[1]);
        if (decoded == nil || records[parts[0]] != nil) {
            return nil;
        }
        records[parts[0]] = decoded;
    }
    return records;
}

static bool positive_integer(NSString *value) {
    if (value.length == 0 || [value characterAtIndex:0] == '0') {
        return false;
    }
    return [value rangeOfCharacterFromSet:NSCharacterSet.decimalDigitCharacterSet.invertedSet]
        .location == NSNotFound;
}

static bool canonical_uint64(NSString *value, uint64_t *result) {
    if (value.length == 0 || (value.length > 1 && [value characterAtIndex:0] == '0')) {
        return false;
    }
    uint64_t parsed = 0;
    for (NSUInteger index = 0; index < value.length; index++) {
        unichar character = [value characterAtIndex:index];
        if (character < '0' || character > '9') {
            return false;
        }
        uint64_t digit = character - '0';
        if (parsed > (UINT64_MAX - digit) / 10) {
            return false;
        }
        parsed = parsed * 10 + digit;
    }
    *result = parsed;
    return true;
}

static bool canonical_bool_digit(NSString *value, uint64_t *result) {
    return canonical_uint64(value, result) && *result <= 1;
}

static bool positive_number(NSString *value) {
    NSScanner *scanner = [NSScanner scannerWithString:value];
    double number = 0;
    return [scanner scanDouble:&number] && scanner.isAtEnd && isfinite(number) && number > 0;
}

static NSData *challenge_data(NSString *nonce, const Options *options) {
    NSString *challenge = [NSString stringWithFormat:
        @"schema\tspaceterm.acceptance.native-launch-challenge/v2\n"
         "launch.nonce\t%@\nrun.id\t%@\npackage.app.sha256\t%@\n"
         "runtime.schema\t%@\nruntime.sample_interval_ms\t%llu\n"
         "runtime.transition_capacity\t%llu\n",
        nonce, options->runID, options->appSHA256, kRuntimeSchema,
        (unsigned long long)kRuntimeSampleIntervalMilliseconds,
        (unsigned long long)kRuntimeTransitionCapacity];
    return [challenge dataUsingEncoding:NSUTF8StringEncoding];
}

static NSDictionary<NSString *, NSString *> *validate_response(
    NSData *data,
    NSString *nonce,
    const Options *options,
    pid_t peer_pid,
    NSString *expected_path,
    const struct stat *expected_stat
) {
    NSArray<NSString *> *keys = @[
        @"schema", @"observation.source", @"launch.nonce", @"run.id",
        @"package.app.sha256", @"runtime.schema", @"runtime.sample_interval_ms",
        @"runtime.transition_capacity", @"process.pid", @"process.executable.path",
        @"process.executable.device", @"process.executable.inode", @"terminal_font_selected",
        @"initial_grid.rows", @"initial_grid.columns", @"initial_grid.logical_width",
        @"initial_grid.logical_height", @"initial_grid.backing_pixel_width",
        @"initial_grid.backing_pixel_height", @"observation.complete"
    ];
    NSDictionary<NSString *, NSString *> *records = parse_records(data, keys);
    NSString *expected_device = [NSString stringWithFormat:@"%llu",
        (unsigned long long)expected_stat->st_dev];
    NSString *expected_inode = [NSString stringWithFormat:@"%llu",
        (unsigned long long)expected_stat->st_ino];
    if (records == nil ||
        ![records[@"schema"] isEqualToString:@"spaceterm.acceptance.native-launch-proof/v3"] ||
        ![records[@"observation.source"] isEqualToString:@"production-app"] ||
        ![records[@"launch.nonce"] isEqualToString:nonce] ||
        ![records[@"run.id"] isEqualToString:options->runID] ||
        ![records[@"package.app.sha256"] isEqualToString:options->appSHA256] ||
        ![records[@"runtime.schema"] isEqualToString:kRuntimeSchema] ||
        ![records[@"runtime.sample_interval_ms"] isEqualToString:@"1000"] ||
        ![records[@"runtime.transition_capacity"] isEqualToString:@"64"] ||
        !positive_integer(records[@"process.pid"]) ||
        records[@"process.pid"].intValue != peer_pid ||
        ![records[@"process.executable.path"] isEqualToString:expected_path] ||
        ![records[@"process.executable.device"] isEqualToString:expected_device] ||
        ![records[@"process.executable.inode"] isEqualToString:expected_inode] ||
        records[@"terminal_font_selected"].length == 0 ||
        records[@"terminal_font_selected"].length > 256 ||
        !positive_integer(records[@"initial_grid.rows"]) ||
        !positive_integer(records[@"initial_grid.columns"]) ||
        !positive_number(records[@"initial_grid.logical_width"]) ||
        !positive_number(records[@"initial_grid.logical_height"]) ||
        !positive_integer(records[@"initial_grid.backing_pixel_width"]) ||
        !positive_integer(records[@"initial_grid.backing_pixel_height"]) ||
        ![records[@"observation.complete"] isEqualToString:@"true"]) {
        return nil;
    }
    return records;
}

static bool append_bounded(NSMutableData *output, NSString *line, NSUInteger maximum) {
    NSData *encoded = [line dataUsingEncoding:NSUTF8StringEncoding];
    if (encoded == nil || encoded.length > maximum || output.length > maximum - encoded.length) {
        return false;
    }
    [output appendData:encoded];
    return true;
}

static NSString *exact_record_value(NSString *line, NSString *key) {
    NSArray<NSString *> *parts = [line componentsSeparatedByString:@"\t"];
    return parts.count == 2 && [parts[0] isEqualToString:key] ? parts[1] : nil;
}

static bool is_runtime_lifecycle(NSString *value, uint64_t *code) {
    NSArray<NSString *> *values = @[
        @"starting", @"running", @"exited", @"failed", @"observer-failed"
    ];
    NSUInteger index = [values indexOfObject:value];
    if (index == NSNotFound) {
        return false;
    }
    *code = index;
    return true;
}

static bool lifecycle_transition_is_valid(uint64_t before, uint64_t after) {
    switch (before) {
        case 0: return true;
        case 1: return after == 1 || after == 2 || after == 3 || after == 4;
        case 2: return after == 2 || after == 4;
        case 3: return after == 3 || after == 4;
        case 4: return after == 4;
        default: return false;
    }
}

static bool is_runtime_event_kind(NSString *value) {
    return [@[
        @"visibility-lost", @"visibility-restored", @"first-next-frame-after-restore",
        @"session-exited", @"session-failed", @"observer-failed"
    ] containsObject:value];
}

static bool runtime_event_aux_is_valid(
    NSString *kind,
    uint64_t aux0,
    uint64_t aux1
) {
    if ([kind isEqualToString:@"session-exited"]) {
        return aux0 >= 1 && aux0 <= 5 && aux1 == 0;
    }
    if ([kind isEqualToString:@"session-failed"]) {
        return aux0 >= 1 && aux0 <= 7 && aux1 == 0;
    }
    return aux0 == 0 && aux1 == 0;
}

static bool parse_runtime_tick(NSData *data, RuntimeCapture *capture) {
    NSString *text = [[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding];
    if (text == nil || ![text hasSuffix:@"\n"] || capture->sampleCount >= kMaximumRuntimeSamples) {
        return false;
    }
    NSArray<NSString *> *lines = [[text substringToIndex:text.length - 1]
        componentsSeparatedByString:@"\n"];
    if (lines.count < 4 ||
        ![exact_record_value(lines[0], @"schema") isEqualToString:kRuntimeTickSchema]) {
        return false;
    }
    uint64_t sequence = 0;
    uint64_t frameEventCount = 0;
    if (!canonical_uint64(exact_record_value(lines[1], @"sequence"), &sequence) ||
        sequence != capture->sampleCount ||
        !canonical_uint64(exact_record_value(lines[2], @"event_count"), &frameEventCount) ||
        frameEventCount > kRuntimeTransitionCapacity ||
        frameEventCount > kMaximumRuntimeEvents - capture->eventCount ||
        lines.count != (NSUInteger)(4 + frameEventCount)) {
        return false;
    }

    NSArray<NSString *> *sample = [lines[3] componentsSeparatedByString:@"\t"];
    if (sample.count != 36 || ![sample[0] isEqualToString:@"sample"]) {
        return false;
    }
    uint64_t current[35] = {0};
    for (NSUInteger index = 0; index < 35; index++) {
        if (index == 33) {
            if (!is_runtime_lifecycle(sample[index + 1], &current[index])) {
                return false;
            }
        } else if ((index >= 14 && index <= 19) || index == 23) {
            if (!canonical_bool_digit(sample[index + 1], &current[index])) {
                return false;
            }
        } else if (!canonical_uint64(sample[index + 1], &current[index])) {
            return false;
        }
    }

    static const NSUInteger monotonic[] = {
        1, 2, 3, 4, 6, 7, 8, 9, 10, 11, 12, 13,
        24, 25, 26, 27, 32, 34,
    };
    if (current[0] == 0 || (capture->hasSample && current[0] < capture->lastContinuousNS) ||
        current[5] > 2 || current[6] > 2 || current[9] > 2 || current[5] > current[6] ||
        current[21] > current[20] || current[22] > current[20] - current[21] ||
        (capture->hasSample && !lifecycle_transition_is_valid(
            capture->previousSample[33], current[33]))) {
        return false;
    }
    if (capture->hasSample) {
        for (NSUInteger index = 0; index < sizeof(monotonic) / sizeof(monotonic[0]); index++) {
            NSUInteger field = monotonic[index];
            if (current[field] < capture->previousSample[field]) {
                return false;
            }
        }
    }

    NSString *samplePayload = [[sample subarrayWithRange:NSMakeRange(1, 35)]
        componentsJoinedByString:@"\t"];
    NSString *sampleRow = [NSString stringWithFormat:@"%llu\t%@\n",
        (unsigned long long)sequence, samplePayload];
    if (!append_bounded(capture->samples, sampleRow, kMaximumRuntimeSampleBytes)) {
        return false;
    }

    for (uint64_t index = 0; index < frameEventCount; index++) {
        NSArray<NSString *> *event = [lines[(NSUInteger)(4 + index)]
            componentsSeparatedByString:@"\t"];
        uint64_t eventSequence = 0;
        uint64_t eventContinuousNS = 0;
        uint64_t generation = 0;
        uint64_t aux0 = 0;
        uint64_t aux1 = 0;
        if (event.count != 7 || ![event[0] isEqualToString:@"event"] ||
            !canonical_uint64(event[1], &eventSequence) ||
            eventSequence != capture->eventCount ||
            !canonical_uint64(event[2], &eventContinuousNS) || eventContinuousNS == 0 ||
            (capture->hasEvent && eventContinuousNS < capture->lastEventContinuousNS) ||
            !is_runtime_event_kind(event[3]) ||
            !canonical_uint64(event[4], &generation) ||
            !canonical_uint64(event[5], &aux0) ||
            !canonical_uint64(event[6], &aux1) ||
            eventContinuousNS > current[0] || generation > current[1] ||
            !runtime_event_aux_is_valid(event[3], aux0, aux1)) {
            return false;
        }
        NSString *eventRow = [NSString stringWithFormat:@"%llu\t%llu\t%@\t%llu\t%llu\t%llu\n",
            (unsigned long long)eventSequence, (unsigned long long)eventContinuousNS, event[3],
            (unsigned long long)generation, (unsigned long long)aux0,
            (unsigned long long)aux1];
        if (!append_bounded(capture->events, eventRow, kMaximumRuntimeEventBytes)) {
            return false;
        }
        capture->lastEventContinuousNS = eventContinuousNS;
        capture->hasEvent = true;
        capture->eventCount++;
        if ([event[3] isEqualToString:@"observer-failed"]) {
            capture->observedFailure = true;
        }
    }

    if (!capture->hasSample) {
        capture->firstContinuousNS = current[0];
    } else {
        if (capture->hasPendingSampleInterval &&
            (capture->pendingSampleIntervalNS < 750000000 ||
                capture->pendingSampleIntervalNS > 1250000000)) {
            return false;
        }
        capture->pendingSampleIntervalNS = current[0] - capture->lastContinuousNS;
        capture->hasPendingSampleInterval = true;
    }
    capture->lastContinuousNS = current[0];
    memcpy(capture->previousSample, current, sizeof(current));
    capture->hasSample = true;
    capture->sampleCount++;
    if (current[33] == 4 || current[34] != 0) {
        capture->observedFailure = true;
    }
    return true;
}

static bool parse_runtime_complete(
    NSData *data,
    RuntimeCapture *capture,
    NSString **status
) {
    NSArray<NSString *> *keys = @[
        @"schema", @"observer.started_continuous_ns", @"observer.ended_continuous_ns",
        @"observer.sample_count", @"observer.event_count", @"observer.status"
    ];
    NSDictionary<NSString *, NSString *> *records = parse_records(data, keys);
    uint64_t started = 0;
    uint64_t ended = 0;
    uint64_t samples = 0;
    uint64_t events = 0;
    if (records == nil ||
        ![records[@"schema"] isEqualToString:kRuntimeCompleteSchema] ||
        !canonical_uint64(records[@"observer.started_continuous_ns"], &started) ||
        !canonical_uint64(records[@"observer.ended_continuous_ns"], &ended) ||
        !canonical_uint64(records[@"observer.sample_count"], &samples) ||
        !canonical_uint64(records[@"observer.event_count"], &events) ||
        (![records[@"observer.status"] isEqualToString:@"complete"] &&
            ![records[@"observer.status"] isEqualToString:@"not-run"]) ||
        !capture->hasSample || started != capture->firstContinuousNS ||
        ended != capture->lastContinuousNS || samples != capture->sampleCount ||
        events != capture->eventCount ||
        ([records[@"observer.status"] isEqualToString:@"complete"] &&
            (capture->observedFailure ||
                (capture->previousSample[33] != 2 && capture->previousSample[33] != 3) ||
                capture->previousSample[3] > capture->previousSample[2] ||
                capture->previousSample[4] > capture->previousSample[3] ||
                capture->previousSample[8] > capture->previousSample[3] ||
                capture->previousSample[10] > capture->previousSample[1] ||
                capture->previousSample[11] > capture->previousSample[10] ||
                capture->previousSample[12] > capture->previousSample[11] ||
                capture->previousSample[12] < capture->previousSample[1] ||
                capture->previousSample[25] > capture->previousSample[24] ||
                capture->previousSample[26] > capture->previousSample[25] ||
                capture->previousSample[27] > capture->previousSample[24] ||
                capture->previousSample[34] != 0))) {
        return false;
    }
    *status = records[@"observer.status"];
    return true;
}

static bool initialize_runtime_capture(RuntimeCapture *capture) {
    capture->samples = [NSMutableData data];
    capture->events = [NSMutableData data];
    return append_bounded(capture->samples, kRuntimeSampleHeader, kMaximumRuntimeSampleBytes) &&
        append_bounded(capture->events, kRuntimeEventHeader, kMaximumRuntimeEventBytes);
}

static bool read_runtime_stream(
    int fd,
    RuntimeCapture *capture,
    NSString **status,
    NSDate *deadline
) {
    if (!initialize_runtime_capture(capture)) {
        return false;
    }
    while (!interrupted) {
        if (deadline != nil && [deadline timeIntervalSinceNow] <= 0) {
            return false;
        }
        NSData *frame = read_frame(fd);
        if (frame == nil) {
            return false;
        }
        NSString *text = [[NSString alloc] initWithData:frame encoding:NSUTF8StringEncoding];
        if ([text hasPrefix:@"schema\tspaceterm.acceptance.runtime-tick/v1\n"]) {
            if (!parse_runtime_tick(frame, capture)) {
                return false;
            }
        } else if ([text hasPrefix:@"schema\tspaceterm.acceptance.runtime-complete/v1\n"]) {
            return parse_runtime_complete(frame, capture, status);
        } else {
            return false;
        }
    }
    return false;
}

static bool token_path(audit_token_t *token, NSString **result) {
    char path[PROC_PIDPATHINFO_MAXSIZE] = {0};
    int length = proc_pidpath_audittoken(token, path, sizeof(path));
    if (length <= 0 || length >= (int)sizeof(path) || path[length] != '\0' ||
        strnlen(path, sizeof(path)) != (size_t)length) {
        return false;
    }
    *result = [[NSFileManager defaultManager]
        stringWithFileSystemRepresentation:path length:(NSUInteger)length];
    return *result != nil;
}

static bool mapped_executable_matches(
    audit_token_t *token,
    NSString *expected_path,
    const struct stat *expected_stat,
    const struct statfs *expected_fs
) {
    pid_t pid = audit_token_to_pid(*token);
    NSString *before = nil;
    if (!token_path(token, &before) || ![before isEqualToString:expected_path]) {
        return false;
    }
    uint64_t address = 0;
    bool matched = false;
    for (NSUInteger count = 0; count < 65536; count++) {
        struct proc_regionwithpathinfo region = {0};
        int size = proc_pidinfo(pid, PROC_PIDREGIONPATHINFO, address, &region, sizeof(region));
        if (size == 0) {
            break;
        }
        if (size != sizeof(region)) {
            return false;
        }
        size_t path_length = strnlen(region.prp_vip.vip_path, sizeof(region.prp_vip.vip_path));
        if (path_length == sizeof(region.prp_vip.vip_path)) {
            return false;
        }
        NSString *path = [[NSFileManager defaultManager]
            stringWithFileSystemRepresentation:region.prp_vip.vip_path length:path_length];
        if ([path isEqualToString:expected_path]) {
            const struct vnode_info *vnode = &region.prp_vip.vip_vi;
            if ((uint64_t)vnode->vi_stat.vst_ino != (uint64_t)expected_stat->st_ino ||
                (uint64_t)vnode->vi_stat.vst_dev != (uint64_t)expected_stat->st_dev ||
                memcmp(&vnode->vi_fsid, &expected_fs->f_fsid, sizeof(fsid_t)) != 0) {
                return false;
            }
            matched = true;
        }
        uint64_t next = region.prp_prinfo.pri_address + region.prp_prinfo.pri_size;
        if (region.prp_prinfo.pri_size == 0 || next <= address) {
            return false;
        }
        address = next;
    }
    NSString *after = nil;
    return matched && token_path(token, &after) && [after isEqualToString:before];
}

static bool live_signature_matches(
    audit_token_t *token,
    const Options *options,
    NSString *expected_path,
    NSString **live_cdhash,
    NSString **live_identifier,
    NSString **live_team
) {
    NSData *token_data = [NSData dataWithBytes:token length:sizeof(*token)];
    NSDictionary *attributes = @{(__bridge NSString *)kSecGuestAttributeAudit: token_data};
    SecCodeRef code = NULL;
    OSStatus status = SecCodeCopyGuestWithAttributes(
        NULL, (__bridge CFDictionaryRef)attributes, kSecCSDefaultFlags, &code);
    if (status != errSecSuccess || code == NULL) {
        return false;
    }
    CFErrorRef validation_error = NULL;
    status = SecCodeCheckValidityWithErrors(code, kSecCSStrictValidate, NULL, &validation_error);
    if (validation_error != NULL) CFRelease(validation_error);
    if (status != errSecSuccess) {
        CFRelease(code);
        return false;
    }
    CFDictionaryRef information_ref = NULL;
    status = SecCodeCopySigningInformation(
        code, kSecCSSigningInformation | kSecCSDynamicInformation, &information_ref);
    CFRelease(code);
    if (status != errSecSuccess || information_ref == NULL) {
        return false;
    }
    NSDictionary *information = CFBridgingRelease(information_ref);
    NSData *unique = information[(__bridge NSString *)kSecCodeInfoUnique];
    NSString *identifier = information[(__bridge NSString *)kSecCodeInfoIdentifier];
    NSString *team = information[(__bridge NSString *)kSecCodeInfoTeamIdentifier] ?: @"";
    NSURL *main_executable = information[(__bridge NSString *)kSecCodeInfoMainExecutable];
    NSString *main_path = canonical_path(main_executable.path);
    NSString *cdhash = [unique isKindOfClass:NSData.class] ? hex_data(unique) : nil;
    bool expects_no_team = [options->teamIdentifier isEqualToString:@"not-set"] ||
        [options->teamIdentifier isEqualToString:@"not set"];
    NSString *expected_team = expects_no_team
        ? @"" : options->teamIdentifier;
    if (![identifier isKindOfClass:NSString.class] || ![team isKindOfClass:NSString.class] ||
        ![cdhash isEqualToString:options->cdhash] ||
        ![identifier isEqualToString:options->identifier] ||
        ![team isEqualToString:expected_team] || ![main_path isEqualToString:expected_path]) {
        return false;
    }
    *live_cdhash = cdhash;
    *live_identifier = identifier;
    *live_team = team;
    return true;
}

static NSString *make_nonce(void) {
    uint8_t bytes[32];
    if (SecRandomCopyBytes(kSecRandomDefault, sizeof(bytes), bytes) != errSecSuccess) {
        return nil;
    }
    NSMutableString *nonce = [NSMutableString stringWithCapacity:64];
    for (NSUInteger index = 0; index < sizeof(bytes); index++) {
        [nonce appendFormat:@"%02x", bytes[index]];
    }
    return nonce;
}

static NSString *final_observation(
    NSDictionary<NSString *, NSString *> *response,
    int pidversion,
    const struct statfs *expected_fs,
    NSString *cdhash,
    NSString *identifier,
    NSString *team
) {
    NSString *fsid = [NSString stringWithFormat:@"%d:%d",
        expected_fs->f_fsid.val[0], expected_fs->f_fsid.val[1]];
    NSArray<NSArray<NSString *> *> *records = @[
        @[@"schema", response[@"schema"]],
        @[@"observation.source", response[@"observation.source"]],
        @[@"launch.nonce", response[@"launch.nonce"]],
        @[@"run.id", response[@"run.id"]],
        @[@"package.app.sha256", response[@"package.app.sha256"]],
        @[@"runtime.schema", response[@"runtime.schema"]],
        @[@"runtime.sample_interval_ms", response[@"runtime.sample_interval_ms"]],
        @[@"runtime.transition_capacity", response[@"runtime.transition_capacity"]],
        @[@"process.pid", response[@"process.pid"]],
        @[@"process.pidversion", [NSString stringWithFormat:@"%d", pidversion]],
        @[@"process.executable.path", response[@"process.executable.path"]],
        @[@"process.executable.device", response[@"process.executable.device"]],
        @[@"process.executable.inode", response[@"process.executable.inode"]],
        @[@"process.executable.fsid", fsid],
        @[@"process.signature.cdhash", cdhash],
        @[@"process.signature.identifier", identifier],
        @[@"process.signature.team_identifier", team],
        @[@"terminal_font_selected", response[@"terminal_font_selected"]],
        @[@"initial_grid.rows", response[@"initial_grid.rows"]],
        @[@"initial_grid.columns", response[@"initial_grid.columns"]],
        @[@"initial_grid.logical_width", response[@"initial_grid.logical_width"]],
        @[@"initial_grid.logical_height", response[@"initial_grid.logical_height"]],
        @[@"initial_grid.backing_pixel_width", response[@"initial_grid.backing_pixel_width"]],
        @[@"initial_grid.backing_pixel_height", response[@"initial_grid.backing_pixel_height"]],
        @[@"observation.complete", @"true"],
    ];
    NSMutableString *result = [NSMutableString string];
    for (NSArray<NSString *> *record in records) {
        [result appendFormat:@"%@\t%@\n", record[0], encode_value(record[1])];
    }
    return result;
}

static NSString *lower_sha256(NSData *data) {
    uint8_t digest[CC_SHA256_DIGEST_LENGTH];
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
    if (CC_SHA256(data.bytes, (CC_LONG)data.length, digest) == NULL) {
        return nil;
    }
#pragma clang diagnostic pop
    NSMutableString *result = [NSMutableString stringWithCapacity:CC_SHA256_DIGEST_LENGTH * 2];
    for (NSUInteger index = 0; index < CC_SHA256_DIGEST_LENGTH; index++) {
        [result appendFormat:@"%02x", digest[index]];
    }
    return result;
}

static NSData *runtime_metadata(
    const Options *options,
    NSDictionary<NSString *, NSString *> *response,
    RuntimeCapture *capture,
    NSString *status,
    NSString *samplesSHA256,
    NSString *eventsSHA256
) {
    NSArray<NSArray<NSString *> *> *records = @[
        @[@"schema", @"spaceterm.acceptance.runtime-observation-metadata/v1"],
        @[@"observation.source", @"production-app"],
        @[@"run.id", options->runID],
        @[@"package.app.sha256", options->appSHA256],
        @[@"process.pid", response[@"process.pid"]],
        @[@"runtime.samples.path", @"runtime-samples.tsv"],
        @[@"runtime.samples.sha256", samplesSHA256],
        @[@"runtime.events.path", @"runtime-events.tsv"],
        @[@"runtime.events.sha256", eventsSHA256],
        @[@"observer.started_continuous_ns", [NSString stringWithFormat:@"%llu",
            (unsigned long long)capture->firstContinuousNS]],
        @[@"observer.ended_continuous_ns", [NSString stringWithFormat:@"%llu",
            (unsigned long long)capture->lastContinuousNS]],
        @[@"observer.sample_interval_ms", @"1000"],
        @[@"observer.transition_capacity", @"64"],
        @[@"observer.sample_count", [NSString stringWithFormat:@"%llu",
            (unsigned long long)capture->sampleCount]],
        @[@"observer.event_count", [NSString stringWithFormat:@"%llu",
            (unsigned long long)capture->eventCount]],
        @[@"observer.status", status],
        @[@"observation.complete", [status isEqualToString:@"complete"] ? @"true" : @"false"],
    ];
    NSMutableString *result = [NSMutableString string];
    for (NSArray<NSString *> *record in records) {
        [result appendFormat:@"%@\t%@\n", record[0], encode_value(record[1])];
    }
    return [result dataUsingEncoding:NSUTF8StringEncoding];
}

static bool publish_exclusive(NSString *output, NSData *data, bool *published) {
    *published = false;
    NSString *parent = output.stringByDeletingLastPathComponent;
    struct stat parent_stat = {0};
    if (lstat(parent.fileSystemRepresentation, &parent_stat) != 0 ||
        !S_ISDIR(parent_stat.st_mode) || S_ISLNK(parent_stat.st_mode) ||
        parent_stat.st_uid != geteuid() || (parent_stat.st_mode & 077) != 0) {
        return report(@"output parent is not an owner-private real directory");
    }
    NSString *temporary = [parent stringByAppendingPathComponent:
        [NSString stringWithFormat:@".%@.%d.tmp", output.lastPathComponent, getpid()]];
    int fd = open(temporary.fileSystemRepresentation,
        O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC | O_NOFOLLOW, 0600);
    if (fd < 0) {
        return report(@"could not exclusively create temporary observation");
    }
    bool success = write_all(fd, data.bytes, data.length) && fsync(fd) == 0;
    int close_result = close(fd);
    fd = -1;
    success = success && close_result == 0;
    if (!success) {
        unlink(temporary.fileSystemRepresentation);
        return report(@"could not durably write observation");
    }
    if (renamex_np(temporary.fileSystemRepresentation, output.fileSystemRepresentation,
            RENAME_EXCL) != 0) {
        unlink(temporary.fileSystemRepresentation);
        return report(@"observation output already exists or could not be published");
    }
    *published = true;
    int directory = open(parent.fileSystemRepresentation, O_RDONLY | O_CLOEXEC | O_DIRECTORY);
    bool directory_synced = directory >= 0 && fsync(directory) == 0;
    int directory_close_result = directory >= 0 ? close(directory) : -1;
    if (!directory_synced || directory_close_result != 0) {
        return report(@"could not durably publish observation directory entry");
    }
    return true;
}

static bool output_is_absent(NSString *path) {
    struct stat status = {0};
    if (lstat(path.fileSystemRepresentation, &status) == 0) {
        return false;
    }
    return errno == ENOENT;
}

static void terminate_exact_application(NSRunningApplication *application) {
    if (application == nil || application.terminated) {
        return;
    }
    [application terminate];
    NSDate *deadline = [NSDate dateWithTimeIntervalSinceNow:5.0];
    while (!application.terminated && [deadline timeIntervalSinceNow] > 0) {
        [NSThread sleepForTimeInterval:0.05];
    }
    if (!application.terminated) {
        [application forceTerminate];
        deadline = [NSDate dateWithTimeIntervalSinceNow:5.0];
        while (!application.terminated && [deadline timeIntervalSinceNow] > 0) {
            [NSThread sleepForTimeInterval:0.05];
        }
    }
}

static int run_verifier(const Options *options) {
    int result = 1;
    int executable_fd = -1;
    int listener = -1;
    int peer = -1;
    char socket_directory[] = "/tmp/spaceterm-acceptance.XXXXXX";
    NSString *socket_path = nil;
    NSRunningApplication *application = nil;
    NSString *expected_app = nil;
    NSString *expected_path = nil;
    NSMutableDictionary<NSString *, NSString *> *environment = nil;
    NSWorkspaceOpenConfiguration *configuration = nil;
    __block NSRunningApplication *launched_application = nil;
    __block NSError *launch_error = nil;
    dispatch_semaphore_t launched = dispatch_semaphore_create(0);
    void (^launch_completion)(NSRunningApplication *, NSError *) =
        ^(NSRunningApplication *opened, NSError *error) {
            launched_application = opened;
            launch_error = error;
            dispatch_semaphore_signal(launched);
        };
    NSString *opened_app = nil;
    NSString *opened_executable = nil;
    NSString *live_cdhash = nil;
    NSString *live_identifier = nil;
    NSString *live_team = nil;
    NSString *nonce = nil;
    NSData *response_data = nil;
    NSDictionary<NSString *, NSString *> *response = nil;
    NSString *observation = nil;
    RuntimeCapture runtime = {0};
    NSString *runtime_status = nil;
    NSString *runtime_samples_path = nil;
    NSString *runtime_events_path = nil;
    NSString *runtime_metadata_path = nil;
    NSString *samples_sha256 = nil;
    NSString *events_sha256 = nil;
    NSData *metadata = nil;
    NSData *observation_data = nil;
    NSData *ack = nil;
    NSString *closed = nil;
    NSDate *runtime_deadline = nil;
    NSString *output_parent = nil;
    audit_token_t final_token = INVALID_AUDIT_TOKEN_VALUE;
    socklen_t final_token_length = sizeof(final_token);
    bool samples_published = false;
    bool events_published = false;
    bool metadata_published = false;
    bool observation_published = false;

    expected_app = canonical_path(options->app);
    expected_path = canonical_path(options->executable);
    if (expected_app == nil || expected_path == nil ||
        ![expected_path hasPrefix:[expected_app stringByAppendingString:@"/"]]) {
        report(@"application or executable path is invalid");
        goto cleanup;
    }
    output_parent = options->output.stringByDeletingLastPathComponent;
    runtime_samples_path = [output_parent stringByAppendingPathComponent:@"runtime-samples.tsv"];
    runtime_events_path = [output_parent stringByAppendingPathComponent:@"runtime-events.tsv"];
    runtime_metadata_path = [output_parent stringByAppendingPathComponent:@"runtime-metadata.tsv"];
    if (!output_is_absent(options->output) || !output_is_absent(runtime_samples_path) ||
        !output_is_absent(runtime_events_path) || !output_is_absent(runtime_metadata_path)) {
        report(@"runtime observation outputs must not already exist");
        goto cleanup;
    }
    executable_fd = open(expected_path.fileSystemRepresentation,
        O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    struct stat expected_stat = {0};
    struct statfs expected_fs = {0};
    if (executable_fd < 0 || fstat(executable_fd, &expected_stat) != 0 ||
        fstatfs(executable_fd, &expected_fs) != 0 || !S_ISREG(expected_stat.st_mode) ||
        (expected_fs.f_flags & MNT_RDONLY) == 0) {
        report(@"expected executable is not a regular file on a read-only mount");
        goto cleanup;
    }
    if (mkdtemp(socket_directory) == NULL || chmod(socket_directory, 0700) != 0) {
        report(@"could not create private socket directory");
        goto cleanup;
    }
    socket_path = [[NSString stringWithUTF8String:socket_directory]
        stringByAppendingPathComponent:@"peer.sock"];
    listener = socket(AF_UNIX, SOCK_STREAM, 0);
    if (listener < 0 || !set_close_on_exec(listener)) {
        report(@"could not create close-on-exec Unix listener");
        goto cleanup;
    }
    struct sockaddr_un address = {0};
    address.sun_family = AF_UNIX;
    const char *socket_bytes = socket_path.fileSystemRepresentation;
    if (strlen(socket_bytes) >= sizeof(address.sun_path)) {
        report(@"private Unix socket path is too long");
        goto cleanup;
    }
    strlcpy(address.sun_path, socket_bytes, sizeof(address.sun_path));
    if (bind(listener, (struct sockaddr *)&address, sizeof(address)) != 0 ||
        chmod(socket_bytes, 0600) != 0 || listen(listener, 1) != 0) {
        report(@"could not bind private Unix listener");
        goto cleanup;
    }

    // Do not forward the verifier's environment into the app or its Shell Process. LaunchServices
    // supplies the normal GUI environment; only the clean campaign HOME and socket capability are
    // explicit additions, and the app removes the socket variable before starting GPUI.
    environment = [NSMutableDictionary dictionary];
    environment[@"SPACETERM_ACCEPTANCE_SOCKET"] = socket_path;
    environment[@"HOME"] = options->home;
    configuration = [NSWorkspaceOpenConfiguration configuration];
    configuration.createsNewApplicationInstance = YES;
    configuration.allowsRunningApplicationSubstitution = NO;
    configuration.addsToRecentItems = NO;
    configuration.activates = YES;
    configuration.promptsUserIfNeeded = NO;
    configuration.environment = environment;
    [[NSWorkspace sharedWorkspace]
        openApplicationAtURL:[NSURL fileURLWithPath:expected_app isDirectory:YES]
        configuration:configuration
        completionHandler:launch_completion];
    if (!wait_for_launch(launched)) {
        // NSWorkspace offers no cancellation. Give a bounded late completion a chance to return
        // the exact instance so cleanup can terminate it; never hang a signal trap indefinitely.
        if (dispatch_semaphore_wait(
                launched, dispatch_time(DISPATCH_TIME_NOW, 5 * NSEC_PER_SEC)) == 0) {
            application = launched_application;
        }
        report(interrupted ? @"LaunchServices launch was interrupted" :
            @"LaunchServices launch timed out");
        goto cleanup;
    }
    if (launch_error != nil || launched_application == nil ||
        launched_application.processIdentifier <= 0) {
        report(@"LaunchServices did not launch the exact application");
        goto cleanup;
    }
    application = launched_application;
    opened_app = canonical_path(application.bundleURL.path);
    opened_executable = canonical_path(application.executableURL.path);
    if (![opened_app isEqualToString:expected_app] || ![opened_executable isEqualToString:expected_path]) {
        report(@"LaunchServices substituted a different application or executable");
        goto cleanup;
    }
    if (!wait_for_fd(listener, false, kProofTimeoutSeconds)) {
        report(@"production app did not connect to the verifier");
        goto cleanup;
    }
    peer = accept(listener, NULL, NULL);
    if (peer < 0 || !set_close_on_exec(peer)) {
        report(@"could not accept a close-on-exec application peer");
        goto cleanup;
    }
    audit_token_t token = INVALID_AUDIT_TOKEN_VALUE;
    socklen_t token_length = sizeof(token);
    if (getsockopt(peer, SOL_LOCAL, LOCAL_PEERTOKEN, &token, &token_length) != 0 ||
        token_length != sizeof(token)) {
        report(@"Unix peer has no valid audit token");
        goto cleanup;
    }
    pid_t peer_pid = audit_token_to_pid(token);
    int pidversion = audit_token_to_pidversion(token);
    if (peer_pid != application.processIdentifier || pidversion <= 0 ||
        audit_token_to_euid(token) != geteuid() ||
        !mapped_executable_matches(&token, expected_path, &expected_stat, &expected_fs)) {
        report(@"Unix peer is not the exact mounted application process");
        goto cleanup;
    }
    if (!live_signature_matches(&token, options, expected_path,
            &live_cdhash, &live_identifier, &live_team)) {
        report(@"Unix peer live signature does not match the mounted application");
        goto cleanup;
    }
    nonce = make_nonce();
    if (nonce == nil || !write_frame(peer, challenge_data(nonce, options))) {
        report(@"could not send the authenticated launch challenge");
        goto cleanup;
    }
    response_data = read_frame(peer);
    response = validate_response(
        response_data, nonce, options, peer_pid, expected_path, &expected_stat);
    if (response == nil || !mapped_executable_matches(
            &token, expected_path, &expected_stat, &expected_fs)) {
        report(@"production app returned an invalid or stale launch observation");
        goto cleanup;
    }
    if (options->replay) {
        [application terminate];
    } else {
        fprintf(stderr, "authenticated mounted app is ready; quit it after acceptance completes\n");
    }
    runtime_deadline = [NSDate dateWithTimeIntervalSinceNow:
        options->replay ? kProofTimeoutSeconds : 12.0 * 60.0 * 60.0 + kProofTimeoutSeconds];

    if (!read_runtime_stream(peer, &runtime, &runtime_status, runtime_deadline)) {
        report(interrupted ? @"runtime observation was interrupted" :
            @"production app returned an invalid runtime observation");
        goto cleanup;
    }

    if (getsockopt(peer, SOL_LOCAL, LOCAL_PEERTOKEN, &final_token, &final_token_length) != 0 ||
        final_token_length != sizeof(final_token) ||
        audit_token_to_pid(final_token) != peer_pid ||
        audit_token_to_pidversion(final_token) != pidversion ||
        audit_token_to_euid(final_token) != geteuid() ||
        !mapped_executable_matches(&final_token, expected_path, &expected_stat, &expected_fs) ||
        !live_signature_matches(&final_token, options, expected_path,
            &live_cdhash, &live_identifier, &live_team)) {
        report(@"runtime observation peer was no longer the live mounted application");
        goto cleanup;
    }

    samples_sha256 = lower_sha256(runtime.samples);
    events_sha256 = lower_sha256(runtime.events);
    if (!is_lower_hex(samples_sha256, 64) || !is_lower_hex(events_sha256, 64)) {
        report(@"runtime observation checksums could not be computed");
        goto cleanup;
    }
    metadata = runtime_metadata(
        options, response, &runtime, runtime_status, samples_sha256, events_sha256);
    observation = final_observation(
        response, pidversion, &expected_fs, live_cdhash, live_identifier, live_team);
    observation_data = [observation dataUsingEncoding:NSUTF8StringEncoding];
    if (metadata == nil || observation_data == nil ||
        !publish_exclusive(runtime_samples_path, runtime.samples, &samples_published)) {
        goto cleanup;
    }
    if (!publish_exclusive(runtime_events_path, runtime.events, &events_published)) {
        goto cleanup;
    }
    if (!publish_exclusive(runtime_metadata_path, metadata, &metadata_published)) {
        goto cleanup;
    }
    ack = [[NSString stringWithFormat:@"schema\t%@\nstatus\taccepted\n", kRuntimeAckSchema]
        dataUsingEncoding:NSUTF8StringEncoding];
    if (ack == nil || !write_frame(peer, ack)) {
        report(@"runtime observation acknowledgement could not be delivered");
        goto cleanup;
    }
    response_data = read_frame(peer);
    closed = [[NSString alloc] initWithData:response_data encoding:NSUTF8StringEncoding];
    if (![closed isEqualToString:[NSString stringWithFormat:
            @"schema\t%@\nstatus\tconfirmed\n", kRuntimeClosedSchema]]) {
        report(@"runtime observation closure was not confirmed");
        goto cleanup;
    }
    uint8_t trailing = 0;
    if (!wait_for_fd(peer, false, kProofTimeoutSeconds) ||
        read(peer, &trailing, sizeof(trailing)) != 0) {
        report(@"runtime observation stream did not close cleanly");
        goto cleanup;
    }
    close(peer);
    peer = -1;
    terminate_exact_application(application);
    if (!application.terminated) {
        report(@"observed application could not finish safely");
        goto cleanup;
    }
    // The native observation is the commit marker. Publish it only after the app consumed the
    // acknowledgement, closed the authenticated stream, and terminated cleanly.
    if (!publish_exclusive(options->output, observation_data, &observation_published)) {
        goto cleanup;
    }
    result = 0;

cleanup:
    if (result != 0) {
        terminate_exact_application(application);
        if (observation_published) unlink(options->output.fileSystemRepresentation);
        if (metadata_published) unlink(runtime_metadata_path.fileSystemRepresentation);
        if (events_published) unlink(runtime_events_path.fileSystemRepresentation);
        if (samples_published) unlink(runtime_samples_path.fileSystemRepresentation);
    }
    if (peer >= 0) close(peer);
    if (listener >= 0) close(listener);
    if (executable_fd >= 0) close(executable_fd);
    if (socket_path != nil) unlink(socket_path.fileSystemRepresentation);
    rmdir(socket_directory);
    return result;
}

static NSMutableArray<NSString *> *self_test_sample(
    NSString *continuousNS,
    NSString *lifecycle
) {
    return [@[
        continuousNS, @"1", @"1", @"1", @"0", @"0", @"1", @"1", @"1", @"1",
        @"1", @"1", @"1", @"1", @"1", @"0", @"0", @"1", @"1", @"0", @"24",
        @"24", @"0", @"0", @"1", @"1", @"1", @"0", @"24", @"80", @"640",
        @"384", @"0", lifecycle, @"0"
    ] mutableCopy];
}

static NSData *self_test_tick(
    NSString *sequence,
    NSString *declaredEventCount,
    NSArray<NSString *> *sample,
    NSArray<NSString *> *events
) {
    NSMutableString *frame = [NSMutableString stringWithFormat:
        @"schema\t%@\nsequence\t%@\nevent_count\t%@\nsample\t%@\n",
        kRuntimeTickSchema, sequence, declaredEventCount,
        [sample componentsJoinedByString:@"\t"]];
    for (NSString *event in events) {
        [frame appendFormat:@"event\t%@\n", event];
    }
    return [frame dataUsingEncoding:NSUTF8StringEncoding];
}

static NSData *self_test_complete(
    NSString *started,
    NSString *ended,
    NSString *samples,
    NSString *events,
    NSString *status
) {
    NSString *frame = [NSString stringWithFormat:
        @"schema\t%@\nobserver.started_continuous_ns\t%@\n"
         "observer.ended_continuous_ns\t%@\nobserver.sample_count\t%@\n"
         "observer.event_count\t%@\nobserver.status\t%@\n",
        kRuntimeCompleteSchema, started, ended, samples, events, status];
    return [frame dataUsingEncoding:NSUTF8StringEncoding];
}

static bool self_test_rejects_tick(NSData *frame) {
    RuntimeCapture capture = {0};
    return initialize_runtime_capture(&capture) && !parse_runtime_tick(frame, &capture);
}

static bool self_test_failure(NSString *name) {
    return report([NSString stringWithFormat:@"self-test failed: %@", name]);
}

static int verifier_self_test(void) {
    NSArray<NSString *> *emptyEvents = @[];
    NSMutableArray<NSString *> *sample = self_test_sample(@"1000000000", @"running");
    NSData *base = self_test_tick(@"0", @"0", sample, emptyEvents);

    NSString *unknownSchemaText = [[NSString alloc] initWithData:base
        encoding:NSUTF8StringEncoding];
    unknownSchemaText = [unknownSchemaText stringByReplacingOccurrencesOfString:
        kRuntimeTickSchema withString:@"spaceterm.acceptance.runtime-tick/unknown"];
    if (!self_test_rejects_tick(
            [unknownSchemaText dataUsingEncoding:NSUTF8StringEncoding])) {
        return self_test_failure(@"unknown schema") ? 0 : 1;
    }
    if (!self_test_rejects_tick(self_test_tick(@"00", @"0", sample, emptyEvents)) ||
        !self_test_rejects_tick(self_test_tick(@"18446744073709551616", @"0",
            sample, emptyEvents))) {
        return self_test_failure(@"noncanonical or overflowing integer") ? 0 : 1;
    }

    NSMutableArray<NSString *> *invalid = [sample mutableCopy];
    invalid[14] = @"2";
    if (!self_test_rejects_tick(self_test_tick(@"0", @"0", invalid, emptyEvents))) {
        return self_test_failure(@"boolean") ? 0 : 1;
    }
    invalid = [sample mutableCopy];
    invalid[33] = @"running-canary";
    if (!self_test_rejects_tick(self_test_tick(@"0", @"0", invalid, emptyEvents))) {
        return self_test_failure(@"lifecycle content canary") ? 0 : 1;
    }
    invalid = [sample mutableCopy];
    invalid[1] = @"1-canary";
    if (!self_test_rejects_tick(self_test_tick(@"0", @"0", invalid, emptyEvents))) {
        return self_test_failure(@"numeric content canary") ? 0 : 1;
    }
    invalid = [sample mutableCopy];
    invalid[6] = @"3";
    if (!self_test_rejects_tick(self_test_tick(@"0", @"0", invalid, emptyEvents))) {
        return self_test_failure(@"event queue high-water") ? 0 : 1;
    }
    invalid = [sample mutableCopy];
    invalid[9] = @"3";
    if (!self_test_rejects_tick(self_test_tick(@"0", @"0", invalid, emptyEvents))) {
        return self_test_failure(@"UI drain high-water") ? 0 : 1;
    }
    if (!self_test_rejects_tick(self_test_tick(@"0", @"65", sample, emptyEvents)) ||
        !self_test_rejects_tick(self_test_tick(@"0", @"1", sample, emptyEvents))) {
        return self_test_failure(@"transition capacity or count mismatch") ? 0 : 1;
    }
    if (!self_test_rejects_tick(self_test_tick(@"0", @"1", sample,
            @[@"0\t1000000000\tunknown-canary\t1\t0\t0"]))) {
        return self_test_failure(@"event kind content canary") ? 0 : 1;
    }
    RuntimeCapture bounded = {0};
    if (!initialize_runtime_capture(&bounded)) {
        return self_test_failure(@"capture initialization") ? 0 : 1;
    }
    bounded.sampleCount = kMaximumRuntimeSamples;
    if (parse_runtime_tick(base, &bounded)) {
        return self_test_failure(@"sample count bound") ? 0 : 1;
    }
    RuntimeCapture eventBounded = {0};
    if (!initialize_runtime_capture(&eventBounded)) {
        return self_test_failure(@"event capture initialization") ? 0 : 1;
    }
    eventBounded.eventCount = kMaximumRuntimeEvents;
    if (parse_runtime_tick(self_test_tick(@"0", @"1", sample,
            @[@"65536\t1000000000\tvisibility-lost\t1\t0\t0"]), &eventBounded)) {
        return self_test_failure(@"event count bound") ? 0 : 1;
    }
    NSMutableData *boundedBytes = [NSMutableData dataWithLength:1];
    if (append_bounded(boundedBytes, @"x", 1)) {
        return self_test_failure(@"byte bound") ? 0 : 1;
    }

    RuntimeCapture regression = {0};
    if (!initialize_runtime_capture(&regression) || !parse_runtime_tick(base, &regression) ||
        parse_runtime_tick(self_test_tick(@"0", @"0", sample, emptyEvents), &regression) ||
        parse_runtime_tick(self_test_tick(@"1", @"0",
            self_test_sample(@"999999999", @"running"), emptyEvents), &regression)) {
        return self_test_failure(@"sequence or time regression") ? 0 : 1;
    }
    RuntimeCapture cadence = {0};
    if (!initialize_runtime_capture(&cadence) ||
        !parse_runtime_tick(base, &cadence) ||
        !parse_runtime_tick(self_test_tick(@"1", @"0",
            self_test_sample(@"1500000000", @"running"), emptyEvents), &cadence) ||
        parse_runtime_tick(self_test_tick(@"2", @"0",
            self_test_sample(@"1500000000", @"exited"), emptyEvents), &cadence)) {
        return self_test_failure(@"sample cadence") ? 0 : 1;
    }

    RuntimeCapture golden = {0};
    NSString *status = nil;
    if (!initialize_runtime_capture(&golden) ||
        !parse_runtime_tick(base, &golden) ||
        !parse_runtime_tick(self_test_tick(@"1", @"1",
            self_test_sample(@"2000000000", @"running"),
            @[@"0\t1500000000\tvisibility-lost\t1\t0\t0"]), &golden) ||
        !parse_runtime_tick(self_test_tick(@"2", @"1",
            self_test_sample(@"2000000000", @"exited"),
            @[@"1\t2000000000\tsession-exited\t1\t1\t0"]), &golden) ||
        !parse_runtime_complete(self_test_complete(@"1000000000", @"2000000000",
            @"3", @"2", @"complete"), &golden, &status) ||
        ![status isEqualToString:@"complete"]) {
        return self_test_failure(@"golden stream") ? 0 : 1;
    }
    RuntimeCapture nonterminal = {0};
    status = nil;
    if (!initialize_runtime_capture(&nonterminal) || !parse_runtime_tick(base, &nonterminal) ||
        parse_runtime_complete(self_test_complete(@"1000000000", @"1000000000",
            @"1", @"0", @"complete"), &nonterminal, &status)) {
        return self_test_failure(@"nonterminal completion") ? 0 : 1;
    }
    invalid = self_test_sample(@"1000000000", @"observer-failed");
    invalid[34] = @"1";
    RuntimeCapture dropped = {0};
    status = nil;
    if (!initialize_runtime_capture(&dropped) ||
        !parse_runtime_tick(self_test_tick(@"0", @"0", invalid, emptyEvents), &dropped) ||
        parse_runtime_complete(self_test_complete(@"1000000000", @"1000000000",
            @"1", @"0", @"complete"), &dropped, &status)) {
        return self_test_failure(@"observer drops completion") ? 0 : 1;
    }
    return 0;
}

int main(int argc, const char *argv[]) {
    @autoreleasepool {
        signal(SIGPIPE, SIG_IGN);
        signal(SIGINT, handle_signal);
        signal(SIGTERM, handle_signal);
        if (argc == 2 && strcmp(argv[1], "--self-test") == 0) {
            return verifier_self_test();
        }
        Options options = {0};
        if (!parse_options(argc, argv, &options)) {
            return 64;
        }
        return run_verifier(&options);
    }
}
