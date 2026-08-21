#import <AppKit/AppKit.h>
#import <ApplicationServices/ApplicationServices.h>
#import <CommonCrypto/CommonDigest.h>
#import <Security/Security.h>

#include <errno.h>
#include <fcntl.h>
#include <libproc.h>
#include <locale.h>
#include <math.h>
#include <mach/mach_time.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/proc.h>
#include <sys/proc_info.h>
#include <sys/stat.h>
#include <unistd.h>

static const NSUInteger kMaximumIdentityBytes = 64 * 1024;

static BOOL fail(NSString **error, NSString *reason) {
    if (error != NULL) *error = reason;
    return NO;
}

static void usage(FILE *stream) {
    fprintf(stream,
            "Usage: performance-window-resolver --self-test\n"
            "  performance-window-resolver --subject-identity FILE \\\n\n"
            "  [--window-number N] --output FILE\n\n"
            "Resolve exactly one eligible visible layer-zero window owned by the\n"
            "frozen packaged subject. An explicit window number disambiguates only\n"
            "among eligible windows and is authenticated through both CoreGraphics\n"
            "and Accessibility before immutable metadata is published.\n");
}

static NSString *canonical_path(NSString *path) {
    if (path.length == 0) return nil;
    char *resolved = realpath(path.fileSystemRepresentation, NULL);
    if (resolved == NULL) return nil;
    NSString *value = [[NSFileManager defaultManager]
        stringWithFileSystemRepresentation:resolved length:strlen(resolved)];
    free(resolved);
    return value;
}

static BOOL safe_field(NSString *value, NSUInteger maximum) {
    if (value.length == 0
        || [value lengthOfBytesUsingEncoding:NSUTF8StringEncoding] > maximum) return NO;
    NSCharacterSet *controls =
        [NSCharacterSet characterSetWithCharactersInString:@"\t\r\n\0"];
    return [value rangeOfCharacterFromSet:controls].location == NSNotFound;
}

static BOOL safe_label(NSString *value) {
    if (!safe_field(value, 255)) return NO;
    NSCharacterSet *allowed = [NSCharacterSet characterSetWithCharactersInString:
        @"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._+-"];
    return [[value stringByTrimmingCharactersInSet:allowed] length] == 0;
}

static BOOL lower_hex(NSString *value, NSUInteger minimum, NSUInteger maximum) {
    if (value.length < minimum || value.length > maximum || value.length % 2 != 0) return NO;
    NSCharacterSet *allowed =
        [NSCharacterSet characterSetWithCharactersInString:@"0123456789abcdef"];
    return [[value stringByTrimmingCharactersInSet:allowed] length] == 0;
}

static BOOL parse_uint(NSString *value, uint64_t maximum, uint64_t *result) {
    if (value.length == 0 || [value characterAtIndex:0] == '+') return NO;
    errno = 0;
    char *end = NULL;
    unsigned long long parsed = strtoull(value.UTF8String, &end, 10);
    if (errno != 0 || end == value.UTF8String || *end != '\0' || parsed > maximum) return NO;
    *result = (uint64_t)parsed;
    return YES;
}

static BOOL parse_start_identity(NSString *value, uint64_t *seconds, uint64_t *microseconds) {
    NSArray<NSString *> *fields = [value componentsSeparatedByString:@":"];
    return fields.count == 2
        && parse_uint(fields[0], UINT64_MAX, seconds)
        && parse_uint(fields[1], 999999, microseconds);
}

static BOOL same_stat(const struct stat *left, const struct stat *right) {
    return left->st_dev == right->st_dev
        && left->st_ino == right->st_ino
        && left->st_mode == right->st_mode
        && left->st_size == right->st_size
        && left->st_mtimespec.tv_sec == right->st_mtimespec.tv_sec
        && left->st_mtimespec.tv_nsec == right->st_mtimespec.tv_nsec
        && left->st_ctimespec.tv_sec == right->st_ctimespec.tv_sec
        && left->st_ctimespec.tv_nsec == right->st_ctimespec.tv_nsec;
}

static NSString *sha256_bytes(const void *bytes, size_t length) {
    uint8_t digest[CC_SHA256_DIGEST_LENGTH];
    CC_SHA256(bytes, (CC_LONG)length, digest);
    NSMutableString *hex = [NSMutableString stringWithCapacity:64];
    for (NSUInteger index = 0; index < CC_SHA256_DIGEST_LENGTH; index += 1) {
        [hex appendFormat:@"%02x", digest[index]];
    }
    return hex;
}

static NSString *hex_data(NSData *data) {
    const uint8_t *bytes = data.bytes;
    NSMutableString *hex = [NSMutableString stringWithCapacity:data.length * 2];
    for (NSUInteger index = 0; index < data.length; index += 1) {
        [hex appendFormat:@"%02x", bytes[index]];
    }
    return hex;
}

static NSString *sha256_fd(int descriptor, NSString **error) {
    if (lseek(descriptor, 0, SEEK_SET) < 0) {
        fail(error, @"hash-seek-failed");
        return nil;
    }
    CC_SHA256_CTX context;
    CC_SHA256_Init(&context);
    uint8_t buffer[64 * 1024];
    for (;;) {
        ssize_t count = read(descriptor, buffer, sizeof(buffer));
        if (count == 0) break;
        if (count < 0 && errno == EINTR) continue;
        if (count < 0) {
            fail(error, @"hash-read-failed");
            return nil;
        }
        CC_SHA256_Update(&context, buffer, (CC_LONG)count);
    }
    uint8_t digest[CC_SHA256_DIGEST_LENGTH];
    CC_SHA256_Final(digest, &context);
    NSData *data = [NSData dataWithBytes:digest length:sizeof(digest)];
    return hex_data(data);
}

static uint64_t continuous_nanoseconds(void) {
    static mach_timebase_info_data_t timebase;
    static dispatch_once_t once;
    dispatch_once(&once, ^{ mach_timebase_info(&timebase); });
    __uint128_t scaled = (__uint128_t)mach_continuous_time() * timebase.numer;
    return (uint64_t)(scaled / timebase.denom);
}

static BOOL ax_window_number(AXUIElementRef window, CGWindowID *number, BOOL *present) {
    CFTypeRef raw = NULL;
    AXError status = AXUIElementCopyAttributeValue(window, CFSTR("AXWindowNumber"), &raw);
    if (status == kAXErrorAttributeUnsupported || status == kAXErrorNoValue) {
        if (raw != NULL) CFRelease(raw);
        *present = NO;
        return YES;
    }
    if (status != kAXErrorSuccess || raw == NULL || CFGetTypeID(raw) != CFNumberGetTypeID()) {
        if (raw != NULL) CFRelease(raw);
        return NO;
    }
    int64_t value = 0;
    BOOL valid = CFNumberGetValue((CFNumberRef)raw, kCFNumberSInt64Type, &value)
        && value > 0 && value <= UINT32_MAX;
    CFRelease(raw);
    if (!valid) return NO;
    *number = (CGWindowID)value;
    *present = YES;
    return YES;
}

static BOOL ax_boolean(AXUIElementRef element, CFStringRef attribute, BOOL *value) {
    CFTypeRef raw = NULL;
    AXError status = AXUIElementCopyAttributeValue(element, attribute, &raw);
    if (status != kAXErrorSuccess || raw == NULL || CFGetTypeID(raw) != CFBooleanGetTypeID()) {
        if (raw != NULL) CFRelease(raw);
        return NO;
    }
    *value = CFBooleanGetValue((CFBooleanRef)raw);
    CFRelease(raw);
    return YES;
}

static BOOL ax_string(AXUIElementRef element, CFStringRef attribute, NSString **value) {
    CFTypeRef raw = NULL;
    AXError status = AXUIElementCopyAttributeValue(element, attribute, &raw);
    if (status != kAXErrorSuccess || raw == NULL || CFGetTypeID(raw) != CFStringGetTypeID()) {
        if (raw != NULL) CFRelease(raw);
        return NO;
    }
    *value = CFBridgingRelease(raw);
    return YES;
}

static BOOL ax_point(AXUIElementRef element, CFStringRef attribute, CGPoint *value) {
    CFTypeRef raw = NULL;
    AXError status = AXUIElementCopyAttributeValue(element, attribute, &raw);
    BOOL valid = status == kAXErrorSuccess && raw != NULL
        && CFGetTypeID(raw) == AXValueGetTypeID()
        && AXValueGetValue((AXValueRef)raw, kAXValueTypeCGPoint, value);
    if (raw != NULL) CFRelease(raw);
    return valid;
}

static BOOL ax_size(AXUIElementRef element, CFStringRef attribute, CGSize *value) {
    CFTypeRef raw = NULL;
    AXError status = AXUIElementCopyAttributeValue(element, attribute, &raw);
    BOOL valid = status == kAXErrorSuccess && raw != NULL
        && CFGetTypeID(raw) == AXValueGetTypeID()
        && AXValueGetValue((AXValueRef)raw, kAXValueTypeCGSize, value);
    if (raw != NULL) CFRelease(raw);
    return valid;
}

static BOOL finite_bounds(NSDictionary *candidate) {
    double x = [candidate[@"x"] doubleValue];
    double y = [candidate[@"y"] doubleValue];
    double width = [candidate[@"width"] doubleValue];
    double height = [candidate[@"height"] doubleValue];
    return isfinite(x) && isfinite(y) && isfinite(width) && isfinite(height)
        && width > 0.0 && height > 0.0;
}

static BOOL same_bounds(NSDictionary *left, NSDictionary *right) {
    return [left[@"x"] doubleValue] == [right[@"x"] doubleValue]
        && [left[@"y"] doubleValue] == [right[@"y"] doubleValue]
        && [left[@"width"] doubleValue] == [right[@"width"] doubleValue]
        && [left[@"height"] doubleValue] == [right[@"height"] doubleValue];
}

static NSArray<NSDictionary *> *one_to_one_windows(
    NSArray<NSDictionary *> *rawAX,
    NSArray<NSDictionary *> *rawCG,
    pid_t subjectPID
) {
    NSPredicate *eligibleAXPredicate = [NSPredicate predicateWithBlock:
        ^BOOL(NSDictionary *candidate, NSDictionary *_) {
            (void)_;
            return [candidate[@"owner"] intValue] == subjectPID
                && [candidate[@"role"] isEqualToString:(__bridge NSString *)kAXWindowRole]
                && [candidate[@"subrole"] isEqualToString:
                    (__bridge NSString *)kAXStandardWindowSubrole]
                && ![candidate[@"minimized"] boolValue] && finite_bounds(candidate);
        }];
    NSPredicate *eligibleCGPredicate = [NSPredicate predicateWithBlock:
        ^BOOL(NSDictionary *candidate, NSDictionary *_) {
            (void)_;
            return [candidate[@"owner"] intValue] == subjectPID
                && [candidate[@"number"] unsignedIntValue] > 0
                && [candidate[@"layer"] integerValue] == 0
                && [candidate[@"onscreen"] boolValue]
                && [candidate[@"alpha"] doubleValue] > 0.0 && finite_bounds(candidate);
        }];
    NSArray<NSDictionary *> *ax = [rawAX filteredArrayUsingPredicate:eligibleAXPredicate];
    NSArray<NSDictionary *> *cg = [rawCG filteredArrayUsingPredicate:eligibleCGPredicate];
    if (ax.count == 0 || ax.count != cg.count) return @[];

    NSMutableArray<NSNumber *> *axDegrees = [NSMutableArray arrayWithCapacity:ax.count];
    NSMutableArray<NSNumber *> *cgDegrees = [NSMutableArray arrayWithCapacity:cg.count];
    for (NSUInteger index = 0; index < ax.count; index++) [axDegrees addObject:@0];
    for (NSUInteger index = 0; index < cg.count; index++) [cgDegrees addObject:@0];
    for (NSUInteger axIndex = 0; axIndex < ax.count; axIndex++) {
        NSDictionary *axWindow = ax[axIndex];
        NSNumber *axNumber = axWindow[@"number"];
        for (NSUInteger cgIndex = 0; cgIndex < cg.count; cgIndex++) {
            NSDictionary *cgWindow = cg[cgIndex];
            BOOL numberMatches = axNumber == nil
                || [axNumber isEqualToNumber:cgWindow[@"number"]];
            if (numberMatches && same_bounds(axWindow, cgWindow)) {
                axDegrees[axIndex] = @([axDegrees[axIndex] unsignedIntegerValue] + 1);
                cgDegrees[cgIndex] = @([cgDegrees[cgIndex] unsignedIntegerValue] + 1);
            }
        }
    }
    for (NSNumber *degree in axDegrees) if (degree.unsignedIntegerValue != 1) return @[];
    for (NSNumber *degree in cgDegrees) if (degree.unsignedIntegerValue != 1) return @[];
    NSMutableArray<NSDictionary *> *mapped = [NSMutableArray arrayWithCapacity:cg.count];
    for (NSDictionary *cgWindow in cg) {
        NSDictionary *matchedAX = nil;
        for (NSDictionary *axWindow in ax) {
            NSNumber *axNumber = axWindow[@"number"];
            if ((axNumber == nil || [axNumber isEqualToNumber:cgWindow[@"number"]])
                && same_bounds(axWindow, cgWindow)) {
                matchedAX = axWindow;
                break;
            }
        }
        if (matchedAX == nil) return @[];
        NSMutableDictionary *result = [cgWindow mutableCopy];
        result[@"ax_main"] = matchedAX[@"main"] == nil ? @NO : matchedAX[@"main"];
        result[@"ax_focused"] = matchedAX[@"focused"] == nil ? @NO : matchedAX[@"focused"];
        [mapped addObject:result];
    }
    return mapped;
}

@interface FrozenSubject : NSObject {
    int _identityFD;
    int _executableFD;
    struct stat _identityStat;
    struct stat _executableStat;
    struct stat _appStat;
    struct stat _infoStat;
}
@property(nonatomic, copy) NSString *identityPath;
@property(nonatomic, copy) NSString *identitySHA256;
@property(nonatomic, copy) NSString *subject;
@property(nonatomic, copy) NSString *appBundlePath;
@property(nonatomic, copy) NSString *bundleIdentifier;
@property(nonatomic, copy) NSString *bundleVersion;
@property(nonatomic, copy) NSString *executablePath;
@property(nonatomic, copy) NSString *executableSHA256;
@property(nonatomic) uint64_t executableDevice;
@property(nonatomic) uint64_t executableInode;
@property(nonatomic, copy) NSString *signingIdentifier;
@property(nonatomic, copy, nullable) NSString *teamIdentifier;
@property(nonatomic, copy) NSString *cdhash;
@property(nonatomic) pid_t pid;
@property(nonatomic, copy) NSString *processStartIdentity;
@property(nonatomic) uint64_t startSeconds;
@property(nonatomic) uint64_t startMicroseconds;
- (BOOL)load:(NSString *)path error:(NSString **)error;
- (BOOL)verify:(NSString **)error;
@end

@implementation FrozenSubject

- (instancetype)init {
    self = [super init];
    if (self != nil) {
        _identityFD = -1;
        _executableFD = -1;
    }
    return self;
}

- (void)dealloc {
    if (_identityFD >= 0) close(_identityFD);
    if (_executableFD >= 0) close(_executableFD);
}

- (NSData *)readIdentity:(NSString *)path error:(NSString **)error {
    _identityFD = open(path.fileSystemRepresentation, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    if (_identityFD < 0 || fstat(_identityFD, &_identityStat) != 0
        || !S_ISREG(_identityStat.st_mode) || (_identityStat.st_mode & 0222) != 0
        || _identityStat.st_size <= 0 || _identityStat.st_size > (off_t)kMaximumIdentityBytes) {
        fail(error, @"subject-identity-not-private-immutable-regular-file");
        return nil;
    }
    NSMutableData *data = [NSMutableData dataWithLength:(NSUInteger)_identityStat.st_size];
    uint8_t *cursor = data.mutableBytes;
    size_t remaining = data.length;
    while (remaining > 0) {
        ssize_t count = read(_identityFD, cursor, remaining);
        if (count < 0 && errno == EINTR) continue;
        if (count <= 0) {
            fail(error, @"subject-identity-read-failed");
            return nil;
        }
        cursor += count;
        remaining -= (size_t)count;
    }
    uint8_t extra = 0;
    struct stat after;
    ssize_t extra_count = read(_identityFD, &extra, 1);
    if (extra_count != 0 || fstat(_identityFD, &after) != 0
        || !same_stat(&_identityStat, &after)
        || memchr(data.bytes, '\0', data.length) != NULL
        || memchr(data.bytes, '\r', data.length) != NULL) {
        fail(error, @"subject-identity-changed-or-invalid");
        return nil;
    }
    return data;
}

- (BOOL)load:(NSString *)path error:(NSString **)error {
    self.identityPath = canonical_path(path);
    if (self.identityPath == nil) return fail(error, @"subject-identity-path-unavailable");
    NSData *data = [self readIdentity:self.identityPath error:error];
    if (data == nil) return NO;
    self.identitySHA256 = sha256_bytes(data.bytes, data.length);
    NSString *text = [[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding];
    if (text == nil || ![text hasSuffix:@"\n"]) {
        return fail(error, @"subject-identity-must-be-utf8-with-final-newline");
    }
    NSMutableArray<NSString *> *lines = [[text componentsSeparatedByString:@"\n"] mutableCopy];
    [lines removeLastObject];
    NSArray<NSString *> *keys = @[
        @"format_version", @"subject", @"app_bundle_path", @"bundle_identifier",
        @"bundle_version", @"executable_path", @"executable_sha256",
        @"executable_device", @"executable_inode", @"executable_fsid",
        @"signature_valid", @"signing_identifier", @"team_identifier", @"cdhash",
        @"process_pid", @"process_start_identity", @"identity_status",
    ];
    if (lines.count != keys.count) return fail(error, @"subject-identity-schema-width-mismatch");
    NSMutableDictionary<NSString *, NSString *> *values = [NSMutableDictionary dictionary];
    for (NSUInteger index = 0; index < keys.count; index += 1) {
        NSArray<NSString *> *fields = [lines[index] componentsSeparatedByString:@"\t"];
        if (fields.count != 2 || ![fields[0] isEqualToString:keys[index]]
            || values[fields[0]] != nil || !safe_field(fields[1], 4096)) {
            return fail(error, @"subject-identity-schema-invalid");
        }
        values[fields[0]] = fields[1];
    }
    uint64_t pid = 0, device = 0, inode = 0, fsid = 0;
    NSString *app = canonical_path(values[@"app_bundle_path"]);
    NSString *executable = canonical_path(values[@"executable_path"]);
    BOOL valid = [values[@"format_version"] isEqualToString:@"1"]
        && ([values[@"subject"] isEqualToString:@"spaceterm"]
            || [values[@"subject"] isEqualToString:@"ghostty"])
        && app != nil && executable != nil && [app.pathExtension isEqualToString:@"app"]
        && [app isEqualToString:values[@"app_bundle_path"]]
        && [executable isEqualToString:values[@"executable_path"]]
        && [executable hasPrefix:[app stringByAppendingString:@"/"]]
        && safe_label(values[@"bundle_identifier"])
        && safe_label(values[@"bundle_version"])
        && lower_hex(values[@"executable_sha256"], 64, 64)
        && parse_uint(values[@"executable_device"], UINT64_MAX, &device)
        && parse_uint(values[@"executable_inode"], UINT64_MAX, &inode)
        && parse_uint(values[@"executable_fsid"], UINT64_MAX, &fsid)
        && device == fsid && [values[@"signature_valid"] isEqualToString:@"true"]
        && safe_label(values[@"signing_identifier"])
        && safe_label(values[@"team_identifier"])
        && lower_hex(values[@"cdhash"], 8, 128)
        && parse_uint(values[@"process_pid"], INT32_MAX, &pid) && pid > 0
        && safe_field(values[@"process_start_identity"], 64)
        && [values[@"identity_status"] isEqualToString:@"frozen"];
    if (!valid) return fail(error, @"subject-identity-value-invalid");
    self.subject = values[@"subject"];
    self.appBundlePath = app;
    self.bundleIdentifier = values[@"bundle_identifier"];
    self.bundleVersion = values[@"bundle_version"];
    self.executablePath = executable;
    self.executableSHA256 = values[@"executable_sha256"];
    self.executableDevice = device;
    self.executableInode = inode;
    self.signingIdentifier = values[@"signing_identifier"];
    self.teamIdentifier = [values[@"team_identifier"] isEqualToString:@"none"]
        ? nil : values[@"team_identifier"];
    self.cdhash = values[@"cdhash"];
    self.pid = (pid_t)pid;
    self.processStartIdentity = values[@"process_start_identity"];
    _executableFD = open(self.executablePath.fileSystemRepresentation,
                         O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    NSString *info = [self.appBundlePath stringByAppendingPathComponent:@"Contents/Info.plist"];
    if (_executableFD < 0 || fstat(_executableFD, &_executableStat) != 0
        || !S_ISREG(_executableStat.st_mode)
        || (uint64_t)_executableStat.st_dev != device
        || (uint64_t)_executableStat.st_ino != inode
        || stat(self.appBundlePath.fileSystemRepresentation, &_appStat) != 0
        || !S_ISDIR(_appStat.st_mode)
        || stat(info.fileSystemRepresentation, &_infoStat) != 0
        || !S_ISREG(_infoStat.st_mode)) {
        return fail(error, @"subject-filesystem-identity-mismatch");
    }
    NSString *hash = sha256_fd(_executableFD, error);
    if (hash == nil || ![hash isEqualToString:self.executableSHA256]) {
        return fail(error, @"subject-executable-hash-mismatch");
    }
    struct proc_bsdinfo process;
    memset(&process, 0, sizeof(process));
    int count = proc_pidinfo(self.pid, PROC_PIDTBSDINFO, 0, &process, sizeof(process));
    uint64_t expectedSeconds = 0;
    uint64_t expectedMicroseconds = 0;
    if (count != sizeof(process) || process.pbi_status == SZOMB
        || process.pbi_uid != geteuid()
        || !parse_start_identity(self.processStartIdentity,
                                 &expectedSeconds,
                                 &expectedMicroseconds)
        || expectedSeconds != process.pbi_start_tvsec
        || expectedMicroseconds != process.pbi_start_tvusec) {
        return fail(error, @"subject-process-start-identity-mismatch");
    }
    self.startSeconds = process.pbi_start_tvsec;
    self.startMicroseconds = process.pbi_start_tvusec;
    return [self verify:error];
}

- (BOOL)verify:(NSString **)error {
    struct stat identityFD, identityPath, executableFD, executablePath, app, info;
    NSString *infoPath = [self.appBundlePath stringByAppendingPathComponent:@"Contents/Info.plist"];
    struct proc_bsdinfo process;
    memset(&process, 0, sizeof(process));
    int count = proc_pidinfo(self.pid, PROC_PIDTBSDINFO, 0, &process, sizeof(process));
    char path[PROC_PIDPATHINFO_MAXSIZE];
    int pathLength = proc_pidpath(self.pid, path, sizeof(path));
    if (count != sizeof(process) || process.pbi_status == SZOMB
        || process.pbi_uid != geteuid()
        || process.pbi_start_tvsec != self.startSeconds
        || process.pbi_start_tvusec != self.startMicroseconds
        || pathLength <= 0 || (size_t)pathLength >= sizeof(path)) {
        return fail(error, @"subject-process-generation-changed");
    }
    path[pathLength] = '\0';
    NSString *livePath = canonical_path([[NSFileManager defaultManager]
        stringWithFileSystemRepresentation:path length:(NSUInteger)pathLength]);
    if (![livePath isEqualToString:self.executablePath]
        || fstat(_identityFD, &identityFD) != 0
        || stat(self.identityPath.fileSystemRepresentation, &identityPath) != 0
        || fstat(_executableFD, &executableFD) != 0
        || stat(self.executablePath.fileSystemRepresentation, &executablePath) != 0
        || stat(self.appBundlePath.fileSystemRepresentation, &app) != 0
        || stat(infoPath.fileSystemRepresentation, &info) != 0
        || !same_stat(&_identityStat, &identityFD)
        || !same_stat(&_identityStat, &identityPath)
        || !same_stat(&_executableStat, &executableFD)
        || !same_stat(&_executableStat, &executablePath)
        || !same_stat(&_appStat, &app) || !same_stat(&_infoStat, &info)) {
        return fail(error, @"subject-file-or-process-identity-changed");
    }
    NSString *hash = sha256_fd(_executableFD, error);
    if (hash == nil || ![hash isEqualToString:self.executableSHA256]) {
        return fail(error, @"subject-executable-hash-changed");
    }
    NSRunningApplication *application =
        [NSRunningApplication runningApplicationWithProcessIdentifier:self.pid];
    if (application == nil || application.terminated
        || ![application.bundleIdentifier isEqualToString:self.bundleIdentifier]
        || ![canonical_path(application.bundleURL.path) isEqualToString:self.appBundlePath]
        || ![canonical_path(application.executableURL.path) isEqualToString:self.executablePath]) {
        return fail(error, @"subject-running-bundle-identity-changed");
    }
    NSDictionary *attributes = @{(__bridge id)kSecGuestAttributePid: @(self.pid)};
    SecCodeRef code = NULL;
    OSStatus status = SecCodeCopyGuestWithAttributes(
        NULL, (__bridge CFDictionaryRef)attributes, kSecCSDefaultFlags, &code);
    if (status == errSecSuccess && code != NULL) {
        status = SecCodeCheckValidity(code, kSecCSStrictValidate, NULL);
    }
    CFDictionaryRef raw = NULL;
    if (status == errSecSuccess) {
        status = SecCodeCopySigningInformation(
            code, kSecCSSigningInformation | kSecCSDynamicInformation, &raw);
    }
    if (code != NULL) CFRelease(code);
    if (status != errSecSuccess || raw == NULL) {
        if (raw != NULL) CFRelease(raw);
        return fail(error, @"subject-live-code-invalid");
    }
    NSDictionary *signing = CFBridgingRelease(raw);
    NSString *identifier = signing[(__bridge id)kSecCodeInfoIdentifier];
    NSString *team = signing[(__bridge id)kSecCodeInfoTeamIdentifier];
    NSData *unique = signing[(__bridge id)kSecCodeInfoUnique];
    NSURL *mainURL = signing[(__bridge id)kSecCodeInfoMainExecutable];
    BOOL teamMatches = self.teamIdentifier == nil ? team == nil
        : [team isEqualToString:self.teamIdentifier];
    if (![identifier isEqualToString:self.signingIdentifier] || !teamMatches
        || ![unique isKindOfClass:[NSData class]]
        || ![canonical_path(mainURL.path) isEqualToString:self.executablePath]) {
        return fail(error, @"subject-live-signing-identity-changed");
    }
    if (![[hex_data(unique) lowercaseString] isEqualToString:self.cdhash]) {
        return fail(error, @"subject-live-cdhash-changed");
    }
    return YES;
}

@end

static NSArray<NSDictionary *> *eligible_windows(FrozenSubject *subject, NSString **error) {
    AXUIElementRef application = AXUIElementCreateApplication(subject.pid);
    if (application == NULL) {
        fail(error, @"accessibility-application-unavailable");
        return nil;
    }
    CFTypeRef rawAX = NULL;
    AXError axStatus = AXUIElementCopyAttributeValue(application, kAXWindowsAttribute, &rawAX);
    CFRelease(application);
    if (axStatus != kAXErrorSuccess || rawAX == NULL
        || CFGetTypeID(rawAX) != CFArrayGetTypeID()) {
        if (rawAX != NULL) CFRelease(rawAX);
        fail(error, @"accessibility-window-list-unavailable");
        return nil;
    }
    NSMutableArray<NSDictionary *> *axCandidates = [NSMutableArray array];
    CFArrayRef axWindows = (CFArrayRef)rawAX;
    for (CFIndex index = 0; index < CFArrayGetCount(axWindows); index += 1) {
        CFTypeRef value = CFArrayGetValueAtIndex(axWindows, index);
        if (CFGetTypeID(value) != AXUIElementGetTypeID()) continue;
        AXUIElementRef window = (AXUIElementRef)value;
        pid_t owner = 0;
        CGWindowID number = 0;
        BOOL numberPresent = NO;
        BOOL minimized = YES;
        BOOL main = NO;
        BOOL focused = NO;
        NSString *role = nil;
        NSString *subrole = nil;
        CGPoint position = CGPointZero;
        CGSize size = CGSizeZero;
        if (AXUIElementGetPid(window, &owner) == kAXErrorSuccess
            && ax_window_number(window, &number, &numberPresent)
            && ax_boolean(window, kAXMinimizedAttribute, &minimized)
            && ax_boolean(window, kAXMainAttribute, &main)
            && ax_boolean(window, kAXFocusedAttribute, &focused)
            && ax_string(window, kAXRoleAttribute, &role)
            && ax_string(window, kAXSubroleAttribute, &subrole)
            && ax_point(window, kAXPositionAttribute, &position)
            && ax_size(window, kAXSizeAttribute, &size)) {
            NSMutableDictionary *candidate = [@{
                @"owner": @(owner), @"role": role, @"subrole": subrole,
                @"minimized": @(minimized), @"main": @(main), @"focused": @(focused),
                @"x": @(position.x), @"y": @(position.y),
                @"width": @(size.width), @"height": @(size.height),
            } mutableCopy];
            if (numberPresent) candidate[@"number"] = @(number);
            [axCandidates addObject:candidate];
        }
    }
    CFRelease(rawAX);

    CFArrayRef rawCG = CGWindowListCopyWindowInfo(
        kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
        kCGNullWindowID);
    if (rawCG == NULL) {
        fail(error, @"coregraphics-window-list-unavailable");
        return nil;
    }
    NSArray *windows = CFBridgingRelease(rawCG);
    NSMutableArray<NSDictionary *> *cgCandidates = [NSMutableArray array];
    for (id value in windows) {
        if (![value isKindOfClass:[NSDictionary class]]) continue;
        NSDictionary *window = value;
        NSNumber *owner = window[(__bridge id)kCGWindowOwnerPID];
        NSNumber *number = window[(__bridge id)kCGWindowNumber];
        NSNumber *layer = window[(__bridge id)kCGWindowLayer];
        NSNumber *onscreen = window[(__bridge id)kCGWindowIsOnscreen];
        NSNumber *alpha = window[(__bridge id)kCGWindowAlpha];
        CGRect bounds = CGRectZero;
        id rawBounds = window[(__bridge id)kCGWindowBounds];
        BOOL validBounds = [rawBounds isKindOfClass:[NSDictionary class]]
            && CGRectMakeWithDictionaryRepresentation(
                (__bridge CFDictionaryRef)rawBounds, &bounds)
            && bounds.size.width > 0 && bounds.size.height > 0;
        if (!validBounds) continue;
        [cgCandidates addObject:@{
            @"owner": owner == nil ? @0 : owner,
            @"number": number == nil ? @0 : number,
            @"layer": layer == nil ? @(-1) : layer,
            @"onscreen": onscreen == nil ? @NO : onscreen,
            @"alpha": alpha == nil ? @0 : alpha,
            @"x": @(bounds.origin.x), @"y": @(bounds.origin.y),
            @"width": @(bounds.size.width), @"height": @(bounds.size.height),
        }];
    }
    return one_to_one_windows(axCandidates, cgCandidates, subject.pid);
}

static NSArray<NSDictionary *> *select_windows(
    NSArray<NSDictionary *> *candidates,
    BOOL hasSelector,
    uint64_t selector
) {
    if (hasSelector) {
        NSPredicate *matches = [NSPredicate predicateWithBlock:
            ^BOOL(NSDictionary *window, NSDictionary *_) {
                (void)_;
                return [window[@"number"] unsignedLongLongValue] == selector;
            }];
        return [candidates filteredArrayUsingPredicate:matches];
    }
    if (candidates.count <= 1) return candidates;
    NSPredicate *focusedMain = [NSPredicate predicateWithBlock:
        ^BOOL(NSDictionary *window, NSDictionary *_) {
            (void)_;
            return [window[@"ax_main"] boolValue] && [window[@"ax_focused"] boolValue];
        }];
    return [candidates filteredArrayUsingPredicate:focusedMain];
}

static NSArray<NSDictionary *> *resolve_windows_with_retry(
    FrozenSubject *subject,
    BOOL hasSelector,
    uint64_t selector,
    NSString **error
) {
    for (NSUInteger attempt = 0; attempt < 100; attempt++) {
        NSArray<NSDictionary *> *candidates = eligible_windows(subject, error);
        if (candidates == nil) return nil;
        NSArray<NSDictionary *> *selected = select_windows(candidates, hasSelector, selector);
        if (selected.count == 1) return selected;
        usleep(50000);
    }
    return @[];
}

static NSDictionary *ax_fixture(
    pid_t owner,
    NSString *subrole,
    BOOL minimized,
    NSNumber *number,
    double x,
    double y,
    double width,
    double height
) {
    NSMutableDictionary *candidate = [@{
        @"owner": @(owner), @"role": (__bridge NSString *)kAXWindowRole,
        @"subrole": subrole, @"minimized": @(minimized), @"main": @YES, @"focused": @YES,
        @"x": @(x), @"y": @(y), @"width": @(width), @"height": @(height),
    } mutableCopy];
    if (number != nil) candidate[@"number"] = number;
    return candidate;
}

static NSDictionary *cg_fixture(
    pid_t owner,
    uint32_t number,
    NSInteger layer,
    BOOL onscreen,
    double x,
    double y,
    double width,
    double height
) {
    return @{
        @"owner": @(owner), @"number": @(number), @"layer": @(layer),
        @"onscreen": @(onscreen), @"alpha": @1,
        @"x": @(x), @"y": @(y), @"width": @(width), @"height": @(height),
    };
}

static BOOL resolver_self_test(void) {
    const pid_t subject = 123;
    NSString *standard = (__bridge NSString *)kAXStandardWindowSubrole;
    NSDictionary *gpui = ax_fixture(subject, standard, NO, nil, 35, 39, 1472, 937);
    NSDictionary *numbered = ax_fixture(subject, standard, NO, @42, 35, 39, 1472, 937);
    NSDictionary *cg = cg_fixture(subject, 42, 0, YES, 35, 39, 1472, 937);
    if (one_to_one_windows(@[gpui], @[cg], subject).count != 1
        || one_to_one_windows(@[numbered], @[cg], subject).count != 1) return NO;
    NSMutableDictionary *applicationProxy = [gpui mutableCopy];
    applicationProxy[@"role"] = (__bridge NSString *)kAXApplicationRole;
    if (one_to_one_windows(@[applicationProxy], @[cg], subject).count != 0) return NO;

    NSMutableDictionary *backgroundAX = [[ax_fixture(
        subject, standard, NO, nil, 1600, 39, 800, 600) mutableCopy] mutableCopy];
    backgroundAX[@"main"] = @NO;
    backgroundAX[@"focused"] = @NO;
    NSDictionary *backgroundCG = cg_fixture(subject, 43, 0, YES, 1600, 39, 800, 600);
    NSArray<NSDictionary *> *twoWindows = one_to_one_windows(
        @[gpui, backgroundAX], @[cg, backgroundCG], subject);
    if (twoWindows.count != 2
        || [select_windows(twoWindows, NO, 0).firstObject[@"number"] unsignedIntValue] != 42
        || select_windows(twoWindows, NO, 0).count != 1
        || [select_windows(twoWindows, YES, 43).firstObject[@"number"] unsignedIntValue] != 43
        || select_windows(twoWindows, YES, 43).count != 1) return NO;
    NSMutableDictionary *ambiguousAX = [backgroundAX mutableCopy];
    ambiguousAX[@"main"] = @YES;
    ambiguousAX[@"focused"] = @YES;
    NSArray<NSDictionary *> *ambiguousWindows = one_to_one_windows(
        @[gpui, ambiguousAX], @[cg, backgroundCG], subject);
    if (select_windows(ambiguousWindows, NO, 0).count != 2) return NO;

    NSDictionary *foreignAX = ax_fixture(subject + 1, standard, NO, nil, 35, 39, 1472, 937);
    NSDictionary *foreignCG = cg_fixture(subject + 1, 42, 0, YES, 35, 39, 1472, 937);
    NSDictionary *hiddenCG = cg_fixture(subject, 42, 0, NO, 35, 39, 1472, 937);
    NSDictionary *layeredCG = cg_fixture(subject, 42, 1, YES, 35, 39, 1472, 937);
    NSDictionary *nonstandardAX = ax_fixture(
        subject, (__bridge NSString *)kAXDialogSubrole, NO, nil, 35, 39, 1472, 937);
    NSDictionary *minimizedAX = ax_fixture(subject, standard, YES, nil, 35, 39, 1472, 937);
    NSDictionary *wrongNumberAX = ax_fixture(subject, standard, NO, @43, 35, 39, 1472, 937);
    if (one_to_one_windows(@[foreignAX], @[cg], subject).count != 0
        || one_to_one_windows(@[gpui], @[foreignCG], subject).count != 0
        || one_to_one_windows(@[gpui], @[hiddenCG], subject).count != 0
        || one_to_one_windows(@[gpui], @[layeredCG], subject).count != 0
        || one_to_one_windows(@[nonstandardAX], @[cg], subject).count != 0
        || one_to_one_windows(@[minimizedAX], @[cg], subject).count != 0
        || one_to_one_windows(@[wrongNumberAX], @[cg], subject).count != 0) return NO;

    NSDictionary *cgDuplicate = cg_fixture(subject, 44, 0, YES, 35, 39, 1472, 937);
    NSDictionary *axDuplicate = ax_fixture(subject, standard, NO, nil, 35, 39, 1472, 937);
    return one_to_one_windows(@[gpui], @[cg, cgDuplicate], subject).count == 0
        && one_to_one_windows(@[gpui, axDuplicate], @[cg], subject).count == 0
        && one_to_one_windows(@[gpui, axDuplicate], @[cg, cgDuplicate], subject).count == 0;
}

static BOOL publish(NSString *path, NSData *data, NSString **error) {
    NSString *directory = [path stringByDeletingLastPathComponent];
    NSString *name = path.lastPathComponent;
    if (name.length == 0 || [name isEqualToString:@"."] || [name isEqualToString:@".."]
        || [name containsString:@"/"] || !safe_field(name, NAME_MAX)) {
        return fail(error, @"output-name-invalid");
    }
    int directoryFD = open(directory.fileSystemRepresentation,
                           O_RDONLY | O_CLOEXEC | O_DIRECTORY | O_NOFOLLOW);
    if (directoryFD < 0) return fail(error, @"output-directory-unavailable");
    struct stat existing;
    if (fstatat(directoryFD, name.fileSystemRepresentation, &existing, AT_SYMLINK_NOFOLLOW) == 0
        || errno != ENOENT) {
        close(directoryFD);
        return fail(error, @"output-already-exists-or-unavailable");
    }
    char temporary[64];
    snprintf(temporary, sizeof(temporary), ".window-identity.%d.XXXXXX", getpid());
    int descriptor = mkostempsat_np(directoryFD, temporary, 0, O_CLOEXEC);
    BOOL valid = descriptor >= 0 && fchmod(descriptor, S_IRUSR | S_IWUSR) == 0;
    const uint8_t *cursor = data.bytes;
    size_t remaining = data.length;
    while (valid && remaining > 0) {
        ssize_t count = write(descriptor, cursor, remaining);
        if (count < 0 && errno == EINTR) continue;
        if (count <= 0) {
            valid = NO;
            break;
        }
        cursor += count;
        remaining -= (size_t)count;
    }
    valid = valid && fsync(descriptor) == 0 && fchmod(descriptor, S_IRUSR) == 0
        && close(descriptor) == 0;
    if (valid) {
        valid = renameatx_np(directoryFD, temporary, directoryFD,
                             name.fileSystemRepresentation, RENAME_EXCL) == 0
            && fsync(directoryFD) == 0;
    }
    if (!valid) {
        if (descriptor >= 0) close(descriptor);
        unlinkat(directoryFD, temporary, 0);
    }
    close(directoryFD);
    return valid ? YES : fail(error, @"output-atomic-publish-failed");
}

int main(int argc, const char *argv[]) {
    @autoreleasepool {
        umask(077);
        setlocale(LC_ALL, "C");
        if (argc == 2 && strcmp(argv[1], "--self-test") == 0) {
            return resolver_self_test() ? 0 : 1;
        }
        if (argc == 2 && (strcmp(argv[1], "--help") == 0 || strcmp(argv[1], "-h") == 0)) {
            usage(stdout);
            return 0;
        }
        NSString *identityPath = nil;
        NSString *outputPath = nil;
        uint64_t selector = 0;
        BOOL hasSelector = NO;
        for (int index = 1; index < argc; index += 2) {
            if (index + 1 >= argc) {
                usage(stderr);
                return 64;
            }
            NSString *key = [NSString stringWithUTF8String:argv[index]];
            NSString *value = [NSString stringWithUTF8String:argv[index + 1]];
            if ([key isEqualToString:@"--subject-identity"] && identityPath == nil) {
                identityPath = value;
            } else if ([key isEqualToString:@"--output"] && outputPath == nil) {
                outputPath = value;
            } else if ([key isEqualToString:@"--window-number"] && !hasSelector
                       && parse_uint(value, UINT32_MAX, &selector) && selector > 0) {
                hasSelector = YES;
            } else {
                fprintf(stderr, "error: unknown, duplicate, or invalid option\n");
                return 64;
            }
        }
        if (identityPath == nil || outputPath == nil) {
            usage(stderr);
            return 64;
        }
        if (!AXIsProcessTrusted()) {
            fprintf(stderr, "error: accessibility-permission-required\n");
            return 65;
        }
        NSString *error = nil;
        FrozenSubject *subject = [FrozenSubject new];
        if (![subject load:identityPath error:&error]) {
            fprintf(stderr, "error: %s\n", error.UTF8String);
            return 65;
        }
        NSArray<NSDictionary *> *selected = resolve_windows_with_retry(
            subject, hasSelector, selector, &error);
        if (selected == nil) {
            fprintf(stderr, "error: %s\n", error.UTF8String);
            return 65;
        }
        if (selected.count != 1) {
            fprintf(stderr, "error: %s\n",
                    hasSelector ? "explicit-window-is-not-exactly-one-eligible-window"
                                : "eligible-visible-window-is-not-unique");
            return 66;
        }
        NSDictionary *window = selected[0];
        if (![subject verify:&error]) {
            fprintf(stderr, "error: %s\n", error.UTF8String);
            return 65;
        }
        NSArray<NSDictionary *> *finalCandidates = eligible_windows(subject, &error);
        NSPredicate *sameIdentity = [NSPredicate predicateWithBlock:^BOOL(NSDictionary *candidate, NSDictionary *_) {
            (void)_;
            return [candidate[@"number"] isEqual:window[@"number"]]
                && same_bounds(candidate, window)
                && [candidate[@"ax_main"] isEqual:window[@"ax_main"]]
                && [candidate[@"ax_focused"] isEqual:window[@"ax_focused"]];
        }];
        NSArray<NSDictionary *> *finalMatches = finalCandidates == nil
            ? nil : [finalCandidates filteredArrayUsingPredicate:sameIdentity];
        if (finalMatches.count != 1) {
            fprintf(stderr, "error: window-identity-changed-during-resolution\n");
            return 66;
        }
        window = finalMatches[0];
        NSMutableString *record = [NSMutableString string];
        [record appendString:@"format_version\t1\n"];
        [record appendFormat:@"subject_identity_sha256\t%@\n", subject.identitySHA256];
        [record appendFormat:@"subject\t%@\n", subject.subject];
        [record appendFormat:@"process_pid\t%d\n", subject.pid];
        [record appendFormat:@"process_start_identity\t%@\n", subject.processStartIdentity];
        [record appendFormat:@"bundle_identifier\t%@\n", subject.bundleIdentifier];
        [record appendFormat:@"executable_sha256\t%@\n", subject.executableSHA256];
        [record appendFormat:@"window_number\t%@\n", window[@"number"]];
        [record appendString:@"window_owner_pid_verified\ttrue\n"];
        [record appendString:@"window_layer\t0\n"];
        [record appendString:@"window_onscreen\ttrue\n"];
        [record appendString:@"window_minimized\tfalse\n"];
        [record appendFormat:@"window_x\t%.3f\n", [window[@"x"] doubleValue]];
        [record appendFormat:@"window_y\t%.3f\n", [window[@"y"] doubleValue]];
        [record appendFormat:@"window_width\t%.3f\n", [window[@"width"] doubleValue]];
        [record appendFormat:@"window_height\t%.3f\n", [window[@"height"] doubleValue]];
        [record appendFormat:@"resolved_continuous_ns\t%llu\n", continuous_nanoseconds()];
        [record appendFormat:@"selector_kind\t%@\n", hasSelector ? @"explicit" : @"unique"];
        [record appendString:@"status\tfrozen\n"];
        NSData *bytes = [record dataUsingEncoding:NSUTF8StringEncoding];
        if (![subject verify:&error] || !publish(outputPath, bytes, &error)) {
            fprintf(stderr, "error: %s\n", error.UTF8String);
            return 73;
        }
        printf("window_number\t%u\n", [window[@"number"] unsignedIntValue]);
        return 0;
    }
}
