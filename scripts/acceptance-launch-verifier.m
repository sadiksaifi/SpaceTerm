#import <AppKit/AppKit.h>
#import <CommonCrypto/CommonDigest.h>
#import <CommonCrypto/CommonHMAC.h>
#import <Foundation/Foundation.h>
#import <Security/Security.h>

#include <bsm/libbsm.h>
#include <errno.h>
#include <fcntl.h>
#include <libproc.h>
#include <limits.h>
#include <mach/message.h>
#include <mach/mach_time.h>
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
static const uint64_t kMaximumFailureActions = 64;
static const NSUInteger kMaximumFailureActionBytes = 256 * 1024;
static const NSTimeInterval kProofTimeoutSeconds = 30.0;
static volatile sig_atomic_t interrupted = 0;
static NSString *runtime_parse_diagnostic = nil;

static NSString *make_nonce(void);
static NSString *lower_sha256(NSData *data);
static bool publish_exclusive(NSString *output, NSData *data, bool *published);

static NSString *const kRuntimeSchema = @"spaceterm.acceptance.runtime-stream/v1";
static NSString *const kRuntimeTickSchema = @"spaceterm.acceptance.runtime-tick/v1";
static NSString *const kRuntimeCompleteSchema = @"spaceterm.acceptance.runtime-complete/v1";
static NSString *const kRuntimeAckSchema = @"spaceterm.acceptance.runtime-ack/v1";
static NSString *const kRuntimeClosedSchema = @"spaceterm.acceptance.runtime-closed/v1";
static NSString *const kFailureActionSchema = @"spaceterm.acceptance.failure-action/v1";
static NSString *const kFailureActionResultSchema =
    @"spaceterm.acceptance.failure-action-result/v2";
static NSString *const kAXSubjectSchema = @"spaceterm.acceptance.ax-subject/v1";

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
static NSString *const kFailureActionHeader =
    @"request_id\tsequence\tcase_id\taction\tresult\tpane_id\tpane_state\t"
     "failure_class\tfailure_recoverability\tfailure_operation\tstate_revision\t"
     "latest_generation\tlast_valid_generation\tvisible_generation\tpending_recovery\t"
     "terminal_input_usable\tsession_attached\tresource_staged_count\t"
     "resource_staged_bytes\tresource_rolled_back_count\tresource_rolled_back_bytes\n";

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
    __strong NSString *failureControl;
    __strong NSString *quitControl;
    __strong NSString *quitReceipt;
    __strong NSString *tailReceipt;
    __strong NSString *campaignSecret;
    __strong NSString *runIntent;
    __strong NSString *subjectExitReceipt;
    bool replay;
    bool externalLifecycle;
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

typedef struct {
    bool requested;
    uint64_t requestContinuousNS;
    __strong NSString *token;
    __strong NSString *campaignID;
    __strong NSString *sessionID;
    __strong NSString *campaignNonce;
    __strong NSString *runIntentSHA256;
    __strong NSString *subject;
} QuitCapture;

static bool validate_tail_receipt(
    const Options *options,
    NSString *clientToken,
    pid_t pid,
    uint64_t startSeconds,
    uint64_t startMicroseconds,
    NSString **campaignID,
    NSString **sessionID,
    NSString **campaignNonce,
    NSString **runIntentSHA256,
    NSString **subject);
static bool strict_normal_terminate(NSRunningApplication *application);
static uint64_t continuous_nanoseconds(void);
static bool mapped_executable_matches(
    audit_token_t *token,
    NSString *expected_path,
    const struct stat *expected_stat,
    const struct statfs *expected_fs);
static bool live_signature_matches(
    audit_token_t *token,
    const Options *options,
    NSString *expected_path,
    NSString **live_cdhash,
    NSString **live_identifier,
    NSString **live_team);

typedef struct {
    __strong NSMutableData *records;
    __strong NSString *pendingRequestID;
    __strong NSString *pendingCaseID;
    __strong NSString *pendingClientToken;
    uint64_t pendingSequence;
    uint64_t nextSequence;
    uint64_t resultCount;
    uint64_t pendingPaneID;
    uint64_t lastStateRevision;
    uint64_t injectedLatest;
    uint64_t injectedLastValid;
    uint64_t injectedVisible;
    uint64_t injectedResourceCount;
    uint64_t injectedResourceBytes;
    NSUInteger pendingPhase;
} FailureCapture;

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
    if (argc != 23 && argc != 25) {
        return report(@"expected eleven or twelve named option/value pairs");
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
        @"--identifier", @"--team-identifier", @"--home", @"--output", @"--mode",
        @"--failure-control"
    ];
    for (NSString *key in keys) {
        if (values[key] == nil) {
            return report([NSString stringWithFormat:@"missing option %@", key]);
        }
    }
    bool externalLifecycle = values[@"--external-lifecycle"] != nil;
    if (values.count != keys.count + (externalLifecycle ? 1 : 0)) {
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
    NSString *failureControl = values[@"--failure-control"];
    if (![failureControl isEqualToString:@"none"] && !failureControl.isAbsolutePath) {
        return report(@"failure control must be none or an absolute path");
    }
    if ([mode isEqualToString:@"replay"] && ![failureControl isEqualToString:@"none"]) {
        return report(@"failure control is unavailable during replay");
    }
    if (externalLifecycle && ([mode isEqualToString:@"replay"] ||
            ![failureControl isEqualToString:@"none"])) {
        return report(@"external lifecycle is campaign-only and failure control must be none");
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
    options->failureControl = failureControl;
    options->quitControl = @"none";
    options->quitReceipt = @"none";
    options->tailReceipt = @"none";
    options->campaignSecret = @"none";
    options->runIntent = @"none";
    options->subjectExitReceipt = @"none";
    options->externalLifecycle = externalLifecycle &&
        [values[@"--external-lifecycle"] isEqualToString:@"true"];
    if (externalLifecycle && !options->externalLifecycle) {
        return report(@"external lifecycle must be true");
    }
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

static bool launch_completion_permits_peer(
    bool delivered,
    bool failed,
    pid_t completion_pid,
    pid_t peer_pid
) {
    return !delivered || (!failed && completion_pid > 0 && completion_pid == peer_pid);
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

static NSData *challenge_data(
    NSString *nonce,
    const Options *options,
    const struct stat *executable
) {
    NSString *challenge = [NSString stringWithFormat:
        @"schema\tspaceterm.acceptance.native-launch-challenge/v5\n"
         "launch.nonce\t%@\nrun.id\t%@\npackage.app.sha256\t%@\n"
         "package.app.executable.device\t%llu\npackage.app.executable.inode\t%llu\n"
         "runtime.schema\t%@\nruntime.sample_interval_ms\t%llu\n"
         "runtime.transition_capacity\t%llu\nfailure.action.schema\t%@\n"
         "failure.action.enabled\t%@\n",
        nonce, options->runID, options->appSHA256, (unsigned long long)executable->st_dev,
        (unsigned long long)executable->st_ino, kRuntimeSchema,
        (unsigned long long)kRuntimeSampleIntervalMilliseconds,
        (unsigned long long)kRuntimeTransitionCapacity, kFailureActionSchema,
        [options->failureControl isEqualToString:@"none"] ? @"false" : @"true"];
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
        @"runtime.transition_capacity", @"failure.action.schema", @"failure.action.enabled",
        @"process.pid",
        @"process.executable.path",
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
#define REQUIRE_RESPONSE(CONDITION, FIELD) \
    do { \
        if (!(CONDITION)) { \
            report([NSString stringWithFormat:@"launch observation mismatch: %@", FIELD]); \
            return nil; \
        } \
    } while (0)
    if (records == nil) {
        NSString *text = [[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding];
        NSUInteger count = text == nil ? 0 : [text componentsSeparatedByString:@"\n"].count - 1;
        report([NSString stringWithFormat:
            @"launch observation mismatch: exact-schema (%lu records)",
            (unsigned long)count]);
        return nil;
    }
    REQUIRE_RESPONSE([records[@"schema"] isEqualToString:
        @"spaceterm.acceptance.native-launch-proof/v5"], @"schema");
    REQUIRE_RESPONSE([records[@"observation.source"] isEqualToString:@"production-app"],
        @"observation.source");
    REQUIRE_RESPONSE([records[@"launch.nonce"] isEqualToString:nonce], @"launch.nonce");
    REQUIRE_RESPONSE([records[@"run.id"] isEqualToString:options->runID], @"run.id");
    REQUIRE_RESPONSE([records[@"package.app.sha256"] isEqualToString:options->appSHA256],
        @"package.app.sha256");
    REQUIRE_RESPONSE([records[@"runtime.schema"] isEqualToString:kRuntimeSchema],
        @"runtime.schema");
    REQUIRE_RESPONSE([records[@"runtime.sample_interval_ms"] isEqualToString:@"1000"],
        @"runtime.sample_interval_ms");
    REQUIRE_RESPONSE([records[@"runtime.transition_capacity"] isEqualToString:@"64"],
        @"runtime.transition_capacity");
    REQUIRE_RESPONSE([records[@"failure.action.schema"] isEqualToString:kFailureActionSchema],
        @"failure.action.schema");
    REQUIRE_RESPONSE([records[@"failure.action.enabled"] isEqualToString:
        [options->failureControl isEqualToString:@"none"] ? @"false" : @"true"],
        @"failure.action.enabled");
    REQUIRE_RESPONSE(positive_integer(records[@"process.pid"]) &&
        records[@"process.pid"].intValue == peer_pid, @"process.pid");
    REQUIRE_RESPONSE([records[@"process.executable.path"] isEqualToString:expected_path],
        @"process.executable.path");
    REQUIRE_RESPONSE([records[@"process.executable.device"] isEqualToString:expected_device],
        @"process.executable.device");
    REQUIRE_RESPONSE([records[@"process.executable.inode"] isEqualToString:expected_inode],
        @"process.executable.inode");
    REQUIRE_RESPONSE(records[@"terminal_font_selected"].length > 0 &&
        records[@"terminal_font_selected"].length <= 256, @"terminal_font_selected");
    REQUIRE_RESPONSE(positive_integer(records[@"initial_grid.rows"]), @"initial_grid.rows");
    REQUIRE_RESPONSE(positive_integer(records[@"initial_grid.columns"]),
        @"initial_grid.columns");
    REQUIRE_RESPONSE(positive_number(records[@"initial_grid.logical_width"]),
        @"initial_grid.logical_width");
    REQUIRE_RESPONSE(positive_number(records[@"initial_grid.logical_height"]),
        @"initial_grid.logical_height");
    REQUIRE_RESPONSE(positive_integer(records[@"initial_grid.backing_pixel_width"]),
        @"initial_grid.backing_pixel_width");
    REQUIRE_RESPONSE(positive_integer(records[@"initial_grid.backing_pixel_height"]),
        @"initial_grid.backing_pixel_height");
    REQUIRE_RESPONSE([records[@"observation.complete"] isEqualToString:@"true"],
        @"observation.complete");
#undef REQUIRE_RESPONSE
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

static NSString *runtime_lifecycle_name(uint64_t code) {
    NSArray<NSString *> *values = @[
        @"starting", @"running", @"exited", @"failed", @"observer-failed"
    ];
    return code < values.count ? values[(NSUInteger)code] : @"unknown";
}

static bool runtime_reject(
    NSString *frame,
    NSUInteger expectedRecords,
    NSUInteger actualRecords,
    NSString *firstKey,
    NSString *classification,
    RuntimeCapture *capture,
    uint64_t lifecycle
) {
    runtime_parse_diagnostic = [NSString stringWithFormat:
        @"frame=%@ expected-records=%lu actual-records=%lu first-key=%@ "
         "classification=%@ lifecycle=%@ captured-samples=%llu captured-events=%llu",
        frame, (unsigned long)expectedRecords, (unsigned long)actualRecords, firstKey,
        classification, runtime_lifecycle_name(lifecycle),
        (unsigned long long)capture->sampleCount,
        (unsigned long long)capture->eventCount];
    return false;
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
    uint64_t priorLifecycle = capture->hasSample ? capture->previousSample[33] : 0;
    if (text == nil || ![text hasSuffix:@"\n"]) {
        return runtime_reject(@"tick", 4, 0, @"frame", @"encoding-or-terminator",
            capture, priorLifecycle);
    }
    NSArray<NSString *> *lines = [[text substringToIndex:text.length - 1]
        componentsSeparatedByString:@"\n"];
    if (capture->sampleCount >= kMaximumRuntimeSamples) {
        return runtime_reject(@"tick", 4, lines.count, @"sequence", @"sample-bound",
            capture, priorLifecycle);
    }
    if (lines.count < 4) {
        return runtime_reject(@"tick", 4, lines.count, @"sample", @"missing-record",
            capture, priorLifecycle);
    }
    if (![exact_record_value(lines[0], @"schema") isEqualToString:kRuntimeTickSchema]) {
        return runtime_reject(@"tick", 4, lines.count, @"schema",
            @"missing-extra-or-misordered-key", capture, priorLifecycle);
    }
    uint64_t sequence = 0;
    uint64_t frameEventCount = 0;
    if (!canonical_uint64(exact_record_value(lines[1], @"sequence"), &sequence)) {
        return runtime_reject(@"tick", 4, lines.count, @"sequence",
            @"missing-misordered-or-noncanonical", capture, priorLifecycle);
    }
    if (sequence != capture->sampleCount) {
        return runtime_reject(@"tick", 4, lines.count, @"sequence", @"state-mismatch",
            capture, priorLifecycle);
    }
    if (!canonical_uint64(exact_record_value(lines[2], @"event_count"), &frameEventCount)) {
        return runtime_reject(@"tick", 4, lines.count, @"event_count",
            @"missing-misordered-or-noncanonical", capture, priorLifecycle);
    }
    NSUInteger expectedRecords = (NSUInteger)(4 + frameEventCount);
    if (frameEventCount > kRuntimeTransitionCapacity ||
        frameEventCount > kMaximumRuntimeEvents - capture->eventCount) {
        return runtime_reject(@"tick", expectedRecords, lines.count, @"event_count",
            @"capacity", capture, priorLifecycle);
    }
    if (lines.count != expectedRecords) {
        return runtime_reject(@"tick", expectedRecords, lines.count, @"event",
            @"missing-or-extra-record", capture, priorLifecycle);
    }

    NSArray<NSString *> *sample = [lines[3] componentsSeparatedByString:@"\t"];
    if (sample.count != 36 || ![sample[0] isEqualToString:@"sample"]) {
        return runtime_reject(@"tick", 36, sample.count, @"sample",
            sample.count == 36 ? @"misordered-key" : @"missing-or-extra-column",
            capture, priorLifecycle);
    }
    NSArray<NSString *> *sampleFields = @[
        @"continuous_ns", @"worker_generation", @"screens_published",
        @"screens_enqueued", @"screens_superseded", @"event_queue_length",
        @"event_queue_high_water", @"ui_dispatches", @"ui_screen_events",
        @"ui_drain_high_water", @"ui_latest_generation", @"render_latest_generation",
        @"next_frame_generation", @"next_frame_count", @"presentable", @"minimized",
        @"occluded", @"workspace_visible", @"pane_visible", @"live_resize",
        @"viewport_total_rows", @"viewport_visible_rows", @"viewport_offset_rows",
        @"selection_present", @"resize_requests", @"resize_notifications",
        @"resize_applied", @"resize_coalesced", @"pty_rows", @"pty_columns",
        @"pty_pixel_width", @"pty_pixel_height", @"terminal_inputs_accepted",
        @"lifecycle", @"observer_drops"
    ];
    uint64_t current[35] = {0};
    for (NSUInteger index = 0; index < 35; index++) {
        if (index == 33) {
            if (!is_runtime_lifecycle(sample[index + 1], &current[index])) {
                return runtime_reject(@"tick", 36, sample.count, sampleFields[index],
                    @"invalid-enum", capture, priorLifecycle);
            }
        } else if ((index >= 14 && index <= 19) || index == 23) {
            if (!canonical_bool_digit(sample[index + 1], &current[index])) {
                return runtime_reject(@"tick", 36, sample.count, sampleFields[index],
                    @"noncanonical-boolean", capture, priorLifecycle);
            }
        } else if (!canonical_uint64(sample[index + 1], &current[index])) {
            return runtime_reject(@"tick", 36, sample.count, sampleFields[index],
                @"noncanonical-unsigned", capture, priorLifecycle);
        }
    }

    static const NSUInteger monotonic[] = {
        1, 2, 3, 4, 6, 7, 8, 9, 10, 11, 12, 13,
        24, 25, 26, 27, 32, 34,
    };
    if (current[0] == 0 || (capture->hasSample && current[0] < capture->lastContinuousNS)) {
        return runtime_reject(@"tick", 36, sample.count, @"continuous_ns",
            @"zero-or-regression", capture, current[33]);
    }
    if (current[5] > 2 || current[6] > 2 || current[5] > current[6]) {
        return runtime_reject(@"tick", 36, sample.count, @"event_queue_length",
            @"bounded-state", capture, current[33]);
    }
    if (current[9] > 2) {
        return runtime_reject(@"tick", 36, sample.count, @"ui_drain_high_water",
            @"bounded-state", capture, current[33]);
    }
    if (current[21] > current[20] || current[22] > current[20] - current[21]) {
        return runtime_reject(@"tick", 36, sample.count, @"viewport_visible_rows",
            @"relational-invariant", capture, current[33]);
    }
    if (capture->hasSample &&
        !lifecycle_transition_is_valid(capture->previousSample[33], current[33])) {
        return runtime_reject(@"tick", 36, sample.count, @"lifecycle",
            @"invalid-transition", capture, current[33]);
    }
    if (capture->hasSample) {
        for (NSUInteger index = 0; index < sizeof(monotonic) / sizeof(monotonic[0]); index++) {
            NSUInteger field = monotonic[index];
            if (current[field] < capture->previousSample[field]) {
                return runtime_reject(@"tick", 36, sample.count, sampleFields[field],
                    @"monotonic-regression", capture, current[33]);
            }
        }
    }

    NSString *samplePayload = [[sample subarrayWithRange:NSMakeRange(1, 35)]
        componentsJoinedByString:@"\t"];
    NSString *sampleRow = [NSString stringWithFormat:@"%llu\t%@\n",
        (unsigned long long)sequence, samplePayload];
    if (!append_bounded(capture->samples, sampleRow, kMaximumRuntimeSampleBytes)) {
        return runtime_reject(@"tick", 36, sample.count, @"sample", @"byte-bound",
            capture, current[33]);
    }

    for (uint64_t index = 0; index < frameEventCount; index++) {
        NSArray<NSString *> *event = [lines[(NSUInteger)(4 + index)]
            componentsSeparatedByString:@"\t"];
        uint64_t eventSequence = 0;
        uint64_t eventContinuousNS = 0;
        uint64_t generation = 0;
        uint64_t aux0 = 0;
        uint64_t aux1 = 0;
        if (event.count != 7 || ![event[0] isEqualToString:@"event"]) {
            return runtime_reject(@"tick-event", 7, event.count, @"event",
                event.count == 7 ? @"misordered-key" : @"missing-or-extra-column",
                capture, current[33]);
        }
        if (!canonical_uint64(event[1], &eventSequence) ||
            eventSequence != capture->eventCount) {
            return runtime_reject(@"tick-event", 7, event.count, @"sequence",
                @"noncanonical-or-state-mismatch", capture, current[33]);
        }
        if (!canonical_uint64(event[2], &eventContinuousNS) || eventContinuousNS == 0 ||
            (capture->hasEvent && eventContinuousNS < capture->lastEventContinuousNS) ||
            eventContinuousNS > current[0]) {
            return runtime_reject(@"tick-event", 7, event.count, @"continuous_ns",
                @"noncanonical-or-order", capture, current[33]);
        }
        if (!is_runtime_event_kind(event[3])) {
            return runtime_reject(@"tick-event", 7, event.count, @"kind",
                @"invalid-enum", capture, current[33]);
        }
        if (!canonical_uint64(event[4], &generation) || generation > current[1]) {
            return runtime_reject(@"tick-event", 7, event.count, @"generation",
                @"noncanonical-or-ahead", capture, current[33]);
        }
        if (!canonical_uint64(event[5], &aux0) ||
            !canonical_uint64(event[6], &aux1) ||
            !runtime_event_aux_is_valid(event[3], aux0, aux1)) {
            return runtime_reject(@"tick-event", 7, event.count, @"aux",
                @"noncanonical-or-kind-mismatch", capture, current[33]);
        }
        NSString *eventRow = [NSString stringWithFormat:@"%llu\t%llu\t%@\t%llu\t%llu\t%llu\n",
            (unsigned long long)eventSequence, (unsigned long long)eventContinuousNS, event[3],
            (unsigned long long)generation, (unsigned long long)aux0,
            (unsigned long long)aux1];
        if (!append_bounded(capture->events, eventRow, kMaximumRuntimeEventBytes)) {
            return runtime_reject(@"tick-event", 7, event.count, @"event", @"byte-bound",
                capture, current[33]);
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
            return runtime_reject(@"tick", 36, sample.count, @"continuous_ns",
                @"cadence", capture, current[33]);
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
    NSString *text = [[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding];
    NSArray<NSString *> *lines = text != nil && [text hasSuffix:@"\n"]
        ? [[text substringToIndex:text.length - 1] componentsSeparatedByString:@"\n"]
        : @[];
    if (text == nil || ![text hasSuffix:@"\n"] || lines.count != keys.count) {
        return runtime_reject(@"complete", keys.count, lines.count, @"frame",
            text == nil || ![text hasSuffix:@"\n"]
                ? @"encoding-or-terminator" : @"missing-or-extra-record",
            capture, capture->hasSample ? capture->previousSample[33] : 0);
    }
    for (NSUInteger index = 0; index < keys.count; index++) {
        NSArray<NSString *> *parts = [lines[index] componentsSeparatedByString:@"\t"];
        if (parts.count != 2 || ![parts[0] isEqualToString:keys[index]]) {
            return runtime_reject(@"complete", keys.count, lines.count, keys[index],
                @"missing-extra-or-misordered-key", capture,
                capture->hasSample ? capture->previousSample[33] : 0);
        }
    }
    NSDictionary<NSString *, NSString *> *records = parse_records(data, keys);
    uint64_t started = 0;
    uint64_t ended = 0;
    uint64_t samples = 0;
    uint64_t events = 0;
    uint64_t lifecycle = capture->hasSample ? capture->previousSample[33] : 0;
    if (records == nil) {
        return runtime_reject(@"complete", keys.count, lines.count, @"value",
            @"noncanonical-encoding", capture, lifecycle);
    }
    if (![records[@"schema"] isEqualToString:kRuntimeCompleteSchema]) {
        return runtime_reject(@"complete", keys.count, lines.count, @"schema",
            @"value-mismatch", capture, lifecycle);
    }
#define REQUIRE_COMPLETE_UINT(FIELD, TARGET) \
    do { \
        if (!canonical_uint64(records[FIELD], &TARGET)) { \
            return runtime_reject(@"complete", keys.count, lines.count, FIELD, \
                @"noncanonical-unsigned", capture, lifecycle); \
        } \
    } while (0)
    REQUIRE_COMPLETE_UINT(@"observer.started_continuous_ns", started);
    REQUIRE_COMPLETE_UINT(@"observer.ended_continuous_ns", ended);
    REQUIRE_COMPLETE_UINT(@"observer.sample_count", samples);
    REQUIRE_COMPLETE_UINT(@"observer.event_count", events);
#undef REQUIRE_COMPLETE_UINT
    if (![records[@"observer.status"] isEqualToString:@"complete"] &&
        ![records[@"observer.status"] isEqualToString:@"not-run"]) {
        return runtime_reject(@"complete", keys.count, lines.count, @"observer.status",
            @"invalid-enum", capture, lifecycle);
    }
    if (!capture->hasSample) {
        return runtime_reject(@"complete", keys.count, lines.count, @"observer.sample_count",
            @"empty-stream", capture, lifecycle);
    }
    if (started != capture->firstContinuousNS) {
        return runtime_reject(@"complete", keys.count, lines.count,
            @"observer.started_continuous_ns", @"state-mismatch", capture, lifecycle);
    }
    if (ended != capture->lastContinuousNS) {
        return runtime_reject(@"complete", keys.count, lines.count,
            @"observer.ended_continuous_ns", @"state-mismatch", capture, lifecycle);
    }
    if (samples != capture->sampleCount) {
        return runtime_reject(@"complete", keys.count, lines.count,
            @"observer.sample_count", @"state-mismatch", capture, lifecycle);
    }
    if (events != capture->eventCount) {
        return runtime_reject(@"complete", keys.count, lines.count,
            @"observer.event_count", @"state-mismatch", capture, lifecycle);
    }
    if ([records[@"observer.status"] isEqualToString:@"complete"] &&
        capture->observedFailure) {
        return runtime_reject(@"complete", keys.count, lines.count, @"observer.status",
            @"failure-state-mismatch", capture, lifecycle);
    }
    if ([records[@"observer.status"] isEqualToString:@"complete"] &&
        lifecycle != 2 && lifecycle != 3) {
        return runtime_reject(@"complete", keys.count, lines.count, @"lifecycle",
            @"nonterminal-complete", capture, lifecycle);
    }
    static const NSUInteger left[] = {3, 4, 8, 10, 11, 12, 12, 25, 26, 27};
    static const NSUInteger right[] = {2, 3, 3, 1, 10, 11, 1, 24, 25, 24};
    NSArray<NSString *> *invariantFields = @[
        @"screens_enqueued", @"screens_superseded", @"ui_screen_events",
        @"ui_latest_generation", @"render_latest_generation", @"next_frame_generation",
        @"next_frame_generation", @"resize_notifications", @"resize_applied",
        @"resize_coalesced"
    ];
    if ([records[@"observer.status"] isEqualToString:@"complete"]) {
        for (NSUInteger index = 0; index < sizeof(left) / sizeof(left[0]); index++) {
            bool invalid = index == 6
                ? capture->previousSample[left[index]] < capture->previousSample[right[index]]
                : capture->previousSample[left[index]] > capture->previousSample[right[index]];
            if (invalid) {
                return runtime_reject(@"complete", keys.count, lines.count,
                    invariantFields[index], @"closure-invariant", capture, lifecycle);
            }
        }
        if (capture->previousSample[34] != 0) {
            return runtime_reject(@"complete", keys.count, lines.count, @"observer_drops",
                @"closure-invariant", capture, lifecycle);
        }
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

static bool is_failure_case(NSString *value) {
    return [@[
        @"presentation-invalid-scale", @"presentation-glyph",
        @"renderer-image-preflight", @"renderer-resource-before-sync",
        @"renderer-resource-after-staging", @"pasteboard-write", @"pty-fatal",
        @"emulator-fatal", @"normal-exit-control"
    ] containsObject:value];
}

static int create_failure_control(NSString *path) {
    if ([path isEqualToString:@"none"]) {
        return -1;
    }
    NSString *parent = path.stringByDeletingLastPathComponent;
    struct stat parent_status = {0};
    struct stat existing = {0};
    if (lstat(parent.fileSystemRepresentation, &parent_status) != 0 ||
        !S_ISDIR(parent_status.st_mode) || S_ISLNK(parent_status.st_mode) ||
        parent_status.st_uid != geteuid() || (parent_status.st_mode & 077) != 0 ||
        lstat(path.fileSystemRepresentation, &existing) == 0 || errno != ENOENT ||
        mkfifo(path.fileSystemRepresentation, 0600) != 0) {
        report(@"failure control path is not a new FIFO in an owner-private directory");
        return -2;
    }
    int fd = open(path.fileSystemRepresentation, O_RDWR | O_NONBLOCK | O_CLOEXEC | O_NOFOLLOW);
    struct stat fifo_status = {0};
    if (fd < 0 || fstat(fd, &fifo_status) != 0 || !S_ISFIFO(fifo_status.st_mode) ||
        fifo_status.st_uid != geteuid() || (fifo_status.st_mode & 077) != 0) {
        if (fd >= 0) close(fd);
        unlink(path.fileSystemRepresentation);
        report(@"failure control FIFO could not be authenticated");
        return -2;
    }
    return fd;
}

static bool initialize_failure_capture(FailureCapture *capture) {
    capture->records = [NSMutableData data];
    return append_bounded(
        capture->records, kFailureActionHeader, kMaximumFailureActionBytes);
}

static int forward_failure_control(
    int controlFD,
    int peerFD,
    NSString *nonce,
    const Options *options,
    FailureCapture *capture
) {
    if (controlFD < 0) {
        return 0;
    }
    uint8_t bytes[256] = {0};
    ssize_t count = read(controlFD, bytes, sizeof(bytes));
    if (count < 0 && (errno == EAGAIN || errno == EWOULDBLOCK)) {
        return 0;
    }
    if (count <= 0 || count >= (ssize_t)sizeof(bytes) || bytes[count - 1] != '\n' ||
        memchr(bytes, '\n', (size_t)count - 1) != NULL || capture->pendingRequestID != nil ||
        capture->nextSequence >= kMaximumFailureActions) {
        return -1;
    }
    NSData *commandData = [NSData dataWithBytes:bytes length:(NSUInteger)count - 1];
    NSString *command = [[NSString alloc] initWithData:commandData encoding:NSUTF8StringEncoding];
    NSArray<NSString *> *parts = [command componentsSeparatedByString:@"\t"];
    NSString *caseID = parts.count == 2 ? parts[0] : nil;
    NSString *clientToken = parts.count == 2 ? parts[1] : nil;
    NSString *requestID = make_nonce();
    if (!is_failure_case(caseID) || !is_lower_hex(clientToken, 64) || requestID == nil) {
        return -1;
    }
    NSString *frame = [NSString stringWithFormat:
        @"schema\t%@\nlaunch.nonce\t%@\nrun.id\t%@\npackage.app.sha256\t%@\n"
         "request.id\t%@\nsequence\t%llu\ncase.id\t%@\nrequest.once\ttrue\n",
        kFailureActionSchema, nonce, options->runID, options->appSHA256, requestID,
        (unsigned long long)capture->nextSequence, caseID];
    NSData *frameData = [frame dataUsingEncoding:NSUTF8StringEncoding];
    if (frameData == nil || !write_frame(peerFD, frameData)) {
        return -1;
    }
    capture->pendingRequestID = requestID;
    capture->pendingCaseID = caseID;
    capture->pendingClientToken = clientToken;
    capture->pendingSequence = capture->nextSequence;
    capture->nextSequence++;
    capture->pendingPhase = 0;
    return 1;
}

static bool value_is_one_of(NSString *value, NSArray<NSString *> *allowed) {
    return value != nil && [allowed containsObject:value];
}

static bool validate_failure_state(
    NSDictionary<NSString *, NSString *> *records,
    NSString *caseID,
    NSString *action
) {
    NSString *class = records[@"failure.class"];
    NSString *recoverability = records[@"failure.recoverability"];
    NSString *operation = records[@"failure.operation"];
    NSString *pending = records[@"pending_recovery"];
    if ([action isEqualToString:@"armed"] ||
        ([action isEqualToString:@"completed"] &&
            ([caseID isEqualToString:@"normal-exit-control"] ||
                ![caseID hasSuffix:@"fatal"]))) {
        return [class isEqualToString:@"none"] &&
            [recoverability isEqualToString:@"none"] &&
            [operation isEqualToString:@"none"] && [pending isEqualToString:@"none"];
    }
    if ([caseID isEqualToString:@"presentation-invalid-scale"]) {
        return [class isEqualToString:@"presentation"] &&
            [recoverability isEqualToString:@"recoverable"] &&
            [operation isEqualToString:@"update-backing-scale"] &&
            [pending isEqualToString:@"presentation"];
    }
    if ([caseID isEqualToString:@"presentation-glyph"]) {
        return [class isEqualToString:@"presentation"] &&
            [recoverability isEqualToString:@"recoverable"] &&
            [operation isEqualToString:@"paint-terminal-presentation"] &&
            [pending isEqualToString:@"presentation"];
    }
    if ([caseID isEqualToString:@"renderer-image-preflight"]) {
        return [class isEqualToString:@"resource"] &&
            [recoverability isEqualToString:@"recoverable"] &&
            [operation isEqualToString:@"paint-terminal-graphics"] &&
            [pending isEqualToString:@"renderer-resources"];
    }
    if ([caseID hasPrefix:@"renderer-resource-"]) {
        return [class isEqualToString:@"resource"] &&
            [recoverability isEqualToString:@"recoverable"] &&
            [operation isEqualToString:@"prepare-terminal-graphics"] &&
            [pending isEqualToString:@"renderer-resources"];
    }
    if ([caseID isEqualToString:@"pasteboard-write"]) {
        return [class isEqualToString:@"platform"] &&
            [recoverability isEqualToString:@"recoverable"] &&
            [operation isEqualToString:@"write-selection-pasteboard"] &&
            [pending isEqualToString:@"copy-selection"];
    }
    if ([caseID isEqualToString:@"pty-fatal"]) {
        return [class isEqualToString:@"pty"] && [recoverability isEqualToString:@"fatal"] &&
            [operation isEqualToString:@"read-shell-output"] &&
            [pending isEqualToString:@"none"];
    }
    if ([caseID isEqualToString:@"emulator-fatal"]) {
        return [class isEqualToString:@"emulator"] &&
            [recoverability isEqualToString:@"fatal"] &&
            [operation isEqualToString:@"session-runtime"] &&
            [pending isEqualToString:@"none"];
    }
    return false;
}

static bool write_failure_status(int fd, NSString *status, NSString *clientToken) {
    if (fd < 0) {
        return true;
    }
    NSData *record = [[NSString stringWithFormat:@"%@\t%@\n", status, clientToken]
        dataUsingEncoding:NSUTF8StringEncoding];
    return record != nil &&
        write(fd, record.bytes, record.length) == (ssize_t)record.length;
}

static bool parse_failure_result_with_status(
    NSData *data,
    FailureCapture *capture,
    int statusFD
) {
    NSArray<NSString *> *keys = @[
        @"schema", @"request.id", @"sequence", @"case.id", @"action", @"result",
        @"pane.id", @"pane.state", @"failure.class", @"failure.recoverability",
        @"failure.operation", @"state.revision", @"latest.generation",
        @"last_valid.generation", @"visible.generation", @"pending_recovery",
        @"terminal_input_usable", @"session_attached", @"resource.staged_count",
        @"resource.staged_bytes", @"resource.rolled_back_count",
        @"resource.rolled_back_bytes"
    ];
    NSDictionary<NSString *, NSString *> *records = parse_records(data, keys);
    uint64_t sequence = 0;
    uint64_t paneID = 0;
    uint64_t stateRevision = 0;
    uint64_t latest = 0;
    uint64_t lastValid = 0;
    uint64_t visible = 0;
    uint64_t inputUsable = 0;
    uint64_t sessionAttached = 0;
    uint64_t stagedCount = 0;
    uint64_t stagedBytes = 0;
    uint64_t rolledBackCount = 0;
    uint64_t rolledBackBytes = 0;
    NSString *action = records[@"action"];
    NSString *result = records[@"result"];
    bool visibleAvailable = ![records[@"visible.generation"] isEqualToString:@"unavailable"];
    if (records == nil || capture->pendingRequestID == nil ||
        ![records[@"schema"] isEqualToString:kFailureActionResultSchema] ||
        ![records[@"request.id"] isEqualToString:capture->pendingRequestID] ||
        ![records[@"case.id"] isEqualToString:capture->pendingCaseID] ||
        !canonical_uint64(records[@"sequence"], &sequence) ||
        sequence != capture->pendingSequence || !canonical_uint64(records[@"pane.id"], &paneID) ||
        !canonical_uint64(records[@"state.revision"], &stateRevision) ||
        !canonical_uint64(records[@"latest.generation"], &latest) ||
        !canonical_uint64(records[@"last_valid.generation"], &lastValid) ||
        (visibleAvailable && !canonical_uint64(records[@"visible.generation"], &visible)) ||
        !canonical_bool_digit(records[@"terminal_input_usable"], &inputUsable) ||
        !canonical_bool_digit(records[@"session_attached"], &sessionAttached) ||
        !canonical_uint64(records[@"resource.staged_count"], &stagedCount) ||
        !canonical_uint64(records[@"resource.staged_bytes"], &stagedBytes) ||
        !canonical_uint64(records[@"resource.rolled_back_count"], &rolledBackCount) ||
        !canonical_uint64(records[@"resource.rolled_back_bytes"], &rolledBackBytes) ||
        stagedCount > 65536 || rolledBackCount > 65536 ||
        stagedBytes > 402653184 || rolledBackBytes > 402653184 ||
        !value_is_one_of(records[@"pane.state"], @[@"running", @"failed", @"exited"]) ||
        !value_is_one_of(action, @[@"armed", @"injected", @"retry-requested", @"completed"]) ||
        !value_is_one_of(result, @[@"accepted", @"failed-state", @"recovered", @"closed", @"exited"]) ||
        latest < lastValid || (visibleAvailable && visible > latest) ||
        !validate_failure_state(records, capture->pendingCaseID, action)) {
        return false;
    }
    if ((capture->pendingPhase != 0 && paneID != capture->pendingPaneID) ||
        (capture->pendingPhase != 0 && stateRevision < capture->lastStateRevision)) {
        return false;
    }

    BOOL phaseStateValid = NO;
    if ([action isEqualToString:@"armed"] && [result isEqualToString:@"accepted"]) {
        phaseStateValid = [records[@"pane.state"] isEqualToString:@"running"] &&
            sessionAttached == 1;
    } else if ([action isEqualToString:@"injected"] &&
        [result isEqualToString:@"failed-state"]) {
        phaseStateValid = [records[@"pane.state"] isEqualToString:@"failed"] &&
            sessionAttached == 1 &&
            (![capture->pendingCaseID hasSuffix:@"fatal"] || inputUsable == 0) &&
            (![capture->pendingCaseID isEqualToString:@"pasteboard-write"] || inputUsable == 1);
    } else if ([action isEqualToString:@"retry-requested"] &&
        [result isEqualToString:@"accepted"]) {
        phaseStateValid = [records[@"pane.state"] isEqualToString:@"failed"] &&
            sessionAttached == 1;
    } else if ([action isEqualToString:@"completed"] &&
        [result isEqualToString:@"recovered"]) {
        phaseStateValid = [records[@"pane.state"] isEqualToString:@"running"] &&
            sessionAttached == 1;
    } else if ([action isEqualToString:@"completed"] &&
        [result isEqualToString:@"closed"]) {
        phaseStateValid = [records[@"pane.state"] isEqualToString:@"failed"] &&
            sessionAttached == 0 && inputUsable == 0;
    } else if ([action isEqualToString:@"completed"] &&
        [result isEqualToString:@"exited"]) {
        phaseStateValid = [records[@"pane.state"] isEqualToString:@"exited"] &&
            sessionAttached == 1;
    }
    if (!phaseStateValid) {
        return false;
    }

    NSUInteger nextPhase = capture->pendingPhase;
    if (capture->pendingPhase == 0 && [action isEqualToString:@"armed"] &&
        [result isEqualToString:@"accepted"]) {
        nextPhase = 1;
    } else if (capture->pendingPhase == 1 && [action isEqualToString:@"injected"] &&
        [result isEqualToString:@"failed-state"]) {
        nextPhase = 2;
    } else if (capture->pendingPhase == 1 &&
        [capture->pendingCaseID isEqualToString:@"normal-exit-control"] &&
        [action isEqualToString:@"completed"] && [result isEqualToString:@"exited"]) {
        nextPhase = 4;
    } else if (capture->pendingPhase == 2 && [action isEqualToString:@"retry-requested"] &&
        [result isEqualToString:@"accepted"]) {
        nextPhase = 3;
    } else if (capture->pendingPhase == 2 && [capture->pendingCaseID hasSuffix:@"fatal"] &&
        [action isEqualToString:@"completed"] && [result isEqualToString:@"closed"]) {
        nextPhase = 4;
    } else if (capture->pendingPhase == 3 && [action isEqualToString:@"completed"] &&
        [result isEqualToString:@"recovered"]) {
        nextPhase = 4;
    } else {
        return false;
    }

    if ([action isEqualToString:@"injected"] && ![capture->pendingCaseID hasSuffix:@"fatal"] &&
        (!visibleAvailable || visible != lastValid)) {
        return false;
    }
    if ([action isEqualToString:@"injected"]) {
        capture->injectedLatest = latest;
        capture->injectedLastValid = lastValid;
        capture->injectedVisible = visible;
    }
    BOOL afterStaging = [capture->pendingCaseID isEqualToString:
        @"renderer-resource-after-staging"];
    if ((!afterStaging && (stagedCount != 0 || stagedBytes != 0 ||
            rolledBackCount != 0 || rolledBackBytes != 0)) ||
        (afterStaging && [action isEqualToString:@"armed"] &&
            (stagedCount != 0 || stagedBytes != 0 || rolledBackCount != 0 ||
                rolledBackBytes != 0)) ||
        (afterStaging && ![action isEqualToString:@"armed"] &&
            (stagedCount == 0 || stagedBytes == 0 || rolledBackCount != stagedCount ||
                rolledBackBytes != stagedBytes))) {
        return false;
    }
    if (afterStaging && [action isEqualToString:@"injected"]) {
        capture->injectedResourceCount = stagedCount;
        capture->injectedResourceBytes = stagedBytes;
    } else if (afterStaging && ![action isEqualToString:@"armed"] &&
        (stagedCount != capture->injectedResourceCount ||
            stagedBytes != capture->injectedResourceBytes)) {
        return false;
    }
    BOOL pasteboard = [capture->pendingCaseID isEqualToString:@"pasteboard-write"];
    if ([action isEqualToString:@"retry-requested"] &&
        ((!pasteboard &&
             (latest != capture->injectedLatest || lastValid != capture->injectedLastValid ||
                 !visibleAvailable || visible != capture->injectedVisible)) ||
            (pasteboard &&
                (latest < capture->injectedLatest || lastValid < capture->injectedLastValid ||
                    !visibleAvailable || visible < capture->injectedVisible ||
                    visible != lastValid || inputUsable != 1 || sessionAttached != 1)))) {
        return false;
    }
    if ([action isEqualToString:@"completed"] &&
        [result isEqualToString:@"recovered"] &&
        ((!pasteboard &&
             (!visibleAvailable || latest != capture->injectedLatest || lastValid != latest ||
                 visible != latest)) ||
            (pasteboard &&
                (!visibleAvailable || latest < capture->injectedLatest ||
                    lastValid < capture->injectedLastValid || lastValid > latest ||
                    visible < capture->injectedVisible || visible != lastValid ||
                    inputUsable != 1 || sessionAttached != 1)))) {
        return false;
    }
    if ([action isEqualToString:@"completed"] && [capture->pendingCaseID hasSuffix:@"fatal"] &&
        (sessionAttached != 0 || ![records[@"pane.state"] isEqualToString:@"failed"])) {
        return false;
    }
    NSString *row = [NSString stringWithFormat:
        @"%@\t%@\t%@\t%@\t%@\t%@\t%@\t%@\t%@\t%@\t%@\t%@\t%@\t%@\t%@\t%@\t%@\t%@\t%@\t%@\t%@\n",
        records[@"request.id"], records[@"sequence"], records[@"case.id"], action, result,
        records[@"pane.id"], records[@"pane.state"], records[@"failure.class"],
        records[@"failure.recoverability"], records[@"failure.operation"],
        records[@"state.revision"], records[@"latest.generation"],
        records[@"last_valid.generation"], records[@"visible.generation"],
        records[@"pending_recovery"], records[@"terminal_input_usable"],
        records[@"session_attached"], records[@"resource.staged_count"],
        records[@"resource.staged_bytes"], records[@"resource.rolled_back_count"],
        records[@"resource.rolled_back_bytes"]];
    if (!append_bounded(capture->records, row, kMaximumFailureActionBytes)) {
        return false;
    }
    if (([action isEqualToString:@"armed"] &&
            !write_failure_status(statusFD, @"accepted", capture->pendingClientToken)) ||
        ([action isEqualToString:@"completed"] &&
            !write_failure_status(statusFD, @"completed", capture->pendingClientToken))) {
        return false;
    }
    capture->resultCount++;
    capture->pendingPaneID = paneID;
    capture->lastStateRevision = stateRevision;
    capture->pendingPhase = nextPhase;
    if (nextPhase == 4) {
        capture->pendingRequestID = nil;
        capture->pendingCaseID = nil;
        capture->pendingClientToken = nil;
        capture->pendingPhase = 0;
        capture->pendingPaneID = 0;
        capture->lastStateRevision = 0;
        capture->injectedLatest = 0;
        capture->injectedLastValid = 0;
        capture->injectedVisible = 0;
        capture->injectedResourceCount = 0;
        capture->injectedResourceBytes = 0;
    }
    return capture->resultCount <= kMaximumFailureActions * 4;
}

static bool parse_failure_result(NSData *data, FailureCapture *capture) {
    return parse_failure_result_with_status(data, capture, -1);
}

static int forward_performance_quit(
    int controlFD,
    int statusFD,
    const Options *options,
    QuitCapture *quit,
    NSRunningApplication *application,
    audit_token_t *token,
    NSString *expectedPath,
    const struct stat *expectedStat,
    const struct statfs *expectedFS,
    pid_t peerPID,
    int pidversion,
    uint64_t startSeconds,
    uint64_t startMicroseconds
) {
    if (controlFD < 0) return 0;
    uint8_t bytes[256] = {0};
    ssize_t count = read(controlFD, bytes, sizeof(bytes));
    if (count < 0 && (errno == EAGAIN || errno == EWOULDBLOCK)) return 0;
    if (count <= 0 || count >= (ssize_t)sizeof(bytes) || bytes[count - 1] != '\n' ||
        memchr(bytes, '\n', (size_t)count - 1) != NULL || quit->requested) return -1;
    NSString *command = [[NSString alloc]
        initWithBytes:bytes length:(NSUInteger)count - 1 encoding:NSUTF8StringEncoding];
    NSArray<NSString *> *parts = [command componentsSeparatedByString:@"\t"];
    NSString *clientToken = parts.count == 2 ? parts[1] : nil;
    struct proc_bsdinfo process = {0};
    NSString *liveCDHash = nil;
    NSString *liveIdentifier = nil;
    NSString *liveTeam = nil;
    NSString *campaignID = nil;
    NSString *sessionID = nil;
    NSString *campaignNonce = nil;
    NSString *runIntentSHA256 = nil;
    NSString *subject = nil;
    if (parts.count != 2 || ![parts[0] isEqualToString:@"tail-complete"] ||
        !is_lower_hex(clientToken, 64) || application.processIdentifier != peerPID ||
        application.terminated || audit_token_to_pid(*token) != peerPID ||
        audit_token_to_pidversion(*token) != pidversion ||
        proc_pidinfo(peerPID, PROC_PIDTBSDINFO, 0, &process, sizeof(process)) != sizeof(process) ||
        process.pbi_start_tvsec != startSeconds ||
        process.pbi_start_tvusec != startMicroseconds ||
        !mapped_executable_matches(token, expectedPath, expectedStat, expectedFS) ||
        !live_signature_matches(token, options, expectedPath,
            &liveCDHash, &liveIdentifier, &liveTeam) ||
        !validate_tail_receipt(options, clientToken, peerPID, startSeconds, startMicroseconds,
            &campaignID, &sessionID, &campaignNonce, &runIntentSHA256, &subject)) {
        return -1;
    }
    uint64_t requested = continuous_nanoseconds();
    if ((!options->externalLifecycle && !strict_normal_terminate(application)) ||
        !write_failure_status(statusFD, @"accepted", clientToken)) return -1;
    quit->requested = true;
    quit->requestContinuousNS = requested;
    quit->token = clientToken;
    quit->campaignID = campaignID;
    quit->sessionID = sessionID;
    quit->campaignNonce = campaignNonce;
    quit->runIntentSHA256 = runIntentSHA256;
    quit->subject = subject;
    return 1;
}

static bool read_runtime_stream(
    int fd,
    RuntimeCapture *capture,
    FailureCapture *failure,
    int failureControlFD,
    int failureStatusFD,
    int quitControlFD,
    int quitStatusFD,
    QuitCapture *quit,
    NSRunningApplication *application,
    audit_token_t *token,
    NSString *expectedPath,
    const struct stat *expectedStat,
    const struct statfs *expectedFS,
    pid_t peerPID,
    int pidversion,
    uint64_t startSeconds,
    uint64_t startMicroseconds,
    NSString *nonce,
    const Options *options,
    NSString **status,
    NSDate *deadline
) {
    runtime_parse_diagnostic = nil;
    if (!initialize_runtime_capture(capture) || !initialize_failure_capture(failure)) {
        runtime_parse_diagnostic = @"frame=stream expected-records=1 actual-records=0 "
            "first-key=capture classification=initialization lifecycle=unknown "
            "captured-samples=0 captured-events=0";
        return false;
    }
    while (!interrupted) {
        if (deadline != nil && [deadline timeIntervalSinceNow] <= 0) {
            return false;
        }
        if (forward_failure_control(failureControlFD, fd, nonce, options, failure) < 0) {
            return false;
        }
        if (forward_performance_quit(quitControlFD, quitStatusFD, options, quit,
                application, token, expectedPath, expectedStat, expectedFS, peerPID,
                pidversion, startSeconds, startMicroseconds) < 0) {
            return false;
        }
        NSData *frame = read_frame(fd);
        if (frame == nil) {
            runtime_parse_diagnostic = [NSString stringWithFormat:
                @"frame=stream expected-records=1 actual-records=0 first-key=frame "
                 "classification=transport-or-frame-bound lifecycle=%@ "
                 "captured-samples=%llu captured-events=%llu",
                runtime_lifecycle_name(capture->hasSample ? capture->previousSample[33] : 0),
                (unsigned long long)capture->sampleCount,
                (unsigned long long)capture->eventCount];
            return false;
        }
        NSString *text = [[NSString alloc] initWithData:frame encoding:NSUTF8StringEncoding];
        if ([text hasPrefix:@"schema\tspaceterm.acceptance.runtime-tick/v1\n"]) {
            if (!parse_runtime_tick(frame, capture)) {
                return false;
            }
        } else if ([text hasPrefix:@"schema\tspaceterm.acceptance.runtime-complete/v1\n"]) {
            return failure->pendingRequestID == nil &&
                parse_runtime_complete(frame, capture, status);
        } else if ([text hasPrefix:@"schema\tspaceterm.acceptance.failure-action-result/v2\n"]) {
            if (!parse_failure_result_with_status(frame, failure, failureStatusFD)) {
                return false;
            }
        } else {
            NSUInteger lineCount = text != nil && [text hasSuffix:@"\n"]
                ? [[text substringToIndex:text.length - 1]
                    componentsSeparatedByString:@"\n"].count : 0;
            runtime_parse_diagnostic = [NSString stringWithFormat:
                @"frame=unknown expected-records=1 actual-records=%lu first-key=schema "
                 "classification=unknown-frame-schema lifecycle=%@ "
                 "captured-samples=%llu captured-events=%llu",
                (unsigned long)lineCount,
                runtime_lifecycle_name(capture->hasSample ? capture->previousSample[33] : 0),
                (unsigned long long)capture->sampleCount,
                (unsigned long long)capture->eventCount];
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
    NSString *teamValue = information[(__bridge NSString *)kSecCodeInfoTeamIdentifier];
    NSString *team = teamValue != nil ? teamValue : @"";
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
        @[@"failure.action.schema", response[@"failure.action.schema"]],
        @[@"failure.action.enabled", response[@"failure.action.enabled"]],
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

static NSString *final_observation_with_closure(
    NSString *baseObservation,
    NSString *provisionalObservationSHA256,
    NSString *metadataSHA256,
    NSString *failureActionsSHA256,
    uint64_t failureRequestCount,
    uint64_t failureResultCount
) {
    NSString *prefix = [baseObservation substringToIndex:
        baseObservation.length - @"observation.complete\ttrue\n".length];
    return [prefix stringByAppendingFormat:
        @"provisional.observation.sha256\t%@\n"
         "runtime.metadata.schema\tspaceterm.acceptance.runtime-observation-metadata/v3\n"
         "runtime.metadata.path\truntime-metadata.tsv\nruntime.metadata.sha256\t%@\n"
         "failure.result.schema\t%@\nfailure.actions.path\tfailure-actions.tsv\n"
         "failure.actions.sha256\t%@\nfailure.request_count\t%llu\n"
         "failure.result_count\t%llu\nobservation.complete\ttrue\n",
        provisionalObservationSHA256, metadataSHA256, kFailureActionResultSchema,
        failureActionsSHA256,
        (unsigned long long)failureRequestCount, (unsigned long long)failureResultCount];
}

static NSData *ax_subject_identity(
    const Options *options,
    NSString *nonce,
    NSString *appPath,
    NSString *executablePath,
    pid_t pid,
    const struct proc_bsdinfo *process,
    const struct stat *executable,
    const struct statfs *filesystem,
    NSString *cdhash,
    NSString *identifier,
    NSString *team,
    NSString *launchObservationSHA256
) {
    NSBundle *bundle = [NSBundle bundleWithPath:appPath];
    NSString *bundleIdentifier = bundle.bundleIdentifier;
    if (bundleIdentifier.length == 0) {
        return nil;
    }
    NSArray<NSArray<NSString *> *> *records = @[
        @[@"schema", kAXSubjectSchema],
        @[@"run.id", options->runID],
        @[@"launch.nonce", nonce],
        @[@"package.app.sha256", options->appSHA256],
        @[@"package.app.path", appPath],
        @[@"package.app.bundle.identifier", bundleIdentifier],
        @[@"package.app.executable.path", executablePath],
        @[@"process.pid", [NSString stringWithFormat:@"%d", pid]],
        @[@"process.start.tv-sec", [NSString stringWithFormat:@"%llu",
            (unsigned long long)process->pbi_start_tvsec]],
        @[@"process.start.tv-usec", [NSString stringWithFormat:@"%llu",
            (unsigned long long)process->pbi_start_tvusec]],
        @[@"process.executable.device", [NSString stringWithFormat:@"%llu",
            (unsigned long long)executable->st_dev]],
        @[@"process.executable.inode", [NSString stringWithFormat:@"%llu",
            (unsigned long long)executable->st_ino]],
        @[@"process.executable.fsid", [NSString stringWithFormat:@"%d:%d",
            filesystem->f_fsid.val[0], filesystem->f_fsid.val[1]]],
        @[@"process.signature.cdhash", cdhash.lowercaseString],
        @[@"process.signature.identifier", identifier],
        @[@"process.signature.team-identifier", team],
        @[@"process.mount.read-only", @"true"],
        @[@"launch.controller", @"acceptance-launch-verifier"],
        @[@"launch.source", @"mounted-dmg"],
        @[@"launch.observation.sha256", launchObservationSHA256],
        @[@"launch.observation.complete", @"true"],
    ];
    NSMutableString *result = [NSMutableString string];
    for (NSArray<NSString *> *record in records) {
        [result appendFormat:@"%@\t%@\n", record[0], encode_value(record[1])];
    }
    return [result dataUsingEncoding:NSUTF8StringEncoding];
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
    NSString *eventsSHA256,
    FailureCapture *failure,
    NSString *failureSHA256
) {
    NSArray<NSArray<NSString *> *> *records = @[
        @[@"schema", @"spaceterm.acceptance.runtime-observation-metadata/v3"],
        @[@"observation.source", @"production-app"],
        @[@"run.id", options->runID],
        @[@"package.app.sha256", options->appSHA256],
        @[@"process.pid", response[@"process.pid"]],
        @[@"runtime.samples.path", @"runtime-samples.tsv"],
        @[@"runtime.samples.sha256", samplesSHA256],
        @[@"runtime.events.path", @"runtime-events.tsv"],
        @[@"runtime.events.sha256", eventsSHA256],
        @[@"failure.action.schema", kFailureActionSchema],
        @[@"failure.action.enabled", response[@"failure.action.enabled"]],
        @[@"failure.result.schema", kFailureActionResultSchema],
        @[@"failure.actions.path", @"failure-actions.tsv"],
        @[@"failure.actions.sha256", failureSHA256],
        @[@"failure.request_count", [NSString stringWithFormat:@"%llu",
            (unsigned long long)failure->nextSequence]],
        @[@"failure.result_count", [NSString stringWithFormat:@"%llu",
            (unsigned long long)failure->resultCount]],
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
    bool success = write_all(fd, data.bytes, data.length) && fchmod(fd, 0400) == 0 && fsync(fd) == 0;
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

static uint64_t continuous_nanoseconds(void) {
    static mach_timebase_info_data_t timebase = {0};
    if (timebase.denom == 0) {
        mach_timebase_info(&timebase);
    }
    __uint128_t scaled = (__uint128_t)mach_continuous_time() * timebase.numer;
    return timebase.denom == 0 ? 0 : (uint64_t)(scaled / timebase.denom);
}

static NSData *read_stable_private_file(NSString *path, NSUInteger maximumBytes) {
    struct stat before = {0};
    if (lstat(path.fileSystemRepresentation, &before) != 0 || !S_ISREG(before.st_mode) ||
        S_ISLNK(before.st_mode) || before.st_uid != geteuid() ||
        (before.st_mode & 0222) != 0 ||
        before.st_nlink != 1 || before.st_size <= 0 || (uint64_t)before.st_size > maximumBytes) {
        return nil;
    }
    int fd = open(path.fileSystemRepresentation, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    if (fd < 0) return nil;
    struct stat opened = {0};
    NSMutableData *data = [NSMutableData dataWithLength:(NSUInteger)before.st_size];
    size_t offset = 0;
    while (offset < data.length) {
        ssize_t count = read(fd, (uint8_t *)data.mutableBytes + offset, data.length - offset);
        if (count > 0) offset += (size_t)count;
        else if (count < 0 && errno == EINTR) continue;
        else break;
    }
    struct stat after = {0};
    bool valid = fstat(fd, &opened) == 0 && fstat(fd, &after) == 0 &&
        before.st_dev == opened.st_dev && before.st_ino == opened.st_ino &&
        before.st_dev == after.st_dev && before.st_ino == after.st_ino &&
        before.st_mode == after.st_mode && before.st_size == after.st_size &&
        before.st_mtimespec.tv_sec == after.st_mtimespec.tv_sec &&
        before.st_mtimespec.tv_nsec == after.st_mtimespec.tv_nsec && offset == data.length;
    close(fd);
    struct stat current = {0};
    valid = valid && lstat(path.fileSystemRepresentation, &current) == 0 &&
        before.st_dev == current.st_dev && before.st_ino == current.st_ino &&
        before.st_mode == current.st_mode && before.st_size == current.st_size &&
        before.st_mtimespec.tv_sec == current.st_mtimespec.tv_sec &&
        before.st_mtimespec.tv_nsec == current.st_mtimespec.tv_nsec;
    return valid ? data : nil;
}

static NSData *read_stable_secret_file(NSString *path) {
    struct stat before = {0};
    if (lstat(path.fileSystemRepresentation, &before) != 0 || !S_ISREG(before.st_mode) ||
        S_ISLNK(before.st_mode) || before.st_uid != geteuid() ||
        (before.st_mode & 077) != 0 || before.st_nlink != 1 ||
        before.st_size < 32 || before.st_size > 4096) {
        return nil;
    }
    int fd = open(path.fileSystemRepresentation, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    if (fd < 0) return nil;
    NSMutableData *data = [NSMutableData dataWithLength:(NSUInteger)before.st_size];
    size_t offset = 0;
    while (offset < data.length) {
        ssize_t count = read(fd, (uint8_t *)data.mutableBytes + offset, data.length - offset);
        if (count > 0) offset += (size_t)count;
        else if (count < 0 && errno == EINTR) continue;
        else break;
    }
    struct stat after = {0};
    bool valid = fstat(fd, &after) == 0 && before.st_dev == after.st_dev &&
        before.st_ino == after.st_ino && before.st_mode == after.st_mode &&
        before.st_size == after.st_size && offset == data.length;
    close(fd);
    return valid ? data : nil;
}

static bool validate_tail_receipt(
    const Options *options,
    NSString *clientToken,
    pid_t pid,
    uint64_t startSeconds,
    uint64_t startMicroseconds,
    NSString **campaignID,
    NSString **sessionID,
    NSString **campaignNonce,
    NSString **runIntentSHA256,
    NSString **subject
) {
    NSData *intentData = read_stable_private_file(options->runIntent, 64 * 1024);
    NSData *tailData = read_stable_private_file(options->tailReceipt, 64 * 1024);
    NSData *secret = read_stable_secret_file(options->campaignSecret);
    NSArray<NSString *> *intentKeys = @[
        @"format_version", @"subject", @"subject_identity_sha256", @"scenario",
        @"scenario_plan_sha256", @"workload_sha256", @"command_sha256",
        @"environment_sha256", @"font_sha256", @"initial_grid_sha256",
        @"measured_duration_ms", @"process_pid", @"process_start_identity",
        @"campaign_id", @"session_id", @"nonce",
        @"native_provisional_observation_sha256", @"evidence_mode", @"status"
    ];
    NSArray<NSString *> *tailKeys = @[
        @"format_version", @"campaign_id", @"session_id", @"nonce", @"quit_token",
        @"run_intent_sha256", @"subject_identity_sha256", @"subject_process_pid",
        @"subject_process_start_identity", @"driver_receipt_sha256",
        @"driver_events_sha256", @"workload_metadata_sha256", @"workload_events_sha256",
        @"rss_samples_sha256", @"trace_provisional_receipt_sha256",
        @"tail_completed_continuous_ns", @"appkit_terminator_source_device",
        @"appkit_terminator_source_inode", @"appkit_terminator_source_sha256",
        @"appkit_terminator_binary_device", @"appkit_terminator_binary_inode",
        @"appkit_terminator_binary_sha256", @"evidence_mode", @"terminal_status", @"auth_algorithm",
        @"tail_hmac_sha256"
    ];
    NSDictionary *intent = intentData == nil ? nil : parse_records(intentData, intentKeys);
    NSDictionary *tail = tailData == nil ? nil : parse_records(tailData, tailKeys);
    NSString *intentHash = intentData == nil ? nil : lower_sha256(intentData).lowercaseString;
    NSString *start = [NSString stringWithFormat:@"%llu:%llu",
        (unsigned long long)startSeconds, (unsigned long long)startMicroseconds];
    uint64_t tailCompleted = 0;
    uint64_t sourceDevice = 0, sourceInode = 0, binaryDevice = 0, binaryInode = 0;
    if (intent == nil || tail == nil || secret == nil ||
        ![intent[@"format_version"] isEqualToString:@"1"] ||
        ![intent[@"subject"] isEqualToString:@"spaceterm"] ||
        ![intent[@"evidence_mode"] isEqualToString:@"production"] ||
        ![intent[@"status"] isEqualToString:@"prepared"] ||
        ![intent[@"process_pid"] isEqualToString:[NSString stringWithFormat:@"%d", pid]] ||
        ![intent[@"process_start_identity"] isEqualToString:start] ||
        ![tail[@"format_version"] isEqualToString:@"1"] ||
        ![tail[@"campaign_id"] isEqualToString:intent[@"campaign_id"]] ||
        ![tail[@"session_id"] isEqualToString:intent[@"session_id"]] ||
        ![tail[@"nonce"] isEqualToString:intent[@"nonce"]] ||
        ![tail[@"quit_token"] isEqualToString:clientToken] ||
        ![tail[@"run_intent_sha256"] isEqualToString:intentHash] ||
        ![tail[@"subject_identity_sha256"] isEqualToString:intent[@"subject_identity_sha256"]] ||
        ![tail[@"subject_process_pid"] isEqualToString:intent[@"process_pid"]] ||
        ![tail[@"subject_process_start_identity"] isEqualToString:start] ||
        !canonical_uint64(tail[@"appkit_terminator_source_device"], &sourceDevice) ||
        !canonical_uint64(tail[@"appkit_terminator_source_inode"], &sourceInode) ||
        !canonical_uint64(tail[@"appkit_terminator_binary_device"], &binaryDevice) ||
        !canonical_uint64(tail[@"appkit_terminator_binary_inode"], &binaryInode) ||
        sourceDevice == 0 || sourceInode == 0 || binaryDevice == 0 || binaryInode == 0 ||
        !is_lower_hex(tail[@"appkit_terminator_source_sha256"], 64) ||
        !is_lower_hex(tail[@"appkit_terminator_binary_sha256"], 64) ||
        ![tail[@"evidence_mode"] isEqualToString:@"production"] ||
        ![tail[@"terminal_status"] isEqualToString:@"tail-complete"] ||
        ![tail[@"auth_algorithm"] isEqualToString:@"hmac-sha256"] ||
        !is_lower_hex(tail[@"tail_hmac_sha256"], 64) ||
        !canonical_uint64(tail[@"tail_completed_continuous_ns"], &tailCompleted) ||
        tailCompleted == 0 || tailCompleted > continuous_nanoseconds()) {
        return false;
    }
    for (NSString *key in @[@"driver_receipt_sha256", @"driver_events_sha256",
            @"workload_metadata_sha256", @"workload_events_sha256", @"rss_samples_sha256",
            @"trace_provisional_receipt_sha256"]) {
        if (!is_lower_hex(tail[key], 64)) return false;
    }
    NSString *text = [[NSString alloc] initWithData:tailData encoding:NSUTF8StringEncoding];
    NSRange signatureRange = [text rangeOfString:@"tail_hmac_sha256\t" options:NSBackwardsSearch];
    if (signatureRange.location == NSNotFound) return false;
    NSData *unsignedData = [[text substringToIndex:signatureRange.location]
        dataUsingEncoding:NSUTF8StringEncoding];
    NSMutableData *authenticated = [NSMutableData data];
    const char magic[] = "spaceterm.performance.tail-complete/v1";
    [authenticated appendBytes:magic length:sizeof(magic)];
    uint64_t length = CFSwapInt64HostToBig((uint64_t)unsignedData.length);
    [authenticated appendBytes:&length length:sizeof(length)];
    [authenticated appendData:unsignedData];
    uint8_t digest[CC_SHA256_DIGEST_LENGTH];
    CCHmac(kCCHmacAlgSHA256, secret.bytes, secret.length,
        authenticated.bytes, authenticated.length, digest);
    NSData *digestData = [NSData dataWithBytes:digest length:sizeof(digest)];
    NSString *expected = hex_data(digestData).lowercaseString;
    if (![expected isEqualToString:tail[@"tail_hmac_sha256"]]) return false;
    *campaignID = intent[@"campaign_id"];
    *sessionID = intent[@"session_id"];
    *campaignNonce = intent[@"nonce"];
    *runIntentSHA256 = intentHash;
    *subject = intent[@"subject"];
    return true;
}

static bool strict_normal_terminate(NSRunningApplication *application) {
    if (application == nil || application.terminated || ![application terminate]) return false;
    return true;
}

static NSData *performance_quit_receipt(
    const QuitCapture *quit,
    pid_t pid,
    uint64_t startSeconds,
    uint64_t startMicroseconds,
    uint64_t exitContinuousNS
) {
    if (!quit->requested || quit->token == nil || quit->campaignID == nil ||
        quit->sessionID == nil || quit->campaignNonce == nil || quit->runIntentSHA256 == nil ||
        quit->requestContinuousNS == 0 || exitContinuousNS < quit->requestContinuousNS) {
        return nil;
    }
    NSString *record = [NSString stringWithFormat:
        @"format_version\t1\ncampaign_id\t%@\nsession_id\t%@\nnonce\t%@\n"
         "run_intent_sha256\t%@\nsubject_process_pid\t%d\n"
         "subject_process_start_identity\t%llu:%llu\nquit_token\t%@\n"
         "request_continuous_ns\t%llu\nexit_continuous_ns\t%llu\n"
         "termination_method\tappkit-terminate\nruntime_closure_status\tconfirmed\n"
         "evidence_mode\tproduction\n"
         "status\tcompleted\n",
        quit->campaignID, quit->sessionID, quit->campaignNonce, quit->runIntentSHA256, pid,
        (unsigned long long)startSeconds, (unsigned long long)startMicroseconds, quit->token,
        (unsigned long long)quit->requestContinuousNS, (unsigned long long)exitContinuousNS];
    return [record dataUsingEncoding:NSUTF8StringEncoding];
}

static NSData *performance_subject_exit_receipt(
    const Options *options,
    const QuitCapture *quit,
    pid_t pid,
    uint64_t startSeconds,
    uint64_t startMicroseconds,
    uint64_t exitContinuousNS,
    NSData *quitReceipt,
    NSData *nativeObservation
) {
    NSData *secret = read_stable_secret_file(options->campaignSecret);
    NSData *tail = read_stable_private_file(options->tailReceipt, 64 * 1024);
    NSData *intentData = read_stable_private_file(options->runIntent, 64 * 1024);
    NSArray<NSString *> *intentKeys = @[
        @"format_version", @"subject", @"subject_identity_sha256", @"scenario",
        @"scenario_plan_sha256", @"workload_sha256", @"command_sha256",
        @"environment_sha256", @"font_sha256", @"initial_grid_sha256",
        @"measured_duration_ms", @"process_pid", @"process_start_identity",
        @"campaign_id", @"session_id", @"nonce",
        @"native_provisional_observation_sha256", @"evidence_mode", @"status"
    ];
    NSDictionary *intent = intentData == nil ? nil : parse_records(intentData, intentKeys);
    if (secret == nil || tail == nil || quitReceipt == nil || quit->subject == nil ||
        intent == nil) return nil;
    NSString *nativeHash = [quit->subject isEqualToString:@"spaceterm"]
        ? lower_sha256(nativeObservation).lowercaseString : @"not-applicable";
    NSString *prefix = [NSString stringWithFormat:
        @"schema\tspaceterm.acceptance.performance-subject-exit/v1\n"
         "subject\t%@\ncampaign_id\t%@\nsession_id\t%@\nnonce\t%@\n"
         "run_intent_sha256\t%@\nsubject_identity_sha256\t%@\n"
         "process_pid\t%d\nprocess_start_identity\t%llu:%llu\n"
         "tail_receipt_sha256\t%@\nquit_receipt_sha256\t%@\n"
         "exit_requested_continuous_ns\t%llu\nprocess_exited_continuous_ns\t%llu\n"
         "exit_status\tnormal\nnative_observation_sha256\t%@\n"
         "evidence_mode\tproduction\n"
         "auth_algorithm\thmac-sha256\n",
        quit->subject, quit->campaignID, quit->sessionID, quit->campaignNonce,
        quit->runIntentSHA256, intent[@"subject_identity_sha256"],
        pid, (unsigned long long)startSeconds, (unsigned long long)startMicroseconds,
        lower_sha256(tail).lowercaseString, lower_sha256(quitReceipt).lowercaseString,
        (unsigned long long)quit->requestContinuousNS, (unsigned long long)exitContinuousNS,
        nativeHash];
    NSData *prefixData = [prefix dataUsingEncoding:NSUTF8StringEncoding];
    NSData *statusData = [@"status\tcomplete\n" dataUsingEncoding:NSUTF8StringEncoding];
    NSMutableData *unsignedData = [prefixData mutableCopy];
    [unsignedData appendData:statusData];
    NSMutableData *authenticated = [NSMutableData data];
    const char magic[] = "spaceterm.acceptance.performance-subject-exit/v1";
    [authenticated appendBytes:magic length:sizeof(magic)];
    uint64_t length = CFSwapInt64HostToBig((uint64_t)unsignedData.length);
    [authenticated appendBytes:&length length:sizeof(length)];
    [authenticated appendData:unsignedData];
    uint8_t digest[CC_SHA256_DIGEST_LENGTH];
    CCHmac(kCCHmacAlgSHA256, secret.bytes, secret.length,
        authenticated.bytes, authenticated.length, digest);
    NSString *signature = hex_data([NSData dataWithBytes:digest length:sizeof(digest)]).lowercaseString;
    NSMutableData *record = [prefixData mutableCopy];
    [record appendData:[[NSString stringWithFormat:@"receipt_hmac_sha256\t%@\n", signature]
        dataUsingEncoding:NSUTF8StringEncoding]];
    [record appendData:statusData];
    return record;
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

static void terminate_exact_mounted_path_processes(
    NSString *expected_app,
    NSString *expected_path,
    const struct stat *expected_stat,
    const struct statfs *expected_fs
) {
    int capacity = proc_listallpids(NULL, 0);
    if (capacity <= 0) {
        return;
    }
    pid_t *pids = calloc((size_t)capacity, sizeof(pid_t));
    if (pids == NULL) {
        return;
    }
    int count = proc_listallpids(pids, capacity * (int)sizeof(pid_t));
    for (int index = 0; index < count; index++) {
        pid_t pid = pids[index];
        struct proc_bsdinfo process = {0};
        char path[PROC_PIDPATHINFO_MAXSIZE] = {0};
        if (pid <= 0 || proc_pidinfo(pid, PROC_PIDTBSDINFO, 0,
                &process, sizeof(process)) != sizeof(process) ||
            process.pbi_uid != geteuid() || proc_pidpath(pid, path, sizeof(path)) <= 0) {
            continue;
        }
        NSString *canonical = canonical_path([NSString stringWithUTF8String:path]);
        struct stat executable = {0};
        struct statfs filesystem = {0};
        if (![canonical isEqualToString:expected_path] ||
            stat(path, &executable) != 0 || statfs(path, &filesystem) != 0 ||
            executable.st_dev != expected_stat->st_dev ||
            executable.st_ino != expected_stat->st_ino ||
            filesystem.f_fsid.val[0] != expected_fs->f_fsid.val[0] ||
            filesystem.f_fsid.val[1] != expected_fs->f_fsid.val[1] ||
            (filesystem.f_flags & MNT_RDONLY) == 0) {
            continue;
        }
        NSRunningApplication *application =
            [NSRunningApplication runningApplicationWithProcessIdentifier:pid];
        if (application == nil ||
            ![canonical_path(application.bundleURL.path) isEqualToString:expected_app] ||
            ![canonical_path(application.executableURL.path) isEqualToString:expected_path]) {
            continue;
        }
        terminate_exact_application(application);
    }
    free(pids);
}

static int run_verifier(const Options *options) {
    int result = 1;
    int executable_fd = -1;
    int listener = -1;
    int peer = -1;
    int failure_control_fd = -1;
    int failure_status_fd = -1;
    int quit_control_fd = -1;
    int quit_status_fd = -1;
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
    NSString *provisional_observation = nil;
    RuntimeCapture runtime = {0};
    FailureCapture failure = {0};
    QuitCapture quit = {0};
    NSString *runtime_status = nil;
    NSString *runtime_samples_path = nil;
    NSString *runtime_events_path = nil;
    NSString *runtime_metadata_path = nil;
    NSString *failure_actions_path = nil;
    NSString *samples_sha256 = nil;
    NSString *events_sha256 = nil;
    NSString *failure_actions_sha256 = nil;
    NSString *metadata_sha256 = nil;
    NSData *metadata = nil;
    NSData *observation_data = nil;
    NSData *provisional_observation_data = nil;
    NSData *ax_subject_data = nil;
    NSString *provisional_observation_sha256 = nil;
    NSData *ack = nil;
    NSString *closed = nil;
    NSDate *runtime_deadline = nil;
    NSString *output_parent = nil;
    NSString *provisional_observation_path = nil;
    NSString *ax_subject_path = nil;
    NSString *failure_status_path = nil;
    NSString *quit_status_path = nil;
    audit_token_t final_token = INVALID_AUDIT_TOKEN_VALUE;
    socklen_t final_token_length = sizeof(final_token);
    bool samples_published = false;
    bool events_published = false;
    bool metadata_published = false;
    bool failure_actions_published = false;
    bool observation_published = false;
    bool provisional_observation_published = false;
    bool ax_subject_published = false;
    bool quit_receipt_published = false;
    bool subject_exit_receipt_published = false;
    bool launch_completion_delivered = false;

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
    failure_actions_path = [output_parent stringByAppendingPathComponent:@"failure-actions.tsv"];
    if (![options->failureControl isEqualToString:@"none"]) {
        failure_status_path = [options->failureControl stringByAppendingString:@".status"];
    }
    if (![options->quitControl isEqualToString:@"none"]) {
        quit_status_path = [options->quitControl stringByAppendingString:@".status"];
    }
    provisional_observation_path =
        [output_parent stringByAppendingPathComponent:@"native-observation-live.tsv"];
    ax_subject_path = [output_parent stringByAppendingPathComponent:@"ax-subject.tsv"];
    if (!output_is_absent(options->output) || !output_is_absent(runtime_samples_path) ||
        !output_is_absent(runtime_events_path) || !output_is_absent(runtime_metadata_path) ||
        !output_is_absent(failure_actions_path) ||
        !output_is_absent(provisional_observation_path) || !output_is_absent(ax_subject_path) ||
        (![options->quitControl isEqualToString:@"none"] &&
            (!output_is_absent(options->quitReceipt) ||
             !output_is_absent(options->subjectExitReceipt) ||
             !output_is_absent(options->quitControl) || !output_is_absent(quit_status_path)))) {
        report(@"runtime observation outputs must not already exist");
        goto cleanup;
    }
    failure_control_fd = create_failure_control(options->failureControl);
    if (failure_control_fd == -2) {
        goto cleanup;
    }
    if (failure_status_path != nil) {
        failure_status_fd = create_failure_control(failure_status_path);
        if (failure_status_fd < 0) {
            goto cleanup;
        }
    }
    quit_control_fd = create_failure_control(options->quitControl);
    if (quit_control_fd == -2) goto cleanup;
    if (quit_status_path != nil) {
        quit_status_fd = create_failure_control(quit_status_path);
        if (quit_status_fd < 0) goto cleanup;
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
    launch_completion_delivered =
        dispatch_semaphore_wait(launched, DISPATCH_TIME_NOW) == 0;
    if (launch_completion_delivered && (launch_error != nil || launched_application == nil ||
            launched_application.processIdentifier <= 0)) {
        report(@"LaunchServices immediately rejected the application launch");
        goto cleanup;
    }
    if (!wait_for_fd(listener, false, kProofTimeoutSeconds)) {
        terminate_exact_mounted_path_processes(
            expected_app, expected_path, &expected_stat, &expected_fs);
        report(interrupted ? @"LaunchServices launch was interrupted" :
            @"production app did not connect to the verifier");
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
    if (peer_pid <= 0 || pidversion <= 0 || audit_token_to_euid(token) != geteuid() ||
        !mapped_executable_matches(&token, expected_path, &expected_stat, &expected_fs)) {
        report(@"Unix peer is not the exact mounted application process");
        goto cleanup;
    }
    application = [NSRunningApplication runningApplicationWithProcessIdentifier:peer_pid];
    opened_app = canonical_path(application.bundleURL.path);
    opened_executable = canonical_path(application.executableURL.path);
    if (application == nil || ![opened_app isEqualToString:expected_app] ||
        ![opened_executable isEqualToString:expected_path]) {
        report(@"Unix peer does not resolve to the exact mounted application");
        goto cleanup;
    }
    if (!launch_completion_delivered) {
        launch_completion_delivered =
            dispatch_semaphore_wait(launched, DISPATCH_TIME_NOW) == 0;
    }
    pid_t completion_pid = launched_application == nil
        ? 0 : launched_application.processIdentifier;
    if (!launch_completion_permits_peer(
            launch_completion_delivered, launch_error != nil, completion_pid, peer_pid)) {
        report(@"LaunchServices completion contradicts the authenticated application peer");
        goto cleanup;
    }
    if (!live_signature_matches(&token, options, expected_path,
            &live_cdhash, &live_identifier, &live_team)) {
        report(@"Unix peer live signature does not match the mounted application");
        goto cleanup;
    }
    nonce = make_nonce();
    if (nonce == nil || !write_frame(peer, challenge_data(nonce, options, &expected_stat))) {
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
    struct proc_bsdinfo process = {0};
    if (proc_pidinfo(peer_pid, PROC_PIDTBSDINFO, 0, &process, sizeof(process)) !=
            sizeof(process) ||
        process.pbi_uid != geteuid()) {
        report(@"authenticated application process start identity is unavailable");
        goto cleanup;
    }
    uint64_t process_start_seconds = process.pbi_start_tvsec;
    uint64_t process_start_microseconds = process.pbi_start_tvusec;
    provisional_observation = final_observation(
        response, pidversion, &expected_fs, live_cdhash, live_identifier, live_team);
    provisional_observation_data =
        [provisional_observation dataUsingEncoding:NSUTF8StringEncoding];
    provisional_observation_sha256 = lower_sha256(provisional_observation_data);
    ax_subject_data = ax_subject_identity(options, nonce, expected_app, expected_path, peer_pid, &process,
        &expected_stat, &expected_fs, live_cdhash, live_identifier, live_team,
        provisional_observation_sha256);
    if (provisional_observation_data == nil ||
        !is_lower_hex(provisional_observation_sha256, 64) || ax_subject_data == nil ||
        !publish_exclusive(provisional_observation_path, provisional_observation_data,
            &provisional_observation_published) ||
        !publish_exclusive(ax_subject_path, ax_subject_data, &ax_subject_published)) {
        report(@"live accessibility subject identity could not be published");
        goto cleanup;
    }
    if (!mapped_executable_matches(&token, expected_path, &expected_stat, &expected_fs) ||
        proc_pidinfo(peer_pid, PROC_PIDTBSDINFO, 0, &process, sizeof(process)) !=
            sizeof(process) ||
        process.pbi_start_tvsec != process_start_seconds ||
        process.pbi_start_tvusec != process_start_microseconds ||
        !live_signature_matches(&token, options, expected_path,
            &live_cdhash, &live_identifier, &live_team)) {
        report(@"live accessibility subject changed while it was published");
        goto cleanup;
    }
    if (options->replay) {
        [application terminate];
    } else {
        fprintf(stderr,
            "authenticated mounted app is ready; AX subject: %s; quit it after acceptance completes\n",
            ax_subject_path.fileSystemRepresentation);
        if (failure_control_fd >= 0) {
            fprintf(stderr, "authenticated failure control is ready: %s; status: %s\n",
                options->failureControl.fileSystemRepresentation,
                failure_status_path.fileSystemRepresentation);
        }
    }
    runtime_deadline = [NSDate dateWithTimeIntervalSinceNow:
        options->replay ? kProofTimeoutSeconds : 12.0 * 60.0 * 60.0 + kProofTimeoutSeconds];

    if (!read_runtime_stream(peer, &runtime, &failure, failure_control_fd, failure_status_fd,
            quit_control_fd, quit_status_fd, &quit, application, &token, expected_path,
            &expected_stat, &expected_fs, peer_pid, pidversion, process_start_seconds,
            process_start_microseconds, nonce, options, &runtime_status, runtime_deadline)) {
        if (runtime_parse_diagnostic != nil) {
            report([@"runtime stream diagnostic: "
                stringByAppendingString:runtime_parse_diagnostic]);
        }
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
    failure_actions_sha256 = lower_sha256(failure.records);
    if (!is_lower_hex(samples_sha256, 64) || !is_lower_hex(events_sha256, 64) ||
        !is_lower_hex(failure_actions_sha256, 64)) {
        report(@"runtime observation checksums could not be computed");
        goto cleanup;
    }
    metadata = runtime_metadata(
        options, response, &runtime, runtime_status, samples_sha256, events_sha256,
        &failure, failure_actions_sha256);
    if (metadata == nil) {
        goto cleanup;
    }
    metadata_sha256 = lower_sha256(metadata);
    observation = final_observation_with_closure(
        final_observation(response, pidversion, &expected_fs, live_cdhash, live_identifier,
            live_team),
        provisional_observation_sha256, metadata_sha256, failure_actions_sha256,
        failure.nextSequence, failure.resultCount);
    observation_data = [observation dataUsingEncoding:NSUTF8StringEncoding];
    if (!is_lower_hex(metadata_sha256, 64) || observation_data == nil ||
        !publish_exclusive(runtime_samples_path, runtime.samples, &samples_published)) {
        goto cleanup;
    }
    if (!publish_exclusive(runtime_events_path, runtime.events, &events_published)) {
        goto cleanup;
    }
    if (!publish_exclusive(
            failure_actions_path, failure.records, &failure_actions_published)) {
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
    if (!options->externalLifecycle && [options->quitControl isEqualToString:@"none"]) {
        terminate_exact_application(application);
    } else {
        NSDate *terminationDeadline = [NSDate dateWithTimeIntervalSinceNow:kProofTimeoutSeconds];
        while (!application.terminated && [terminationDeadline timeIntervalSinceNow] > 0) {
            [NSThread sleepForTimeInterval:0.02];
        }
    }
    if (!application.terminated ||
        (!options->externalLifecycle && ![options->quitControl isEqualToString:@"none"] &&
            !quit.requested)) {
        report(@"observed application could not finish safely");
        goto cleanup;
    }
    if (![options->quitControl isEqualToString:@"none"]) {
        if (!write_failure_status(quit_status_fd, @"completed", quit.token)) {
            report(@"performance lifecycle completion could not be acknowledged");
            goto cleanup;
        }
        if (!options->externalLifecycle) {
            uint64_t exitContinuousNS = continuous_nanoseconds();
            NSData *quitReceipt = performance_quit_receipt(
                &quit, peer_pid, process_start_seconds, process_start_microseconds,
                exitContinuousNS);
            NSData *exitReceipt = performance_subject_exit_receipt(options, &quit, peer_pid,
                process_start_seconds, process_start_microseconds, exitContinuousNS,
                quitReceipt, observation_data);
            if (quitReceipt == nil || exitReceipt == nil ||
                !publish_exclusive(options->quitReceipt, quitReceipt, &quit_receipt_published) ||
                !publish_exclusive(options->subjectExitReceipt, exitReceipt,
                    &subject_exit_receipt_published)) {
                report(@"performance quit receipt could not be published");
                goto cleanup;
            }
        }
    }
    // The native observation is the commit marker. Publish it only after the app consumed the
    // acknowledgement, closed the authenticated stream, and terminated cleanly.
    if (!publish_exclusive(options->output, observation_data, &observation_published)) {
        goto cleanup;
    }
    result = 0;

cleanup:
    if (result != 0) {
        if (!options->externalLifecycle && [options->quitControl isEqualToString:@"none"]) {
            terminate_exact_application(application);
        }
        if (quit_receipt_published) unlink(options->quitReceipt.fileSystemRepresentation);
        if (subject_exit_receipt_published) {
            unlink(options->subjectExitReceipt.fileSystemRepresentation);
        }
        if (observation_published) unlink(options->output.fileSystemRepresentation);
        if (ax_subject_published) unlink(ax_subject_path.fileSystemRepresentation);
        if (provisional_observation_published) {
            unlink(provisional_observation_path.fileSystemRepresentation);
        }
        if (metadata_published) unlink(runtime_metadata_path.fileSystemRepresentation);
        if (failure_actions_published) unlink(failure_actions_path.fileSystemRepresentation);
        if (events_published) unlink(runtime_events_path.fileSystemRepresentation);
        if (samples_published) unlink(runtime_samples_path.fileSystemRepresentation);
    }
    if (peer >= 0) close(peer);
    if (failure_control_fd >= 0) close(failure_control_fd);
    if (failure_status_fd >= 0) close(failure_status_fd);
    if (quit_control_fd >= 0) close(quit_control_fd);
    if (quit_status_fd >= 0) close(quit_status_fd);
    if (listener >= 0) close(listener);
    if (executable_fd >= 0) close(executable_fd);
    if (socket_path != nil) unlink(socket_path.fileSystemRepresentation);
    if (![options->failureControl isEqualToString:@"none"]) {
        unlink(options->failureControl.fileSystemRepresentation);
    }
    if (failure_status_path != nil) unlink(failure_status_path.fileSystemRepresentation);
    if (quit_status_path != nil) unlink(quit_status_path.fileSystemRepresentation);
    if (![options->quitControl isEqualToString:@"none"]) {
        unlink(options->quitControl.fileSystemRepresentation);
    }
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

static NSData *self_test_failure_result(
    NSString *requestID,
    NSString *sequence,
    NSString *caseID,
    NSString *action,
    NSString *result,
    NSString *paneState,
    NSString *failureClass,
    NSString *recoverability,
    NSString *operation,
    NSString *latest,
    NSString *lastValid,
    NSString *visible,
    NSString *pending,
    NSString *inputUsable,
    NSString *sessionAttached
) {
    NSString *frame = [NSString stringWithFormat:
        @"schema\t%@\nrequest.id\t%@\nsequence\t%@\ncase.id\t%@\naction\t%@\n"
         "result\t%@\npane.id\t1\npane.state\t%@\nfailure.class\t%@\n"
         "failure.recoverability\t%@\nfailure.operation\t%@\nstate.revision\t1\n"
         "latest.generation\t%@\nlast_valid.generation\t%@\nvisible.generation\t%@\n"
         "pending_recovery\t%@\nterminal_input_usable\t%@\nsession_attached\t%@\n"
         "resource.staged_count\t0\nresource.staged_bytes\t0\n"
         "resource.rolled_back_count\t0\nresource.rolled_back_bytes\t0\n",
        kFailureActionResultSchema, requestID, sequence, caseID, action, result, paneState,
        failureClass, recoverability, operation, latest, lastValid, visible, pending,
        inputUsable, sessionAttached];
    return [frame dataUsingEncoding:NSUTF8StringEncoding];
}

static bool self_test_ax_subject_schema(void) {
    Options options = {
        .runID = @"i43-ax",
        .appSHA256 = [@"a" stringByPaddingToLength:64 withString:@"a" startingAtIndex:0],
        .executable = @"/Volumes/SpaceTerm/SpaceTerm.app/Contents/MacOS/SpaceTerm",
    };
    struct proc_bsdinfo process = {0};
    process.pbi_start_tvsec = 11;
    process.pbi_start_tvusec = 12;
    struct stat executable = {0};
    executable.st_dev = 13;
    executable.st_ino = 14;
    struct statfs filesystem = {0};
    filesystem.f_fsid.val[0] = 15;
    filesystem.f_fsid.val[1] = -16;
    NSString *nonce = [@"b" stringByPaddingToLength:64 withString:@"b" startingAtIndex:0];
    NSString *observationSHA =
        [@"c" stringByPaddingToLength:64 withString:@"c" startingAtIndex:0];
    NSData *data = ax_subject_identity(&options, nonce,
        @"/System/Applications/Utilities/Terminal.app",
        @"/System/Applications/Utilities/Terminal.app/Contents/MacOS/SpaceTerm",
        17, &process, &executable,
        &filesystem, @"0123456789ABCDEF0123456789ABCDEF01234567", @"test.identifier", @"",
        observationSHA);
    if (data == nil) {
        return false;
    }
    NSArray<NSString *> *keys = @[
        @"schema", @"run.id", @"launch.nonce", @"package.app.sha256", @"package.app.path",
        @"package.app.bundle.identifier", @"package.app.executable.path", @"process.pid",
        @"process.start.tv-sec", @"process.start.tv-usec", @"process.executable.device",
        @"process.executable.inode", @"process.executable.fsid", @"process.signature.cdhash",
        @"process.signature.identifier", @"process.signature.team-identifier",
        @"process.mount.read-only", @"launch.controller", @"launch.source",
        @"launch.observation.sha256", @"launch.observation.complete"
    ];
    NSDictionary<NSString *, NSString *> *records = parse_records(data, keys);
    return records != nil && [records[@"schema"] isEqualToString:kAXSubjectSchema] &&
        [records[@"package.app.bundle.identifier"] isEqualToString:@"com.apple.Terminal"] &&
        [records[@"package.app.executable.path"] isEqualToString:
            @"/System/Applications/Utilities/Terminal.app/Contents/MacOS/SpaceTerm"] &&
        [records[@"process.start.tv-sec"] isEqualToString:@"11"] &&
        [records[@"process.start.tv-usec"] isEqualToString:@"12"] &&
        [records[@"process.executable.fsid"] isEqualToString:@"15:-16"] &&
        [records[@"process.signature.cdhash"]
            isEqualToString:@"0123456789abcdef0123456789abcdef01234567"] &&
        [records[@"launch.observation.sha256"] isEqualToString:observationSHA];
}

static bool self_test_launch_completion_authority(void) {
    return launch_completion_permits_peer(false, false, 0, 123) &&
        launch_completion_permits_peer(true, false, 123, 123) &&
        !launch_completion_permits_peer(true, true, 123, 123) &&
        !launch_completion_permits_peer(true, false, 0, 123) &&
        !launch_completion_permits_peer(true, false, 124, 123);
}

static bool initialize_self_test_failure_capture(
    FailureCapture *capture,
    NSString *caseID
) {
    if (!initialize_failure_capture(capture)) {
        return false;
    }
    capture->pendingRequestID = [@"a" stringByPaddingToLength:64
        withString:@"a" startingAtIndex:0];
    capture->pendingCaseID = caseID;
    capture->pendingClientToken = [@"f" stringByPaddingToLength:64
        withString:@"f" startingAtIndex:0];
    capture->pendingSequence = 0;
    return true;
}

static int verifier_self_test(void) {
    NSArray<NSString *> *emptyEvents = @[];
    NSMutableArray<NSString *> *sample = self_test_sample(@"1000000000", @"running");
    NSData *base = self_test_tick(@"0", @"0", sample, emptyEvents);
    if (!self_test_launch_completion_authority()) {
        return self_test_failure(@"missing, delayed, or contradictory launch completion") ? 0 : 1;
    }
    if (!self_test_ax_subject_schema()) {
        return self_test_failure(@"AX subject exact schema") ? 0 : 1;
    }

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

    NSString *requestID = [@"a" stringByPaddingToLength:64
        withString:@"a" startingAtIndex:0];
    FailureCapture failure = {0};
    if (!initialize_self_test_failure_capture(&failure, @"presentation-invalid-scale") ||
        !parse_failure_result(self_test_failure_result(requestID, @"0",
            @"presentation-invalid-scale", @"armed", @"accepted", @"running", @"none",
            @"none", @"none", @"4", @"4", @"4", @"none", @"1", @"1"), &failure) ||
        parse_failure_result(self_test_failure_result(requestID, @"0",
            @"presentation-invalid-scale", @"armed", @"accepted", @"running", @"none",
            @"none", @"none", @"4", @"4", @"4", @"none", @"1", @"1"), &failure)) {
        return self_test_failure(@"failure action replay or order") ? 0 : 1;
    }
    if (!parse_failure_result(self_test_failure_result(requestID, @"0",
            @"presentation-invalid-scale", @"injected", @"failed-state", @"failed",
            @"presentation", @"recoverable", @"update-backing-scale", @"4", @"4",
            @"4", @"presentation", @"1", @"1"), &failure) ||
        parse_failure_result(self_test_failure_result(requestID, @"0",
            @"presentation-invalid-scale", @"completed", @"recovered", @"running",
            @"none", @"none", @"none", @"4", @"4", @"4", @"none", @"1", @"1"),
            &failure) ||
        !parse_failure_result(self_test_failure_result(requestID, @"0",
            @"presentation-invalid-scale", @"retry-requested", @"accepted", @"failed",
            @"presentation", @"recoverable", @"update-backing-scale", @"4", @"4",
            @"4", @"presentation", @"1", @"1"), &failure) ||
        !parse_failure_result(self_test_failure_result(requestID, @"0",
            @"presentation-invalid-scale", @"completed", @"recovered", @"running",
            @"none", @"none", @"none", @"4", @"4", @"4", @"none", @"1", @"1"),
            &failure) || failure.pendingRequestID != nil) {
        return self_test_failure(@"failure action recovery sequence") ? 0 : 1;
    }
    FailureCapture paneBound = {0};
    if (!initialize_self_test_failure_capture(&paneBound, @"presentation-invalid-scale") ||
        !parse_failure_result(self_test_failure_result(requestID, @"0",
            @"presentation-invalid-scale", @"armed", @"accepted", @"running", @"none",
            @"none", @"none", @"4", @"4", @"4", @"none", @"1", @"1"), &paneBound)) {
        return self_test_failure(@"pane binding setup") ? 0 : 1;
    }
    NSString *switchedPane = [[[NSString alloc] initWithData:self_test_failure_result(requestID,
        @"0", @"presentation-invalid-scale", @"injected", @"failed-state", @"failed",
        @"presentation", @"recoverable", @"update-backing-scale", @"4", @"4", @"4",
        @"presentation", @"1", @"1") encoding:NSUTF8StringEncoding]
        stringByReplacingOccurrencesOfString:@"pane.id\t1\n" withString:@"pane.id\t2\n"];
    if (parse_failure_result(
            [switchedPane dataUsingEncoding:NSUTF8StringEncoding], &paneBound)) {
        return self_test_failure(@"failure action switched Pane") ? 0 : 1;
    }
    FailureCapture revisionBound = {0};
    NSString *armedRevisionTwo = [[[NSString alloc] initWithData:self_test_failure_result(requestID,
        @"0", @"presentation-invalid-scale", @"armed", @"accepted", @"running", @"none",
        @"none", @"none", @"4", @"4", @"4", @"none", @"1", @"1")
        encoding:NSUTF8StringEncoding]
        stringByReplacingOccurrencesOfString:@"state.revision\t1\n"
        withString:@"state.revision\t2\n"];
    if (!initialize_self_test_failure_capture(&revisionBound, @"presentation-invalid-scale") ||
        !parse_failure_result(
            [armedRevisionTwo dataUsingEncoding:NSUTF8StringEncoding], &revisionBound) ||
        parse_failure_result(self_test_failure_result(requestID, @"0",
            @"presentation-invalid-scale", @"injected", @"failed-state", @"failed",
            @"presentation", @"recoverable", @"update-backing-scale", @"4", @"4", @"4",
            @"presentation", @"1", @"1"), &revisionBound)) {
        return self_test_failure(@"failure action revision regression") ? 0 : 1;
    }
    FailureCapture fatal = {0};
    if (!initialize_self_test_failure_capture(&fatal, @"pty-fatal") ||
        !parse_failure_result(self_test_failure_result(requestID, @"0", @"pty-fatal",
            @"armed", @"accepted", @"running", @"none", @"none", @"none", @"7", @"7",
            @"7", @"none", @"1", @"1"), &fatal) ||
        !parse_failure_result(self_test_failure_result(requestID, @"0", @"pty-fatal",
            @"injected", @"failed-state", @"failed", @"pty", @"fatal",
            @"read-shell-output", @"7", @"7", @"7", @"none", @"0", @"1"), &fatal) ||
        parse_failure_result(self_test_failure_result(requestID, @"0", @"pty-fatal",
            @"completed", @"closed", @"failed", @"pty", @"fatal", @"read-shell-output",
            @"7", @"7", @"7", @"none", @"0", @"1"), &fatal) ||
        !parse_failure_result(self_test_failure_result(requestID, @"0", @"pty-fatal",
            @"completed", @"closed", @"failed", @"pty", @"fatal", @"read-shell-output",
            @"7", @"7", @"7", @"none", @"0", @"0"), &fatal)) {
        return self_test_failure(@"fatal close authentication") ? 0 : 1;
    }
    FailureCapture stagedRollback = {0};
    if (!initialize_self_test_failure_capture(
            &stagedRollback, @"renderer-resource-after-staging") ||
        !parse_failure_result(self_test_failure_result(requestID, @"0",
            @"renderer-resource-after-staging", @"armed", @"accepted", @"running",
            @"none", @"none", @"none", @"5", @"5", @"5", @"none", @"1", @"1"),
            &stagedRollback)) {
        return self_test_failure(@"resource rollback setup") ? 0 : 1;
    }
    NSData *zeroRollback = self_test_failure_result(requestID, @"0",
        @"renderer-resource-after-staging", @"injected", @"failed-state", @"failed",
        @"resource", @"recoverable", @"prepare-terminal-graphics", @"5", @"5", @"5",
        @"renderer-resources", @"1", @"1");
    NSString *rollbackText = [[NSString alloc] initWithData:zeroRollback
        encoding:NSUTF8StringEncoding];
    rollbackText = [rollbackText stringByReplacingOccurrencesOfString:
        @"resource.staged_count\t0\nresource.staged_bytes\t0\n"
         "resource.rolled_back_count\t0\nresource.rolled_back_bytes\t0\n"
        withString:
        @"resource.staged_count\t1\nresource.staged_bytes\t4\n"
         "resource.rolled_back_count\t1\nresource.rolled_back_bytes\t4\n"];
    if (parse_failure_result(zeroRollback, &stagedRollback) ||
        !parse_failure_result(
            [rollbackText dataUsingEncoding:NSUTF8StringEncoding], &stagedRollback)) {
        return self_test_failure(@"resource rollback proof") ? 0 : 1;
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
