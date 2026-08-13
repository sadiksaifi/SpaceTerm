#import <AppKit/AppKit.h>
#import <ApplicationServices/ApplicationServices.h>
#import <CommonCrypto/CommonDigest.h>
#import <Security/Security.h>

#include <errno.h>
#include <bsm/libbsm.h>
#include <fcntl.h>
#include <libproc.h>
#include <locale.h>
#include <mach/mach.h>
#include <mach/mach_time.h>
#include <math.h>
#include <signal.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/proc_info.h>
#include <sys/proc.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <time.h>
#include <unistd.h>

static const NSUInteger kMaximumPlanBytes = 1024 * 1024;
static const NSUInteger kMaximumEventIdentifierBytes = 96;
static const NSUInteger kMaximumPlanEvents = 4096;
static const uint64_t kMaximumPlanOffsetMilliseconds = 720000;
static const uint64_t kResizeFrameNanoseconds = 1000000000ULL / 60ULL;
static const NSUInteger kResizeHalfFrames = 26;
static const uint64_t kResizeMaximumLatenessNanoseconds = 20000000ULL;
static const CGFloat kResizeDeltaTolerance = 8.0;
static const CGFloat kRestorationTolerance = 1.0;
static const uint64_t kMaximumDispatchLatenessNanoseconds = 250000000ULL;
static const int64_t kDriverEventTag = 0x5350414345544552LL;
static volatile sig_atomic_t gInterrupted = 0;

static void handle_signal(int signal_number) {
    (void)signal_number;
    gInterrupted = 1;
}

static void print_usage(FILE *stream) {
    fprintf(stream,
            "Usage: performance-driver --pid PID --start-identity STRING \\\n\n"
            "  --executable PATH --executable-sha256 SHA256 --app-bundle PATH \\\n\n"
            "  --bundle-identifier ID --signing-identifier ID \\\n\n"
            "  --team-identifier ID|none|- --cdhash HEX --window-number N \\\n\n"
            "  --scenario-plan PATH --plan-start-continuous-ns N --output PATH\n\n"
            "Consume a frozen native-performance scenario plan and atomically emit\n"
            "driver-events.tsv. Synthetic input is posted only to the pinned PID;\n"
            "the driver never posts to a global event tap. Accessibility permission\n"
            "is required for exact-window minimize, restore, and resize actions.\n");
}

static BOOL set_error(NSString **error, NSString *value) {
    if (error != NULL) {
        *error = value;
    }
    return NO;
}

static BOOL string_is_safe_field(NSString *value, NSUInteger maximum_bytes) {
    if (value.length == 0 || [value lengthOfBytesUsingEncoding:NSUTF8StringEncoding] > maximum_bytes) {
        return NO;
    }
    NSCharacterSet *unsafe = [NSCharacterSet characterSetWithCharactersInString:@"\t\r\n\0"];
    return [value rangeOfCharacterFromSet:unsafe].location == NSNotFound;
}

static BOOL string_is_safe_label(NSString *value) {
    if (!string_is_safe_field(value, 255)) {
        return NO;
    }
    NSCharacterSet *allowed = [NSCharacterSet characterSetWithCharactersInString:
        @"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._-"];
    return [[value stringByTrimmingCharactersInSet:allowed] length] == 0;
}

static BOOL string_is_event_identifier(NSString *value) {
    if (!string_is_safe_field(value, kMaximumEventIdentifierBytes)) {
        return NO;
    }
    NSCharacterSet *first = [NSCharacterSet alphanumericCharacterSet];
    if (![first characterIsMember:[value characterAtIndex:0]]) {
        return NO;
    }
    NSCharacterSet *allowed = [NSCharacterSet characterSetWithCharactersInString:
        @"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._:-"];
    return [[value stringByTrimmingCharactersInSet:allowed] length] == 0;
}

static BOOL string_is_hex(NSString *value, NSUInteger minimum, NSUInteger maximum) {
    if (value.length < minimum || value.length > maximum || value.length % 2 != 0) {
        return NO;
    }
    NSCharacterSet *hex = [NSCharacterSet characterSetWithCharactersInString:@"0123456789abcdef"];
    return [[value stringByTrimmingCharactersInSet:hex] length] == 0;
}

static BOOL parse_unsigned(NSString *value, uint64_t maximum, uint64_t *parsed) {
    if (value.length == 0 || [value characterAtIndex:0] == '+') {
        return NO;
    }
    const char *bytes = value.UTF8String;
    if (bytes == NULL) {
        return NO;
    }
    errno = 0;
    char *end = NULL;
    unsigned long long number = strtoull(bytes, &end, 10);
    if (errno != 0 || end == bytes || *end != '\0' || number > maximum) {
        return NO;
    }
    *parsed = (uint64_t)number;
    return YES;
}

static BOOL parse_signed(NSString *value, int64_t minimum, int64_t maximum, int64_t *parsed) {
    if (value.length == 0 || [value characterAtIndex:0] == '+') {
        return NO;
    }
    const char *bytes = value.UTF8String;
    if (bytes == NULL) {
        return NO;
    }
    errno = 0;
    char *end = NULL;
    long long number = strtoll(bytes, &end, 10);
    if (errno != 0 || end == bytes || *end != '\0' || number < minimum || number > maximum) {
        return NO;
    }
    *parsed = (int64_t)number;
    return YES;
}

static BOOL parse_start_identity(NSString *value, uint64_t *seconds,
                                 uint64_t *microseconds) {
    NSArray<NSString *> *fields = [value componentsSeparatedByString:@":"];
    return fields.count == 2
        && parse_unsigned(fields[0], UINT64_MAX, seconds)
        && parse_unsigned(fields[1], 999999, microseconds);
}

static NSString *canonical_path(NSString *path) {
    if (path.length == 0) {
        return nil;
    }
    char *resolved = realpath(path.fileSystemRepresentation, NULL);
    if (resolved == NULL) {
        return nil;
    }
    NSString *value = [[NSFileManager defaultManager]
        stringWithFileSystemRepresentation:resolved
        length:strlen(resolved)];
    free(resolved);
    return value;
}

static BOOL rect_matches(CGRect observed, CGRect expected, CGFloat tolerance) {
    return fabs(observed.origin.x - expected.origin.x) <= tolerance
        && fabs(observed.origin.y - expected.origin.y) <= tolerance
        && fabs(observed.size.width - expected.size.width) <= tolerance
        && fabs(observed.size.height - expected.size.height) <= tolerance;
}

static BOOL same_stat_identity(const struct stat *left, const struct stat *right) {
    return left->st_dev == right->st_dev
        && left->st_ino == right->st_ino
        && left->st_mode == right->st_mode
        && left->st_size == right->st_size
        && left->st_mtimespec.tv_sec == right->st_mtimespec.tv_sec
        && left->st_mtimespec.tv_nsec == right->st_mtimespec.tv_nsec
        && left->st_ctimespec.tv_sec == right->st_ctimespec.tv_sec
        && left->st_ctimespec.tv_nsec == right->st_ctimespec.tv_nsec;
}

static NSString *sha256_for_fd(int fd, NSString **error) {
    if (lseek(fd, 0, SEEK_SET) < 0) {
        set_error(error, @"executable-hash-seek-failed");
        return nil;
    }
    CC_SHA256_CTX context;
    CC_SHA256_Init(&context);
    uint8_t buffer[64 * 1024];
    while (true) {
        ssize_t count = read(fd, buffer, sizeof(buffer));
        if (count == 0) {
            break;
        }
        if (count < 0) {
            if (errno == EINTR) {
                continue;
            }
            set_error(error, @"executable-hash-read-failed");
            return nil;
        }
        CC_SHA256_Update(&context, buffer, (CC_LONG)count);
    }
    uint8_t digest[CC_SHA256_DIGEST_LENGTH];
    CC_SHA256_Final(digest, &context);
    NSMutableString *hex = [NSMutableString stringWithCapacity:CC_SHA256_DIGEST_LENGTH * 2];
    for (NSUInteger index = 0; index < CC_SHA256_DIGEST_LENGTH; index += 1) {
        [hex appendFormat:@"%02x", digest[index]];
    }
    return hex;
}

static NSString *hex_for_data(NSData *data) {
    const uint8_t *bytes = data.bytes;
    NSMutableString *hex = [NSMutableString stringWithCapacity:data.length * 2];
    for (NSUInteger index = 0; index < data.length; index += 1) {
        [hex appendFormat:@"%02x", bytes[index]];
    }
    return hex;
}

static uint64_t continuous_nanoseconds(void) {
    static mach_timebase_info_data_t timebase;
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        mach_timebase_info(&timebase);
    });
    __uint128_t scaled = (__uint128_t)mach_continuous_time() * timebase.numer;
    return (uint64_t)(scaled / timebase.denom);
}

static BOOL wait_until_continuous(uint64_t deadline) {
    while (!gInterrupted) {
        uint64_t now = continuous_nanoseconds();
        if (now >= deadline) {
            return YES;
        }
        uint64_t remaining = deadline - now;
        uint64_t slice = remaining < 10000000ULL ? remaining : 10000000ULL;
        struct timespec request = {
            .tv_sec = (time_t)(slice / 1000000000ULL),
            .tv_nsec = (long)(slice % 1000000000ULL),
        };
        while (nanosleep(&request, &request) != 0 && errno == EINTR && !gInterrupted) {
        }
    }
    return NO;
}

static void pump_run_loop(uint64_t milliseconds) {
    uint64_t deadline = continuous_nanoseconds() + milliseconds * 1000000ULL;
    while (!gInterrupted && continuous_nanoseconds() < deadline) {
        @autoreleasepool {
            [[NSRunLoop currentRunLoop]
                runMode:NSDefaultRunLoopMode
                beforeDate:[NSDate dateWithTimeIntervalSinceNow:0.01]];
        }
    }
}

@interface ScenarioEvent : NSObject
@property(nonatomic, copy) NSString *identifier;
@property(nonatomic) uint64_t offsetMilliseconds;
@property(nonatomic, copy) NSString *action;
@property(nonatomic, copy) NSString *argument0;
@property(nonatomic, copy) NSString *argument1;
@end

@implementation ScenarioEvent
@end

@interface ActionObservation : NSObject
@property(nonatomic) BOOL succeeded;
@property(nonatomic) uint64_t eventNanoseconds;
@property(nonatomic) int64_t requestedA;
@property(nonatomic) int64_t requestedB;
@property(nonatomic) int64_t observedA;
@property(nonatomic) int64_t observedB;
@property(nonatomic, copy) NSString *result;
@end

@implementation ActionObservation
@end

static ActionObservation *observation(BOOL succeeded,
                                      int64_t requested_a,
                                      int64_t requested_b,
                                      int64_t observed_a,
                                      int64_t observed_b,
                                      NSString *result) {
    ActionObservation *value = [ActionObservation new];
    value.succeeded = succeeded;
    value.eventNanoseconds = continuous_nanoseconds();
    value.requestedA = requested_a;
    value.requestedB = requested_b;
    value.observedA = observed_a;
    value.observedB = observed_b;
    value.result = result;
    return value;
}

static NSData *read_frozen_plan(NSString *path, NSString **error) {
    int fd = open(path.fileSystemRepresentation, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    if (fd < 0) {
        set_error(error, @"scenario-plan-open-failed");
        return nil;
    }
    struct stat before;
    if (fstat(fd, &before) != 0 || !S_ISREG(before.st_mode) || (before.st_mode & 0222) != 0
        || before.st_size <= 0
        || before.st_size > (off_t)kMaximumPlanBytes) {
        close(fd);
        set_error(error, @"scenario-plan-not-bounded-regular-file");
        return nil;
    }
    NSMutableData *data = [NSMutableData dataWithLength:(NSUInteger)before.st_size];
    uint8_t *cursor = data.mutableBytes;
    size_t remaining = (size_t)before.st_size;
    while (remaining > 0) {
        ssize_t count = read(fd, cursor, remaining);
        if (count < 0 && errno == EINTR) {
            continue;
        }
        if (count <= 0) {
            close(fd);
            set_error(error, @"scenario-plan-read-failed");
            return nil;
        }
        cursor += count;
        remaining -= (size_t)count;
    }
    uint8_t extra;
    ssize_t extra_count;
    do {
        extra_count = read(fd, &extra, sizeof(extra));
    } while (extra_count < 0 && errno == EINTR);
    struct stat after;
    BOOL unchanged = extra_count == 0 && fstat(fd, &after) == 0
        && same_stat_identity(&before, &after);
    close(fd);
    if (!unchanged) {
        set_error(error, @"scenario-plan-changed-during-read");
        return nil;
    }
    if (memchr(data.bytes, '\0', data.length) != NULL
        || memchr(data.bytes, '\r', data.length) != NULL) {
        set_error(error, @"scenario-plan-invalid-line-encoding");
        return nil;
    }
    return data;
}

static NSArray<ScenarioEvent *> *parse_plan(NSString *path, NSString **error) {
    NSData *data = read_frozen_plan(path, error);
    if (data == nil) {
        return nil;
    }
    NSString *text = [[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding];
    if (text == nil || ![text hasSuffix:@"\n"]) {
        set_error(error, @"scenario-plan-must-be-utf8-with-final-newline");
        return nil;
    }
    NSMutableArray<NSString *> *lines = [[text componentsSeparatedByString:@"\n"] mutableCopy];
    [lines removeLastObject];
    if (lines.count < 2
        || ![lines[0] isEqualToString:@"event_id\toffset_ms\taction\targ0\targ1"]) {
        set_error(error, @"scenario-plan-header-mismatch");
        return nil;
    }

    NSMutableArray<ScenarioEvent *> *events = [NSMutableArray array];
    NSMutableSet<NSString *> *identifiers = [NSMutableSet set];
    uint64_t prior_offset = 0;
    BOOL has_prior_offset = NO;
    BOOL minimized = NO;
    BOOL occluder_visible = NO;
    NSSet<NSString *> *actions = [NSSet setWithArray:@[
        @"input", @"scroll-rows", @"minimize", @"restore", @"occluder-show",
        @"occluder-hide", @"resize-grid", @"checkpoint", @"stop",
    ]];

    for (NSUInteger line_index = 1; line_index < lines.count; line_index += 1) {
        if (line_index > kMaximumPlanEvents) {
            set_error(error, @"scenario-plan-too-many-events");
            return nil;
        }
        NSArray<NSString *> *fields = [lines[line_index] componentsSeparatedByString:@"\t"];
        if (fields.count != 5) {
            set_error(error, @"scenario-plan-row-width-mismatch");
            return nil;
        }
        NSString *identifier = fields[0];
        NSString *offset_field = fields[1];
        NSString *action = fields[2];
        NSString *argument0 = fields[3];
        NSString *argument1 = fields[4];
        uint64_t offset = 0;
        if (!string_is_event_identifier(identifier) || [identifiers containsObject:identifier]) {
            set_error(error, @"scenario-plan-event-id-invalid-or-duplicate");
            return nil;
        }
        if (!parse_unsigned(offset_field, kMaximumPlanOffsetMilliseconds, &offset)
            || (has_prior_offset && offset < prior_offset)) {
            set_error(error, @"scenario-plan-offset-decreased");
            return nil;
        }
        if (![actions containsObject:action]) {
            set_error(error, @"scenario-plan-action-unsupported");
            return nil;
        }

        int64_t numeric0 = 0;
        int64_t numeric1 = 0;
        if ([action isEqualToString:@"input"]) {
            NSSet<NSString *> *safe_tokens = [NSSet setWithArray:@[
                @"0", @"a", @"return", @"left", @"right", @"up", @"down",
                @"escape", @"space", @"backspace",
            ]];
            if (![safe_tokens containsObject:argument0]
                || !parse_signed(argument1, 0, 64, &numeric1)) {
                set_error(error, @"scenario-plan-input-arguments-invalid");
                return nil;
            }
        } else if ([action isEqualToString:@"scroll-rows"]) {
            if (!parse_signed(argument0, -1000, 1000, &numeric0) || numeric0 == 0
                || ![argument1 isEqualToString:@"0"]) {
                set_error(error, @"scenario-plan-scroll-arguments-invalid");
                return nil;
            }
        } else if ([action isEqualToString:@"resize-grid"]) {
            if (!parse_signed(argument0, -512, 512, &numeric0)
                || !parse_signed(argument1, -512, 512, &numeric1)
                || (numeric0 == 0 && numeric1 == 0)) {
                set_error(error, @"scenario-plan-resize-arguments-invalid");
                return nil;
            }
        } else if ([action isEqualToString:@"checkpoint"]) {
            if (!parse_signed(argument0, 0, INT32_MAX, &numeric0)
                || !parse_signed(argument1, 0, INT32_MAX, &numeric1)) {
                set_error(error, @"scenario-plan-checkpoint-arguments-invalid");
                return nil;
            }
        } else if (![argument0 isEqualToString:@"0"]
                   || ![argument1 isEqualToString:@"0"]) {
            set_error(error, @"scenario-plan-placeholder-arguments-invalid");
            return nil;
        }

        if ([action isEqualToString:@"minimize"]) {
            if (minimized || occluder_visible) {
                set_error(error, @"scenario-plan-minimize-state-invalid");
                return nil;
            }
            minimized = YES;
        } else if ([action isEqualToString:@"restore"]) {
            if (!minimized) {
                set_error(error, @"scenario-plan-restore-state-invalid");
                return nil;
            }
            minimized = NO;
        } else if ([action isEqualToString:@"occluder-show"]) {
            if (minimized || occluder_visible) {
                set_error(error, @"scenario-plan-occluder-show-state-invalid");
                return nil;
            }
            occluder_visible = YES;
        } else if ([action isEqualToString:@"occluder-hide"]) {
            if (!occluder_visible) {
                set_error(error, @"scenario-plan-occluder-hide-state-invalid");
                return nil;
            }
            occluder_visible = NO;
        } else if ([action isEqualToString:@"resize-grid"] && minimized) {
            set_error(error, @"scenario-plan-resize-while-minimized");
            return nil;
        } else if ([action isEqualToString:@"stop"]
                   && (minimized || occluder_visible || line_index + 1 != lines.count)) {
            set_error(error, @"scenario-plan-stop-must-be-final-and-restored");
            return nil;
        }

        ScenarioEvent *event = [ScenarioEvent new];
        event.identifier = identifier;
        event.offsetMilliseconds = offset;
        event.action = action;
        event.argument0 = argument0;
        event.argument1 = argument1;
        [events addObject:event];
        [identifiers addObject:identifier];
        prior_offset = offset;
        has_prior_offset = YES;
    }
    if (![[events lastObject].action isEqualToString:@"stop"]) {
        set_error(error, @"scenario-plan-missing-final-stop");
        return nil;
    }
    return events;
}

static NSDictionary *copy_window_info(CGWindowID window_number) {
    CFArrayRef raw = CGWindowListCopyWindowInfo(
        kCGWindowListOptionIncludingWindow | kCGWindowListExcludeDesktopElements,
        window_number);
    if (raw == NULL) {
        return nil;
    }
    NSArray *items = CFBridgingRelease(raw);
    if (items.count != 1 || ![items[0] isKindOfClass:[NSDictionary class]]) {
        return nil;
    }
    return items[0];
}

static BOOL window_bounds(NSDictionary *info, CGRect *bounds) {
    id representation = info[(__bridge id)kCGWindowBounds];
    if (![representation isKindOfClass:[NSDictionary class]]) {
        return NO;
    }
    return CGRectMakeWithDictionaryRepresentation((__bridge CFDictionaryRef)representation, bounds);
}

static NSInteger window_integer(NSDictionary *info, CFStringRef key, NSInteger fallback) {
    id value = info[(__bridge id)key];
    return [value isKindOfClass:[NSNumber class]] ? [value integerValue] : fallback;
}

static BOOL ax_window_number(AXUIElementRef window, CGWindowID *number) {
    CFTypeRef raw = NULL;
    AXError status = AXUIElementCopyAttributeValue(window, CFSTR("AXWindowNumber"), &raw);
    if (status != kAXErrorSuccess || raw == NULL) {
        if (raw != NULL) {
            CFRelease(raw);
        }
        return NO;
    }
    BOOL valid = CFGetTypeID(raw) == CFNumberGetTypeID();
    int64_t value = 0;
    if (valid) {
        valid = CFNumberGetValue((CFNumberRef)raw, kCFNumberSInt64Type, &value)
            && value > 0 && value <= UINT32_MAX;
    }
    CFRelease(raw);
    if (!valid) {
        return NO;
    }
    *number = (CGWindowID)value;
    return YES;
}

static BOOL ax_boolean(AXUIElementRef element, CFStringRef attribute, BOOL *value) {
    CFTypeRef raw = NULL;
    AXError status = AXUIElementCopyAttributeValue(element, attribute, &raw);
    if (status != kAXErrorSuccess || raw == NULL || CFGetTypeID(raw) != CFBooleanGetTypeID()) {
        if (raw != NULL) {
            CFRelease(raw);
        }
        return NO;
    }
    *value = CFBooleanGetValue((CFBooleanRef)raw);
    CFRelease(raw);
    return YES;
}

@interface DriverTarget : NSObject {
    struct stat _executableStat;
    struct stat _appBundleStat;
    struct stat _infoPlistStat;
    audit_token_t _auditToken;
    int _pidVersion;
    CGRect _initialWindowBounds;
    BOOL _hasInitialWindowBounds;
    AXUIElementRef _applicationElement;
    AXUIElementRef _windowElement;
    CGEventSourceRef _eventSource;
}
@property(nonatomic) pid_t pid;
@property(nonatomic) uint64_t startSeconds;
@property(nonatomic) uint64_t startMicroseconds;
@property(nonatomic, copy) NSString *startIdentity;
@property(nonatomic) CGWindowID windowNumber;
@property(nonatomic, copy) NSString *executablePath;
@property(nonatomic, copy) NSString *expectedExecutableSHA256;
@property(nonatomic, copy) NSString *appBundlePath;
@property(nonatomic, copy) NSString *bundleIdentifier;
@property(nonatomic, copy) NSString *signingIdentifier;
@property(nonatomic, copy, nullable) NSString *teamIdentifier;
@property(nonatomic, copy) NSString *cdhash;
@property(nonatomic) int executableFD;
@property(nonatomic, strong) NSRunningApplication *runningApplication;
@property(nonatomic, strong, nullable) NSPanel *occluder;
- (BOOL)prepare:(NSString **)error;
- (BOOL)verifyIdentity:(NSString **)error;
- (BOOL)verifyFastIdentity:(NSString **)error;
- (BOOL)verifyKernelExecution:(NSString **)error;
- (BOOL)verifyWindow:(NSString **)error;
- (BOOL)windowIsMinimized:(BOOL *)minimized;
- (BOOL)activateAndRaise:(NSString **)error;
- (BOOL)restoreWindowBounds:(CGRect)bounds;
- (BOOL)verifyOccluder:(NSString **)error;
- (ActionObservation *)execute:(ScenarioEvent *)event;
- (BOOL)restoreSafeState;
@end

@implementation DriverTarget

- (instancetype)init {
    self = [super init];
    if (self != nil) {
        _executableFD = -1;
    }
    return self;
}

- (void)dealloc {
    if (_eventSource != NULL) {
        CFRelease(_eventSource);
    }
    if (_windowElement != NULL) {
        CFRelease(_windowElement);
    }
    if (_applicationElement != NULL) {
        CFRelease(_applicationElement);
    }
    if (_executableFD >= 0) {
        close(_executableFD);
    }
}

- (BOOL)readProcessInformation:(struct proc_bsdinfo *)information error:(NSString **)error {
    memset(information, 0, sizeof(*information));
    int count = proc_pidinfo(self.pid,
                             PROC_PIDTBSDINFO,
                             0,
                             information,
                             sizeof(*information));
    if (count != sizeof(*information) || information->pbi_status == SZOMB
        || information->pbi_uid != geteuid()) {
        return set_error(error, @"target-process-identity-mismatch");
    }
    return YES;
}

- (BOOL)captureProcessStart:(NSString **)error {
    struct proc_bsdinfo information;
    if (![self readProcessInformation:&information error:error]) {
        return NO;
    }
    uint64_t expected_seconds = 0;
    uint64_t expected_microseconds = 0;
    if (!parse_start_identity(self.startIdentity, &expected_seconds,
                              &expected_microseconds)
        || expected_seconds != information.pbi_start_tvsec
        || expected_microseconds != information.pbi_start_tvusec) {
        return set_error(error, @"target-process-start-identity-mismatch");
    }
    self.startSeconds = information.pbi_start_tvsec;
    self.startMicroseconds = information.pbi_start_tvusec;

    mach_port_t task = MACH_PORT_NULL;
    mach_msg_type_number_t token_count = TASK_AUDIT_TOKEN_COUNT;
    kern_return_t status = task_name_for_pid(mach_task_self(), self.pid, &task);
    if (status != KERN_SUCCESS || task == MACH_PORT_NULL) {
        return set_error(error, @"target-audit-token-unavailable");
    }
    status = task_info(task,
                       TASK_AUDIT_TOKEN,
                       (task_info_t)&_auditToken,
                       &token_count);
    mach_port_deallocate(mach_task_self(), task);
    _pidVersion = audit_token_to_pidversion(_auditToken);
    if (status != KERN_SUCCESS || token_count != TASK_AUDIT_TOKEN_COUNT
        || audit_token_to_pid(_auditToken) != self.pid
        || audit_token_to_euid(_auditToken) != geteuid()
        || _pidVersion <= 0) {
        return set_error(error, @"target-audit-token-invalid");
    }
    return YES;
}

- (BOOL)verifyKernelExecution:(NSString **)error {
    struct proc_bsdinfo information;
    if (![self readProcessInformation:&information error:error]
        || information.pbi_start_tvsec != self.startSeconds
        || information.pbi_start_tvusec != self.startMicroseconds
        || audit_token_to_pid(_auditToken) != self.pid
        || audit_token_to_pidversion(_auditToken) != _pidVersion
        || audit_token_to_euid(_auditToken) != geteuid()) {
        return set_error(error, @"target-process-identity-mismatch");
    }
    return YES;
}

- (BOOL)verifyProcess:(NSString **)error {
    if (![self verifyKernelExecution:error]) {
        return NO;
    }
    char buffer[PROC_PIDPATHINFO_MAXSIZE];
    int length = proc_pidpath_audittoken(&_auditToken, buffer, sizeof(buffer));
    if (length <= 0 || (size_t)length >= sizeof(buffer)) {
        return set_error(error, @"target-executable-path-unavailable");
    }
    buffer[length] = '\0';
    NSString *reported = [[NSFileManager defaultManager]
        stringWithFileSystemRepresentation:buffer
        length:(NSUInteger)length];
    NSString *reported_canonical = canonical_path(reported);
    if (reported_canonical == nil || ![reported_canonical isEqualToString:self.executablePath]) {
        return set_error(error, @"target-executable-path-mismatch");
    }
    return YES;
}

- (BOOL)verifyExecutableVnode:(NSString **)error {
    struct stat descriptor_stat;
    struct stat path_stat;
    if (fstat(self.executableFD, &descriptor_stat) != 0
        || stat(self.executablePath.fileSystemRepresentation, &path_stat) != 0
        || !same_stat_identity(&_executableStat, &descriptor_stat)
        || !same_stat_identity(&_executableStat, &path_stat)) {
        return set_error(error, @"target-executable-vnode-changed");
    }
    return YES;
}

- (BOOL)verifyBundleVnodes:(NSString **)error {
    struct stat app;
    struct stat info;
    NSString *info_path = [self.appBundlePath stringByAppendingPathComponent:@"Contents/Info.plist"];
    if (stat(self.appBundlePath.fileSystemRepresentation, &app) != 0
        || stat(info_path.fileSystemRepresentation, &info) != 0
        || !same_stat_identity(&_appBundleStat, &app)
        || !same_stat_identity(&_infoPlistStat, &info)) {
        return set_error(error, @"target-bundle-vnode-changed");
    }
    return YES;
}

- (BOOL)verifyRunningCode:(NSString **)error {
    NSData *token = [NSData dataWithBytes:&_auditToken length:sizeof(_auditToken)];
    NSDictionary *attributes = @{(__bridge id)kSecGuestAttributeAudit: token};
    SecCodeRef code = NULL;
    OSStatus status = SecCodeCopyGuestWithAttributes(
        NULL,
        (__bridge CFDictionaryRef)attributes,
        kSecCSDefaultFlags,
        &code);
    if (status != errSecSuccess || code == NULL) {
        return set_error(error, @"target-running-code-unavailable");
    }
    status = SecCodeCheckValidity(code, kSecCSStrictValidate, NULL);
    if (status != errSecSuccess) {
        CFRelease(code);
        return set_error(error, @"target-running-code-invalid");
    }
    CFDictionaryRef raw_information = NULL;
    status = SecCodeCopySigningInformation(
        code,
        kSecCSSigningInformation | kSecCSDynamicInformation,
        &raw_information);
    CFRelease(code);
    if (status != errSecSuccess || raw_information == NULL) {
        return set_error(error, @"target-signing-information-unavailable");
    }
    NSDictionary *information = CFBridgingRelease(raw_information);
    NSString *identifier = information[(__bridge id)kSecCodeInfoIdentifier];
    NSString *team = information[(__bridge id)kSecCodeInfoTeamIdentifier];
    NSData *unique = information[(__bridge id)kSecCodeInfoUnique];
    NSURL *main_executable = information[(__bridge id)kSecCodeInfoMainExecutable];
    NSString *main_path = [main_executable isKindOfClass:[NSURL class]]
        ? canonical_path(main_executable.path)
        : nil;
    BOOL team_matches = self.teamIdentifier == nil ? team == nil
        : [team isKindOfClass:[NSString class]] && [team isEqualToString:self.teamIdentifier];
    if (![identifier isKindOfClass:[NSString class]]
        || ![identifier isEqualToString:self.signingIdentifier]
        || !team_matches
        || ![unique isKindOfClass:[NSData class]]
        || ![[hex_for_data(unique) lowercaseString] isEqualToString:self.cdhash]
        || main_path == nil
        || ![main_path isEqualToString:self.executablePath]) {
        return set_error(error, @"target-signing-identity-mismatch");
    }
    return YES;
}

- (BOOL)verifyStaticBundle:(NSString **)error {
    NSURL *url = [NSURL fileURLWithPath:self.appBundlePath isDirectory:YES];
    SecStaticCodeRef code = NULL;
    OSStatus status = SecStaticCodeCreateWithPath(
        (__bridge CFURLRef)url,
        kSecCSDefaultFlags,
        &code);
    if (status != errSecSuccess || code == NULL) {
        return set_error(error, @"target-static-code-unavailable");
    }
    CFErrorRef validation_error = NULL;
    status = SecStaticCodeCheckValidityWithErrors(code,
                                                  kSecCSStrictValidate | kSecCSCheckAllArchitectures,
                                                  NULL,
                                                  &validation_error);
    if (validation_error != NULL) {
        CFRelease(validation_error);
    }
    if (status != errSecSuccess) {
        CFRelease(code);
        return set_error(error, @"target-static-code-invalid");
    }
    CFDictionaryRef raw = NULL;
    status = SecCodeCopySigningInformation(code, kSecCSSigningInformation, &raw);
    CFRelease(code);
    if (status != errSecSuccess || raw == NULL) {
        return set_error(error, @"target-static-signing-information-unavailable");
    }
    NSDictionary *information = CFBridgingRelease(raw);
    NSString *identifier = information[(__bridge id)kSecCodeInfoIdentifier];
    NSString *team = information[(__bridge id)kSecCodeInfoTeamIdentifier];
    NSData *unique = information[(__bridge id)kSecCodeInfoUnique];
    BOOL team_matches = self.teamIdentifier == nil ? team == nil
        : [team isKindOfClass:[NSString class]] && [team isEqualToString:self.teamIdentifier];
    if (![identifier isEqualToString:self.signingIdentifier]
        || !team_matches
        || ![unique isKindOfClass:[NSData class]]
        || ![[hex_for_data(unique) lowercaseString] isEqualToString:self.cdhash]) {
        return set_error(error, @"target-static-signing-identity-mismatch");
    }
    return YES;
}

- (BOOL)verifyBundle:(NSString **)error {
    NSRunningApplication *running = [NSRunningApplication runningApplicationWithProcessIdentifier:self.pid];
    if (running == nil || running.terminated
        || ![running.bundleIdentifier isEqualToString:self.bundleIdentifier]) {
        return set_error(error, @"target-bundle-identity-mismatch");
    }
    NSString *bundle_path = canonical_path(running.bundleURL.path);
    NSString *executable_path = canonical_path(running.executableURL.path);
    if (bundle_path == nil || executable_path == nil
        || ![bundle_path isEqualToString:self.appBundlePath]
        || ![executable_path isEqualToString:self.executablePath]) {
        return set_error(error, @"target-bundle-path-mismatch");
    }
    self.runningApplication = running;
    return YES;
}

- (BOOL)verifyIdentity:(NSString **)error {
    return [self verifyFastIdentity:error]
        && [self verifyExecutableVnode:error]
        && [self verifyRunningCode:error]
        && [self verifyBundle:error]
        && [self verifyStaticBundle:error];
}

- (BOOL)verifyFastIdentity:(NSString **)error {
    return [self verifyProcess:error]
        && [self verifyExecutableVnode:error]
        && [self verifyBundleVnodes:error];
}

- (BOOL)verifyWindow:(NSString **)error {
    NSDictionary *info = copy_window_info(self.windowNumber);
    NSInteger owner = window_integer(info, kCGWindowOwnerPID, -1);
    NSInteger number = window_integer(info, kCGWindowNumber, -1);
    NSInteger layer = window_integer(info, kCGWindowLayer, -1);
    if (info == nil || owner != self.pid || number != self.windowNumber || layer != 0) {
        return set_error(error, @"target-window-identity-mismatch");
    }
    pid_t ax_pid = 0;
    CGWindowID ax_number = 0;
    if (_windowElement == NULL
        || AXUIElementGetPid(_windowElement, &ax_pid) != kAXErrorSuccess
        || ax_pid != self.pid
        || !ax_window_number(_windowElement, &ax_number)
        || ax_number != self.windowNumber) {
        return set_error(error, @"target-accessibility-window-mismatch");
    }
    return YES;
}

- (BOOL)findAccessibilityWindow:(NSString **)error {
    _applicationElement = AXUIElementCreateApplication(self.pid);
    if (_applicationElement == NULL) {
        return set_error(error, @"target-accessibility-application-unavailable");
    }
    CFTypeRef raw_windows = NULL;
    AXError status = AXUIElementCopyAttributeValue(
        _applicationElement,
        kAXWindowsAttribute,
        &raw_windows);
    if (status != kAXErrorSuccess || raw_windows == NULL
        || CFGetTypeID(raw_windows) != CFArrayGetTypeID()) {
        if (raw_windows != NULL) {
            CFRelease(raw_windows);
        }
        return set_error(error, @"target-accessibility-windows-unavailable");
    }
    CFArrayRef windows = (CFArrayRef)raw_windows;
    CFIndex match_count = 0;
    AXUIElementRef match = NULL;
    for (CFIndex index = 0; index < CFArrayGetCount(windows); index += 1) {
        CFTypeRef candidate = CFArrayGetValueAtIndex(windows, index);
        if (CFGetTypeID(candidate) != AXUIElementGetTypeID()) {
            continue;
        }
        CGWindowID candidate_number = 0;
        if (ax_window_number((AXUIElementRef)candidate, &candidate_number)
            && candidate_number == self.windowNumber) {
            match_count += 1;
            match = (AXUIElementRef)candidate;
        }
    }
    if (match_count == 1) {
        _windowElement = (AXUIElementRef)CFRetain(match);
    }
    CFRelease(raw_windows);
    if (match_count != 1) {
        return set_error(error, @"target-accessibility-window-not-unique");
    }
    return YES;
}

- (BOOL)windowIsMinimized:(BOOL *)minimized {
    return ax_boolean(_windowElement, kAXMinimizedAttribute, minimized);
}

- (BOOL)windowBounds:(CGRect *)bounds {
    return window_bounds(copy_window_info(self.windowNumber), bounds);
}

- (BOOL)restoreWindowBounds:(CGRect)bounds {
    NSString *identity_error = nil;
    if (![self verifyFastIdentity:&identity_error] || _windowElement == NULL) {
        return NO;
    }
    CGPoint position = bounds.origin;
    CGSize size = bounds.size;
    AXValueRef position_value = AXValueCreate(kAXValueCGPointType, &position);
    AXValueRef size_value = AXValueCreate(kAXValueCGSizeType, &size);
    if (position_value == NULL || size_value == NULL) {
        if (position_value != NULL) CFRelease(position_value);
        if (size_value != NULL) CFRelease(size_value);
        return NO;
    }
    BOOL set = AXUIElementSetAttributeValue(_windowElement, kAXPositionAttribute, position_value)
            == kAXErrorSuccess
        && AXUIElementSetAttributeValue(_windowElement, kAXSizeAttribute, size_value)
            == kAXErrorSuccess;
    CFRelease(position_value);
    CFRelease(size_value);
    if (!set) {
        return NO;
    }
    uint64_t deadline = continuous_nanoseconds() + 1000000000ULL;
    while (!gInterrupted && continuous_nanoseconds() < deadline) {
        CGRect observed;
        if ([self windowBounds:&observed]
            && rect_matches(observed, bounds, kRestorationTolerance)) {
            return YES;
        }
        pump_run_loop(10);
    }
    return NO;
}

- (BOOL)focusedWindowMatches {
    CFTypeRef raw = NULL;
    AXError status = AXUIElementCopyAttributeValue(
        _applicationElement,
        kAXFocusedWindowAttribute,
        &raw);
    if (status != kAXErrorSuccess || raw == NULL
        || CFGetTypeID(raw) != AXUIElementGetTypeID()) {
        if (raw != NULL) {
            CFRelease(raw);
        }
        return NO;
    }
    CGWindowID number = 0;
    BOOL matches = ax_window_number((AXUIElementRef)raw, &number)
        && number == self.windowNumber;
    CFRelease(raw);
    return matches;
}

- (BOOL)activateAndRaise:(NSString **)error {
    if (![self.runningApplication activateWithOptions:0]) {
        return set_error(error, @"target-activation-failed");
    }
    if (AXUIElementPerformAction(_windowElement, kAXRaiseAction) != kAXErrorSuccess) {
        return set_error(error, @"target-window-raise-failed");
    }
    uint64_t deadline = continuous_nanoseconds() + 2000000000ULL;
    while (!gInterrupted && continuous_nanoseconds() < deadline) {
        if (self.runningApplication.active && [self focusedWindowMatches]) {
            return YES;
        }
        pump_run_loop(10);
    }
    return set_error(error, gInterrupted ? @"interrupted" : @"target-focus-not-observed");
}

- (BOOL)prepare:(NSString **)error {
    if (!AXIsProcessTrusted()) {
        return set_error(error, @"accessibility-permission-required");
    }
    self.executableFD = open(
        self.executablePath.fileSystemRepresentation,
        O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    if (self.executableFD < 0 || fstat(self.executableFD, &_executableStat) != 0
        || !S_ISREG(_executableStat.st_mode)) {
        return set_error(error, @"target-executable-open-failed");
    }
    NSString *actual_hash = sha256_for_fd(self.executableFD, error);
    if (actual_hash == nil || ![actual_hash isEqualToString:self.expectedExecutableSHA256]) {
        return set_error(error, @"target-executable-hash-mismatch");
    }
    NSString *info_path = [self.appBundlePath stringByAppendingPathComponent:@"Contents/Info.plist"];
    if (stat(self.appBundlePath.fileSystemRepresentation, &_appBundleStat) != 0
        || !S_ISDIR(_appBundleStat.st_mode)
        || stat(info_path.fileSystemRepresentation, &_infoPlistStat) != 0
        || !S_ISREG(_infoPlistStat.st_mode)) {
        return set_error(error, @"target-bundle-vnode-unavailable");
    }
    if (![self captureProcessStart:error]
        || ![self verifyIdentity:error] || ![self findAccessibilityWindow:error]
        || ![self verifyWindow:error]) {
        return NO;
    }
    BOOL minimized = NO;
    if (![self windowIsMinimized:&minimized] || minimized) {
        return set_error(error, @"target-window-must-start-restored");
    }
    Boolean size_settable = false;
    Boolean position_settable = false;
    if (AXUIElementIsAttributeSettable(_windowElement, kAXSizeAttribute, &size_settable)
            != kAXErrorSuccess
        || AXUIElementIsAttributeSettable(_windowElement, kAXPositionAttribute, &position_settable)
            != kAXErrorSuccess
        || !size_settable || !position_settable
        || ![self windowBounds:&_initialWindowBounds]) {
        return set_error(error, @"target-window-is-not-restorable");
    }
    _hasInitialWindowBounds = YES;
    _eventSource = CGEventSourceCreate(kCGEventSourceStatePrivate);
    if (_eventSource == NULL) {
        return set_error(error, @"private-event-source-unavailable");
    }
    CGEventSourceSetLocalEventsSuppressionInterval(_eventSource, 0.0);
    return [self activateAndRaise:error];
}

- (void)tagEvent:(CGEventRef)event {
    CGEventSetIntegerValueField(event, kCGEventSourceUserData, kDriverEventTag);
}

- (BOOL)postKeyCode:(CGKeyCode)key_code down:(BOOL)down {
    NSString *identity_error = nil;
    if (![self verifyKernelExecution:&identity_error]) {
        return NO;
    }
    CGEventRef event = CGEventCreateKeyboardEvent(_eventSource, key_code, down);
    if (event == NULL) {
        return NO;
    }
    [self tagEvent:event];
    CGEventPostToPid(self.pid, event);
    CFRelease(event);
    return [self verifyKernelExecution:&identity_error];
}

- (BOOL)postUnicode:(NSString *)text down:(BOOL)down {
    NSString *identity_error = nil;
    if (![self verifyKernelExecution:&identity_error]) {
        return NO;
    }
    NSUInteger length = text.length;
    if (length == 0 || length > 255) {
        return NO;
    }
    UniChar characters[255];
    [text getCharacters:characters range:NSMakeRange(0, length)];
    CGEventRef event = CGEventCreateKeyboardEvent(_eventSource, 0, down);
    if (event == NULL) {
        return NO;
    }
    CGEventKeyboardSetUnicodeString(event, length, characters);
    [self tagEvent:event];
    CGEventPostToPid(self.pid, event);
    CFRelease(event);
    return [self verifyKernelExecution:&identity_error];
}

- (ActionObservation *)executeInput:(ScenarioEvent *)event {
    NSString *failure = nil;
    BOOL minimized = NO;
    if (![self windowIsMinimized:&minimized]) {
        return observation(NO, 0, 0, 0, 0, @"minimized-state-unavailable");
    }
    if (!minimized && ![self activateAndRaise:&failure]) {
        return observation(NO, 0, 0, 0, 0, failure);
    }
    int64_t repeat_count = 0;
    parse_signed(event.argument1, 0, 64, &repeat_count);
    if ([event.argument0 isEqualToString:@"0"]) {
        NSString *frame = [NSString stringWithFormat:@"SPACETERM-PERF-INPUT %@", event.identifier];
        int64_t requested_units = (int64_t)frame.length + 1;
        uint64_t injection_time = continuous_nanoseconds();
        BOOL posted = [self postUnicode:frame down:YES]
            && [self postUnicode:frame down:NO]
            && [self postKeyCode:36 down:YES]
            && [self postKeyCode:36 down:NO];
        ActionObservation *result = observation(
            posted,
            requested_units,
            4,
            0,
            self.runningApplication.active && [self focusedWindowMatches],
            posted ? @"verified" : @"event-construction-failed");
        result.eventNanoseconds = injection_time;
        return result;
    }

    NSDictionary<NSString *, NSNumber *> *key_codes = @{
        @"a": @0,
        @"return": @36,
        @"left": @123,
        @"right": @124,
        @"down": @125,
        @"up": @126,
        @"escape": @53,
        @"space": @49,
        @"backspace": @51,
    };
    CGKeyCode key_code = (CGKeyCode)key_codes[event.argument0].unsignedShortValue;
    if (repeat_count == 0) {
        repeat_count = 1;
    }
    uint64_t injection_time = continuous_nanoseconds();
    int64_t posted_count = 0;
    for (int64_t index = 0; index < repeat_count && !gInterrupted; index += 1) {
        if (![self postKeyCode:key_code down:YES] || ![self postKeyCode:key_code down:NO]) {
            break;
        }
        posted_count += 2;
    }
    BOOL succeeded = posted_count == repeat_count * 2 && !gInterrupted;
    ActionObservation *result = observation(
        succeeded,
        repeat_count,
        repeat_count * 2,
        0,
        self.runningApplication.active && [self focusedWindowMatches],
        succeeded ? @"verified" : (gInterrupted ? @"interrupted" : @"event-construction-failed"));
    result.eventNanoseconds = injection_time;
    return result;
}

- (ActionObservation *)executeScroll:(ScenarioEvent *)event {
    NSString *failure = nil;
    BOOL minimized = NO;
    if (![self windowIsMinimized:&minimized]) {
        return observation(NO, 0, 0, 0, 0, @"minimized-state-unavailable");
    }
    if (!minimized && ![self activateAndRaise:&failure]) {
        return observation(NO, 0, 0, 0, 0, failure);
    }
    int64_t rows = 0;
    parse_signed(event.argument0, -1000, 1000, &rows);
    CGEventRef scroll = CGEventCreateScrollWheelEvent(
        _eventSource,
        kCGScrollEventUnitLine,
        1,
        (int32_t)rows);
    if (scroll == NULL) {
        return observation(NO, rows, 0, 0, 0, @"event-construction-failed");
    }
    NSString *identity_error = nil;
    if (![self verifyKernelExecution:&identity_error]) {
        CFRelease(scroll);
        return observation(NO, rows, 0, 0, 0, identity_error);
    }
    [self tagEvent:scroll];
    uint64_t injection_time = continuous_nanoseconds();
    CGEventPostToPid(self.pid, scroll);
    CFRelease(scroll);
    BOOL identity_preserved = [self verifyKernelExecution:&identity_error];
    ActionObservation *result = observation(
        identity_preserved,
        rows,
        0,
        0,
        self.runningApplication.active && [self focusedWindowMatches],
        identity_preserved ? @"verified" : identity_error);
    result.eventNanoseconds = injection_time;
    return result;
}

- (ActionObservation *)setMinimized:(BOOL)minimized {
    AXError status = AXUIElementSetAttributeValue(
        _windowElement,
        kAXMinimizedAttribute,
        minimized ? kCFBooleanTrue : kCFBooleanFalse);
    if (status != kAXErrorSuccess) {
        return observation(NO, minimized, 0, 0, 0, @"minimized-state-set-failed");
    }
    if (!minimized) {
        NSString *failure = nil;
        if (![self activateAndRaise:&failure]) {
            return observation(NO, minimized, 0, 0, 0, failure);
        }
    }
    uint64_t deadline = continuous_nanoseconds() + 2000000000ULL;
    BOOL observed = !minimized;
    NSUInteger stable_observations = 0;
    while (!gInterrupted && continuous_nanoseconds() < deadline) {
        NSDictionary *info = copy_window_info(self.windowNumber);
        NSInteger onscreen = window_integer(info, kCGWindowIsOnscreen, -1);
        BOOL native_matches = [self windowIsMinimized:&observed]
            && observed == minimized
            && onscreen == (minimized ? 0 : 1);
        stable_observations = native_matches ? stable_observations + 1 : 0;
        if (stable_observations >= 3) {
            return observation(YES, minimized, 0, observed, onscreen, @"verified");
        }
        pump_run_loop(10);
    }
    return observation(NO,
                       minimized,
                       0,
                       observed,
                       0,
                       gInterrupted ? @"interrupted" : @"minimized-state-not-observed");
}

static NSRect appkit_rect_for_quartz_rect(CGRect quartz) {
    CGRect primary = CGDisplayBounds(CGMainDisplayID());
    return NSMakeRect(quartz.origin.x,
                      CGRectGetMaxY(primary) - CGRectGetMaxY(quartz),
                      quartz.size.width,
                      quartz.size.height);
}

- (ActionObservation *)showOccluder {
    if (self.occluder != nil) {
        return observation(NO, 1, 0, self.occluder.visible, 0, @"occluder-already-visible");
    }
    NSDictionary *target_info = copy_window_info(self.windowNumber);
    CGRect target_bounds;
    if (!window_bounds(target_info, &target_bounds)
        || window_integer(target_info, kCGWindowIsOnscreen, 0) != 1) {
        return observation(NO, 1, 0, 0, 0, @"target-window-not-onscreen");
    }
    NSPanel *panel = [[NSPanel alloc]
        initWithContentRect:appkit_rect_for_quartz_rect(target_bounds)
        styleMask:NSWindowStyleMaskBorderless | NSWindowStyleMaskNonactivatingPanel
        backing:NSBackingStoreBuffered
        defer:NO];
    panel.opaque = YES;
    panel.backgroundColor = NSColor.blackColor;
    panel.alphaValue = 1.0;
    panel.hasShadow = NO;
    panel.ignoresMouseEvents = YES;
    panel.releasedWhenClosed = NO;
    panel.level = NSFloatingWindowLevel + 1;
    panel.collectionBehavior = NSWindowCollectionBehaviorTransient
        | NSWindowCollectionBehaviorIgnoresCycle;
    [panel orderFrontRegardless];
    self.occluder = panel;
    pump_run_loop(100);

    CGWindowID occluder_number = (CGWindowID)panel.windowNumber;
    CFArrayRef raw_above = CGWindowListCopyWindowInfo(
        kCGWindowListOptionOnScreenAboveWindow | kCGWindowListExcludeDesktopElements,
        self.windowNumber);
    NSArray *above = CFBridgingRelease(raw_above);
    BOOL found_above = NO;
    for (NSDictionary *info in above) {
        if (window_integer(info, kCGWindowOwnerPID, -1) == getpid()
            && window_integer(info, kCGWindowNumber, -1) == occluder_number) {
            found_above = YES;
            break;
        }
    }
    NSString *coverage_error = nil;
    BOOL succeeded = found_above && [self verifyOccluder:&coverage_error];
    BOOL visible = panel.visible;
    if (!succeeded) {
        [panel orderOut:nil];
        [panel close];
        self.occluder = nil;
    }
    ActionObservation *result = observation(
        succeeded,
        1,
        0,
        visible ? occluder_number : 0,
        found_above,
        succeeded ? @"verified"
            : (coverage_error != nil ? coverage_error : @"occluder-not-observed-above-target"));
    return result;
}

- (BOOL)verifyOccluder:(NSString **)error {
    if (self.occluder == nil) {
        return YES;
    }
    NSDictionary *target_info = copy_window_info(self.windowNumber);
    NSDictionary *occluder_info = copy_window_info((CGWindowID)self.occluder.windowNumber);
    CGRect target_bounds;
    CGRect occluder_bounds;
    NSNumber *alpha = occluder_info[(__bridge id)kCGWindowAlpha];
    if (!window_bounds(target_info, &target_bounds)
        || !window_bounds(occluder_info, &occluder_bounds)
        || window_integer(target_info, kCGWindowIsOnscreen, 0) != 1
        || window_integer(occluder_info, kCGWindowIsOnscreen, 0) != 1
        || window_integer(occluder_info, kCGWindowOwnerPID, -1) != getpid()
        || ![alpha isKindOfClass:[NSNumber class]]
        || alpha.doubleValue < 0.999
        || !CGRectContainsRect(CGRectInset(occluder_bounds, -1.0, -1.0), target_bounds)
        || !CGRectContainsRect(CGRectInset(target_bounds, -1.0, -1.0), occluder_bounds)) {
        return set_error(error, @"occluder-coverage-not-observed");
    }
    CFArrayRef raw_above = CGWindowListCopyWindowInfo(
        kCGWindowListOptionOnScreenAboveWindow | kCGWindowListExcludeDesktopElements,
        self.windowNumber);
    NSArray *above = CFBridgingRelease(raw_above);
    for (NSDictionary *info in above) {
        if (window_integer(info, kCGWindowOwnerPID, -1) == getpid()
            && window_integer(info, kCGWindowNumber, -1) == self.occluder.windowNumber) {
            return YES;
        }
    }
    return set_error(error, @"occluder-not-above-target");
}

- (ActionObservation *)hideOccluder {
    if (self.occluder == nil) {
        return observation(NO, 0, 0, 0, 0, @"occluder-not-visible");
    }
    CGWindowID number = (CGWindowID)self.occluder.windowNumber;
    [self.occluder orderOut:nil];
    [self.occluder close];
    self.occluder = nil;
    pump_run_loop(100);
    BOOL visible = YES;
    uint64_t deadline = continuous_nanoseconds() + 1000000000ULL;
    NSUInteger stable_observations = 0;
    while (!gInterrupted && continuous_nanoseconds() < deadline) {
        NSDictionary *info = copy_window_info(number);
        visible = window_integer(info, kCGWindowIsOnscreen, 0) == 1;
        stable_observations = visible ? 0 : stable_observations + 1;
        if (stable_observations >= 3) {
            break;
        }
        pump_run_loop(10);
    }
    ActionObservation *result = observation(
        !visible && stable_observations >= 3,
        0,
        0,
        visible,
        0,
        visible ? @"occluder-still-visible"
            : (stable_observations >= 3 ? @"verified" : @"occluder-hide-not-stable"));
    return result;
}

static BOOL point_is_on_active_display(CGPoint point) {
    CGDirectDisplayID displays[32];
    uint32_t count = 0;
    if (CGGetActiveDisplayList(32, displays, &count) != kCGErrorSuccess) {
        return NO;
    }
    for (uint32_t index = 0; index < count; index += 1) {
        if (CGRectContainsPoint(CGDisplayBounds(displays[index]), point)) {
            return YES;
        }
    }
    return NO;
}

- (BOOL)postMouseType:(CGEventType)type at:(CGPoint)point {
    NSString *identity_error = nil;
    if (![self verifyKernelExecution:&identity_error]) {
        return NO;
    }
    CGEventRef event = CGEventCreateMouseEvent(_eventSource, type, point, kCGMouseButtonLeft);
    if (event == NULL) {
        return NO;
    }
    CGEventSetIntegerValueField(event, kCGMouseEventClickState, 1);
    CGEventSetIntegerValueField(event, kCGMouseEventWindowUnderMousePointer, self.windowNumber);
    CGEventSetIntegerValueField(
        event,
        kCGMouseEventWindowUnderMousePointerThatCanHandleThisEvent,
        self.windowNumber);
    [self tagEvent:event];
    CGEventPostToPid(self.pid, event);
    CFRelease(event);
    return [self verifyKernelExecution:&identity_error];
}

- (ActionObservation *)executeResize:(ScenarioEvent *)event {
    int64_t requested_width = 0;
    int64_t requested_height = 0;
    parse_signed(event.argument0, -512, 512, &requested_width);
    parse_signed(event.argument1, -512, 512, &requested_height);
    NSString *failure = nil;
    if (![self activateAndRaise:&failure]) {
        return observation(NO, requested_width, requested_height, 0, 0, failure);
    }
    if (NSEvent.pressedMouseButtons != 0) {
        return observation(NO, requested_width, requested_height, 0, 0, @"physical-mouse-button-active");
    }
    NSDictionary *before_info = copy_window_info(self.windowNumber);
    CGRect before;
    if (!window_bounds(before_info, &before)
        || window_integer(before_info, kCGWindowIsOnscreen, 0) != 1) {
        return observation(NO, requested_width, requested_height, 0, 0, @"target-window-not-onscreen");
    }
    if (before.size.width + requested_width < 320
        || before.size.height + requested_height < 200) {
        return observation(NO, requested_width, requested_height, 0, 0, @"requested-resize-below-safe-minimum");
    }
    CGPoint start = CGPointMake(CGRectGetMaxX(before) - 2, CGRectGetMaxY(before) - 2);
    CGPoint outward = CGPointMake(start.x + requested_width, start.y + requested_height);
    if (!point_is_on_active_display(start) || !point_is_on_active_display(outward)) {
        return observation(NO, requested_width, requested_height, 0, 0, @"requested-resize-outside-active-display");
    }

    uint64_t action_time = continuous_nanoseconds();
    BOOL posted = [self postMouseType:kCGEventLeftMouseDown at:start];
    BOOL mouse_down = posted;
    uint64_t drag_start = continuous_nanoseconds();
    BOOL cadence_valid = YES;
    CGRect midpoint = before;
    for (NSUInteger index = 1; posted && index <= kResizeHalfFrames * 2; index += 1) {
        if (gInterrupted || NSEvent.pressedMouseButtons != 0) {
            posted = NO;
            break;
        }
        if (index == kResizeHalfFrames || index == kResizeHalfFrames * 2) {
            NSString *identity_failure = nil;
            if (![self verifyFastIdentity:&identity_failure] || ![self verifyWindow:&identity_failure]) {
                posted = NO;
                break;
            }
        }
        double progress = index <= kResizeHalfFrames
            ? (double)index / (double)kResizeHalfFrames
            : (double)(kResizeHalfFrames * 2 - index) / (double)kResizeHalfFrames;
        CGPoint point = CGPointMake(start.x + requested_width * progress,
                                    start.y + requested_height * progress);
        posted = [self postMouseType:kCGEventLeftMouseDragged at:point];
        if (index == kResizeHalfFrames) {
            pump_run_loop(5);
            window_bounds(copy_window_info(self.windowNumber), &midpoint);
        }
        uint64_t frame_deadline = drag_start + index * kResizeFrameNanoseconds;
        wait_until_continuous(frame_deadline);
        uint64_t observed_time = continuous_nanoseconds();
        if (observed_time > frame_deadline + kResizeMaximumLatenessNanoseconds) {
            cadence_valid = NO;
        }
    }
    if (mouse_down) {
        posted = [self postMouseType:kCGEventLeftMouseUp at:start] && posted;
    }
    pump_run_loop(100);
    CGRect after = before;
    BOOL has_after = window_bounds(copy_window_info(self.windowNumber), &after);
    int64_t observed_width = llround(midpoint.size.width - before.size.width);
    int64_t observed_height = llround(midpoint.size.height - before.size.height);
    BOOL restored = has_after
        && rect_matches(after, _initialWindowBounds, kRestorationTolerance);
    if (!restored) {
        restored = [self restoreWindowBounds:_initialWindowBounds];
        [self windowBounds:&after];
        restored = restored
            && rect_matches(after, _initialWindowBounds, kRestorationTolerance);
    }
    BOOL moved = fabs((CGFloat)observed_width - (CGFloat)requested_width)
            <= kResizeDeltaTolerance
        && fabs((CGFloat)observed_height - (CGFloat)requested_height)
            <= kResizeDeltaTolerance;
    BOOL succeeded = posted && restored && moved && cadence_valid && !gInterrupted;
    NSString *result = succeeded ? @"verified"
        : (gInterrupted ? @"interrupted"
           : (!posted ? @"resize-event-post-failed"
              : (!restored ? @"native-window-not-restored"
                 : (!moved ? @"native-window-delta-mismatch" : @"native-resize-cadence-missed"))));
    ActionObservation *value = observation(
        succeeded,
        requested_width,
        requested_height,
        observed_width,
        observed_height,
        result);
    value.eventNanoseconds = action_time;
    return value;
}

- (ActionObservation *)checkpoint:(ScenarioEvent *)event {
    int64_t requested_a = 0;
    int64_t requested_b = 0;
    parse_signed(event.argument0, 0, INT32_MAX, &requested_a);
    parse_signed(event.argument1, 0, INT32_MAX, &requested_b);
    BOOL minimized = NO;
    BOOL has_minimized = [self windowIsMinimized:&minimized];
    NSDictionary *info = copy_window_info(self.windowNumber);
    NSInteger onscreen = window_integer(info, kCGWindowIsOnscreen, 0);
    BOOL succeeded = has_minimized && !minimized && onscreen == 1;
    return observation(succeeded,
                       requested_a,
                       requested_b,
                       onscreen,
                       self.runningApplication.active && [self focusedWindowMatches],
                       succeeded ? @"verified" : @"target-window-not-restored");
}

- (ActionObservation *)execute:(ScenarioEvent *)event {
    NSString *failure = nil;
    if (![self verifyFastIdentity:&failure] || ![self verifyWindow:&failure]) {
        return observation(NO, 0, 0, 0, 0, failure);
    }
    ActionObservation *result;
    if ([event.action isEqualToString:@"input"]) {
        result = [self executeInput:event];
    } else if ([event.action isEqualToString:@"scroll-rows"]) {
        result = [self executeScroll:event];
    } else if ([event.action isEqualToString:@"minimize"]) {
        result = [self setMinimized:YES];
    } else if ([event.action isEqualToString:@"restore"]) {
        result = [self setMinimized:NO];
    } else if ([event.action isEqualToString:@"occluder-show"]) {
        result = [self showOccluder];
    } else if ([event.action isEqualToString:@"occluder-hide"]) {
        result = [self hideOccluder];
    } else if ([event.action isEqualToString:@"resize-grid"]) {
        result = [self executeResize:event];
    } else {
        result = [self checkpoint:event];
    }
    if (result.succeeded && self.occluder != nil && ![self verifyOccluder:&failure]) {
        result.succeeded = NO;
        result.result = failure;
    }
    BOOL full_verification = [event.action isEqualToString:@"stop"];
    if (result.succeeded
        && ((full_verification ? ![self verifyIdentity:&failure]
                              : ![self verifyFastIdentity:&failure])
            || ![self verifyWindow:&failure])) {
        result.succeeded = NO;
        result.result = failure;
    }
    return result;
}

- (BOOL)restoreSafeState {
    BOOL succeeded = YES;
    if (self.occluder != nil) {
        [self.occluder orderOut:nil];
        [self.occluder close];
        self.occluder = nil;
    }
    BOOL minimized = NO;
    if (_windowElement != NULL && [self windowIsMinimized:&minimized] && minimized) {
        NSString *identity_error = nil;
        if ([self verifyFastIdentity:&identity_error]) {
            succeeded = AXUIElementSetAttributeValue(
                _windowElement,
                kAXMinimizedAttribute,
                kCFBooleanFalse) == kAXErrorSuccess;
        } else {
            succeeded = NO;
        }
    }
    if (_hasInitialWindowBounds) {
        succeeded = [self restoreWindowBounds:_initialWindowBounds] && succeeded;
    }
    BOOL minimized_after = YES;
    CGRect bounds_after;
    succeeded = [self windowIsMinimized:&minimized_after]
        && !minimized_after
        && [self windowBounds:&bounds_after]
        && rect_matches(bounds_after, _initialWindowBounds, kRestorationTolerance)
        && succeeded;
    return succeeded;
}

@end

static NSDictionary<NSString *, NSString *> *parse_options(int argc, const char *argv[], NSString **error) {
    NSArray<NSString *> *required = @[
        @"--pid", @"--start-identity", @"--executable",
        @"--executable-sha256", @"--app-bundle", @"--bundle-identifier",
        @"--signing-identifier", @"--team-identifier", @"--cdhash",
        @"--window-number", @"--scenario-plan", @"--plan-start-continuous-ns",
        @"--output",
    ];
    NSSet<NSString *> *allowed = [NSSet setWithArray:required];
    NSMutableDictionary<NSString *, NSString *> *options = [NSMutableDictionary dictionary];
    for (int index = 1; index < argc; index += 2) {
        if (index + 1 >= argc) {
            set_error(error, @"option-missing-value");
            return nil;
        }
        NSString *key = [NSString stringWithUTF8String:argv[index]];
        NSString *value = [NSString stringWithUTF8String:argv[index + 1]];
        if (key == nil || value == nil || ![allowed containsObject:key] || options[key] != nil) {
            set_error(error, @"unknown-or-duplicate-option");
            return nil;
        }
        options[key] = value;
    }
    for (NSString *key in required) {
        if (options[key] == nil) {
            set_error(error, @"required-option-missing");
            return nil;
        }
    }
    return options;
}

typedef struct {
    int directoryFD;
    int fileFD;
    FILE *stream;
    char temporaryName[64];
    char finalName[NAME_MAX + 1];
} DriverOutput;

static void discard_output(DriverOutput *output) {
    if (output->stream != NULL) {
        fclose(output->stream);
        output->stream = NULL;
        output->fileFD = -1;
    } else if (output->fileFD >= 0) {
        close(output->fileFD);
        output->fileFD = -1;
    }
    if (output->directoryFD >= 0 && output->temporaryName[0] != '\0') {
        unlinkat(output->directoryFD, output->temporaryName, 0);
    }
    if (output->directoryFD >= 0) {
        close(output->directoryFD);
        output->directoryFD = -1;
    }
}

static BOOL open_atomic_output(NSString *output_path,
                               DriverOutput *output,
                               NSString **error) {
    memset(output, 0, sizeof(*output));
    output->directoryFD = -1;
    output->fileFD = -1;
    NSString *directory = [output_path stringByDeletingLastPathComponent];
    if (directory.length == 0) {
        directory = @".";
    }
    NSString *name = output_path.lastPathComponent;
    if (name.length == 0 || [name isEqualToString:@"."] || [name isEqualToString:@".."]
        || !string_is_safe_field(name, NAME_MAX) || [name containsString:@"/"]) {
        return set_error(error, @"output-name-invalid");
    }
    output->directoryFD = open(
        directory.fileSystemRepresentation,
        O_RDONLY | O_CLOEXEC | O_DIRECTORY | O_NOFOLLOW);
    if (output->directoryFD < 0) {
        return set_error(error, @"output-directory-unavailable");
    }
    struct stat existing;
    if (fstatat(output->directoryFD,
                name.fileSystemRepresentation,
                &existing,
                AT_SYMLINK_NOFOLLOW) == 0
        || errno != ENOENT) {
        discard_output(output);
        return set_error(error, @"output-already-exists-or-unavailable");
    }
    snprintf(output->temporaryName,
             sizeof(output->temporaryName),
             ".driver-events.%d.XXXXXX",
             getpid());
    output->fileFD = mkostempsat_np(
        output->directoryFD,
        output->temporaryName,
        0,
        O_CLOEXEC);
    if (output->fileFD < 0) {
        discard_output(output);
        return set_error(error, @"output-temporary-create-failed");
    }
    if (fchmod(output->fileFD, S_IRUSR | S_IWUSR) != 0) {
        discard_output(output);
        return set_error(error, @"output-permission-set-failed");
    }
    output->stream = fdopen(output->fileFD, "w");
    if (output->stream == NULL) {
        discard_output(output);
        return set_error(error, @"output-stream-create-failed");
    }
    strlcpy(output->finalName, name.fileSystemRepresentation, sizeof(output->finalName));
    return YES;
}

static BOOL publish_atomic_output(DriverOutput *output, NSString **error) {
    BOOL valid = output->stream != NULL && ferror(output->stream) == 0;
    if (valid) valid = fflush(output->stream) == 0 && ferror(output->stream) == 0;
    if (valid) valid = fchmod(output->fileFD, S_IRUSR) == 0;
    if (valid) valid = fsync(output->fileFD) == 0;
    int close_status = output->stream != NULL ? fclose(output->stream) : -1;
    output->stream = NULL;
    output->fileFD = -1;
    valid = valid && close_status == 0;
    if (!valid) {
        discard_output(output);
        return set_error(error, @"output-flush-failed");
    }
    if (renameatx_np(output->directoryFD,
                     output->temporaryName,
                     output->directoryFD,
                     output->finalName,
                     RENAME_EXCL) != 0) {
        discard_output(output);
        return set_error(error, @"output-atomic-publish-failed");
    }
    output->temporaryName[0] = '\0';
    if (fsync(output->directoryFD) != 0) {
        unlinkat(output->directoryFD, output->finalName, 0);
        fsync(output->directoryFD);
        discard_output(output);
        return set_error(error, @"output-directory-flush-failed");
    }
    close(output->directoryFD);
    output->directoryFD = -1;
    return YES;
}

int main(int argc, const char *argv[]) {
    @autoreleasepool {
        umask(077);
        setlocale(LC_ALL, "C");
        if (argc == 2 && (strcmp(argv[1], "--help") == 0 || strcmp(argv[1], "-h") == 0)) {
            print_usage(stdout);
            return 0;
        }
        NSString *error = nil;
        NSDictionary<NSString *, NSString *> *options = parse_options(argc, argv, &error);
        if (options == nil) {
            print_usage(stderr);
            fprintf(stderr, "error: %s\n", error.UTF8String);
            return 64;
        }

        uint64_t pid_value = 0;
        uint64_t window_number = 0;
        uint64_t plan_start_continuous_ns = 0;
        if (!parse_unsigned(options[@"--pid"], INT32_MAX, &pid_value) || pid_value == 0
            || !string_is_safe_field(options[@"--start-identity"], 64)
            || !parse_unsigned(options[@"--window-number"], UINT32_MAX, &window_number)
            || window_number == 0
            || !parse_unsigned(options[@"--plan-start-continuous-ns"],
                               UINT64_MAX - 2000000000ULL,
                               &plan_start_continuous_ns)
            || plan_start_continuous_ns == 0
            || !string_is_hex(options[@"--executable-sha256"], 64, 64)
            || !string_is_hex(options[@"--cdhash"], 40, 128)
            || !string_is_safe_label(options[@"--bundle-identifier"])
            || !string_is_safe_label(options[@"--signing-identifier"])
            || (![options[@"--team-identifier"] isEqualToString:@"-"]
                && ![options[@"--team-identifier"] isEqualToString:@"none"]
                && !string_is_safe_label(options[@"--team-identifier"]))) {
            fprintf(stderr, "error: invalid identity option\n");
            return 64;
        }
        NSString *executable = canonical_path(options[@"--executable"]);
        NSString *app_bundle = canonical_path(options[@"--app-bundle"]);
        if (executable == nil || app_bundle == nil || ![app_bundle.pathExtension isEqualToString:@"app"]
            || ![executable hasPrefix:[app_bundle stringByAppendingString:@"/"]]) {
            fprintf(stderr, "error: invalid package path\n");
            return 64;
        }

        NSArray<ScenarioEvent *> *events = parse_plan(options[@"--scenario-plan"], &error);
        if (events == nil) {
            fprintf(stderr, "error: %s\n", error.UTF8String);
            return 64;
        }

        [NSApplication sharedApplication];
        [NSApp setActivationPolicy:NSApplicationActivationPolicyAccessory];
        DriverTarget *target = [DriverTarget new];
        target.pid = (pid_t)pid_value;
        target.startIdentity = options[@"--start-identity"];
        target.windowNumber = (CGWindowID)window_number;
        target.executablePath = executable;
        target.expectedExecutableSHA256 = options[@"--executable-sha256"];
        target.appBundlePath = app_bundle;
        target.bundleIdentifier = options[@"--bundle-identifier"];
        target.signingIdentifier = options[@"--signing-identifier"];
        target.teamIdentifier = ([options[@"--team-identifier"] isEqualToString:@"-"]
                                 || [options[@"--team-identifier"] isEqualToString:@"none"])
            ? nil
            : options[@"--team-identifier"];
        target.cdhash = options[@"--cdhash"];
        if (![target prepare:&error]) {
            fprintf(stderr, "error: %s\n", error.UTF8String);
            return 65;
        }

        DriverOutput output;
        if (!open_atomic_output(options[@"--output"], &output, &error)) {
            fprintf(stderr, "error: %s\n", error.UTF8String);
            return 73;
        }
        BOOL output_valid = fprintf(output.stream,
                "sequence\tcontinuous_ns\tevent_id\taction\ttarget_pid\twindow_number\t"
                "requested_a\trequested_b\tobserved_a\tobserved_b\tresult\n") >= 0;

        struct sigaction action;
        memset(&action, 0, sizeof(action));
        action.sa_handler = handle_signal;
        sigemptyset(&action.sa_mask);
        sigaction(SIGINT, &action, NULL);
        sigaction(SIGTERM, &action, NULL);

        uint64_t prepared = continuous_nanoseconds();
        if (prepared > plan_start_continuous_ns + 2000000000ULL) {
            discard_output(&output);
            fprintf(stderr, "error: plan start deadline was missed\n");
            return 65;
        }
        uint64_t started = plan_start_continuous_ns;
        BOOL all_succeeded = YES;
        BOOL restoration_succeeded = YES;
        NSUInteger sequence = 0;
        @try {
            for (ScenarioEvent *event in events) {
                uint64_t deadline = started + event.offsetMilliseconds * 1000000ULL;
                if (!wait_until_continuous(deadline)) {
                    ActionObservation *interrupted = observation(NO, 0, 0, 0, 0, @"interrupted");
                    output_valid = fprintf(output.stream,
                            "%lu\t%llu\t%s\t%s\t%d\t%u\t%lld\t%lld\t%lld\t%lld\t%s\n",
                            (unsigned long)sequence,
                            interrupted.eventNanoseconds,
                            event.identifier.UTF8String,
                            event.action.UTF8String,
                            target.pid,
                            target.windowNumber,
                            interrupted.requestedA,
                            interrupted.requestedB,
                            interrupted.observedA,
                            interrupted.observedB,
                            interrupted.result.UTF8String) >= 0 && output_valid;
                    all_succeeded = NO;
                    break;
                }
                ActionObservation *result = [target execute:event];
                uint64_t maximum_lateness = sequence == 0
                    ? 2000000000ULL
                    : kMaximumDispatchLatenessNanoseconds;
                if (result.eventNanoseconds > deadline + maximum_lateness) {
                    result.succeeded = NO;
                    result.result = @"schedule-deadline-missed";
                }
                output_valid = fprintf(output.stream,
                        "%lu\t%llu\t%s\t%s\t%d\t%u\t%lld\t%lld\t%lld\t%lld\t%s\n",
                        (unsigned long)sequence,
                        result.eventNanoseconds,
                        event.identifier.UTF8String,
                        event.action.UTF8String,
                        target.pid,
                        target.windowNumber,
                        result.requestedA,
                        result.requestedB,
                        result.observedA,
                        result.observedB,
                        result.result.UTF8String) >= 0 && output_valid;
                if (!result.succeeded) {
                    all_succeeded = NO;
                    break;
                }
                sequence += 1;
            }
        } @finally {
            restoration_succeeded = [target restoreSafeState];
        }

        if (!restoration_succeeded) {
            all_succeeded = NO;
            output_valid = NO;
            error = @"safe-state-restoration-failed";
        }
        output_valid = output_valid && ferror(output.stream) == 0;

        if (!output_valid) {
            discard_output(&output);
            NSString *reason = error != nil ? error : @"output write failed";
            fprintf(stderr, "error: %s\n", reason.UTF8String);
            return 74;
        }
        if (!publish_atomic_output(&output, &error)) {
            fprintf(stderr, "error: %s\n", error.UTF8String);
            return 74;
        }
        if (!all_succeeded) {
            fprintf(stderr, "error: native scenario did not complete\n");
            return 70;
        }
        return 0;
    }
}
