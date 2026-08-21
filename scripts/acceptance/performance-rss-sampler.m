#import <AppKit/AppKit.h>
#import <CommonCrypto/CommonDigest.h>
#import <Security/Security.h>

#include <errno.h>
#include <fcntl.h>
#include <libproc.h>
#include <locale.h>
#include <mach/mach_time.h>
#include <signal.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/proc.h>
#include <sys/proc_info.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <time.h>
#include <unistd.h>

static const uint64_t kSampleIntervalMilliseconds = 10000;
static const uint64_t kFirstSampleDelayMilliseconds = 500;
static const uint64_t kMaximumDurationMilliseconds = 720000;
static const uint64_t kMaximumWarmupMilliseconds = 120000;
static const uint64_t kMaximumSampleLatenessNanoseconds = 1000000000ULL;
static const size_t kMaximumIdentityBytes = 64 * 1024;
static volatile sig_atomic_t gInterrupted = 0;

static void handle_signal(int signal_number) {
    (void)signal_number;
    gInterrupted = 1;
}

static void usage(FILE *stream) {
    fprintf(stream,
            "Usage: performance-rss-sampler --subject-identity FILE \\\n\n"
            "  --plan-start-continuous-ns N --warmup-ms N --duration-ms N\n"
            "  --plan-start-gate-sha256 SHA256 --ready-receipt-sha256 SHA256\n"
            "  --output FILE\n\n"
            "After the requested warm-up, sample the exact frozen process's resident\n"
            "memory at 10-second intervals from elapsed 500 ms through 500 ms past\n"
            "the requested duration. This gives authenticated producer progress time\n"
            "to publish before sample zero. The artifact is published atomically after\n"
            "the process, executable, bundle, and signing identity remain verified.\n");
}

static BOOL fail(NSString **error, NSString *reason) {
    if (error != NULL) {
        *error = reason;
    }
    return NO;
}

static BOOL parse_uint(NSString *value, uint64_t maximum, uint64_t *result) {
    if (value.length == 0 || [value characterAtIndex:0] == '+') {
        return NO;
    }
    const char *bytes = value.UTF8String;
    if (bytes == NULL) {
        return NO;
    }
    errno = 0;
    char *end = NULL;
    unsigned long long parsed = strtoull(bytes, &end, 10);
    if (errno != 0 || end == bytes || *end != '\0' || parsed > maximum) {
        return NO;
    }
    *result = (uint64_t)parsed;
    return YES;
}

static BOOL safe_field(NSString *value, NSUInteger maximum_bytes) {
    if (value.length == 0 || [value lengthOfBytesUsingEncoding:NSUTF8StringEncoding] > maximum_bytes) {
        return NO;
    }
    NSCharacterSet *controls = [NSCharacterSet characterSetWithCharactersInString:@"\t\r\n\0"];
    return [value rangeOfCharacterFromSet:controls].location == NSNotFound;
}

static BOOL safe_label(NSString *value) {
    if (!safe_field(value, 255)) {
        return NO;
    }
    NSCharacterSet *allowed = [NSCharacterSet characterSetWithCharactersInString:
        @"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._+-"];
    return [[value stringByTrimmingCharactersInSet:allowed] length] == 0;
}

static BOOL lower_hex(NSString *value, NSUInteger minimum, NSUInteger maximum) {
    if (value.length < minimum || value.length > maximum || value.length % 2 != 0) {
        return NO;
    }
    NSCharacterSet *allowed = [NSCharacterSet characterSetWithCharactersInString:@"0123456789abcdef"];
    return [[value stringByTrimmingCharactersInSet:allowed] length] == 0;
}

static NSString *canonical_path(NSString *path) {
    if (path.length == 0) {
        return nil;
    }
    char *resolved = realpath(path.fileSystemRepresentation, NULL);
    if (resolved == NULL) {
        return nil;
    }
    NSString *result = [[NSFileManager defaultManager]
        stringWithFileSystemRepresentation:resolved
        length:strlen(resolved)];
    free(resolved);
    return result;
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

static NSString *sha256_for_bytes(const void *bytes, size_t length) {
    uint8_t digest[CC_SHA256_DIGEST_LENGTH];
    CC_SHA256(bytes, (CC_LONG)length, digest);
    NSMutableString *hex = [NSMutableString stringWithCapacity:CC_SHA256_DIGEST_LENGTH * 2];
    for (NSUInteger index = 0; index < CC_SHA256_DIGEST_LENGTH; index += 1) {
        [hex appendFormat:@"%02x", digest[index]];
    }
    return hex;
}

static NSString *sha256_for_fd(int fd, NSString **error) {
    if (lseek(fd, 0, SEEK_SET) < 0) {
        fail(error, @"hash-seek-failed");
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
            fail(error, @"hash-read-failed");
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

static NSString *hex_data(NSData *data) {
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

static BOOL wait_until(uint64_t deadline) {
    while (!gInterrupted) {
        uint64_t now = continuous_nanoseconds();
        if (now >= deadline) {
            return YES;
        }
        uint64_t remaining = deadline - now;
        uint64_t slice = remaining < 250000000ULL ? remaining : 250000000ULL;
        struct timespec request = {
            .tv_sec = (time_t)(slice / 1000000000ULL),
            .tv_nsec = (long)(slice % 1000000000ULL),
        };
        while (nanosleep(&request, &request) != 0 && errno == EINTR && !gInterrupted) {
        }
    }
    return NO;
}

@interface FrozenSubject : NSObject {
    int _identityFD;
    int _executableFD;
    struct stat _identityStat;
    struct stat _executableStat;
    struct stat _appBundleStat;
    struct stat _infoPlistStat;
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
- (BOOL)loadFromPath:(NSString *)path error:(NSString **)error;
- (BOOL)verify:(NSString **)error;
- (BOOL)residentKiB:(uint64_t *)rss error:(NSString **)error;
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
        fail(error, @"subject-identity-is-not-private-immutable-regular-file");
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
    uint8_t extra;
    ssize_t extra_count;
    do {
        extra_count = read(_identityFD, &extra, 1);
    } while (extra_count < 0 && errno == EINTR);
    struct stat after;
    if (extra_count != 0 || fstat(_identityFD, &after) != 0
        || !same_stat(&_identityStat, &after)) {
        fail(error, @"subject-identity-changed-during-read");
        return nil;
    }
    if (memchr(data.bytes, '\0', data.length) != NULL
        || memchr(data.bytes, '\r', data.length) != NULL) {
        fail(error, @"subject-identity-line-encoding-invalid");
        return nil;
    }
    return data;
}

- (BOOL)loadFromPath:(NSString *)path error:(NSString **)error {
    self.identityPath = canonical_path(path);
    if (self.identityPath == nil) {
        return fail(error, @"subject-identity-path-unavailable");
    }
    NSData *data = [self readIdentity:self.identityPath error:error];
    if (data == nil) {
        return NO;
    }
    self.identitySHA256 = sha256_for_bytes(data.bytes, data.length);
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
    if (lines.count != keys.count) {
        return fail(error, @"subject-identity-schema-width-mismatch");
    }
    NSMutableDictionary<NSString *, NSString *> *values = [NSMutableDictionary dictionary];
    for (NSUInteger index = 0; index < keys.count; index += 1) {
        NSArray<NSString *> *fields = [lines[index] componentsSeparatedByString:@"\t"];
        if (fields.count != 2 || ![fields[0] isEqualToString:keys[index]]
            || values[fields[0]] != nil || !safe_field(fields[1], 4096)) {
            return fail(error, @"subject-identity-schema-invalid-unknown-or-duplicate");
        }
        values[fields[0]] = fields[1];
    }

    uint64_t pid = 0;
    uint64_t device = 0;
    uint64_t inode = 0;
    uint64_t fsid = 0;
    NSString *app = canonical_path(values[@"app_bundle_path"]);
    NSString *executable = canonical_path(values[@"executable_path"]);
    BOOL valid = [values[@"format_version"] isEqualToString:@"1"]
        && ([values[@"subject"] isEqualToString:@"spaceterm"]
            || [values[@"subject"] isEqualToString:@"ghostty"])
        && app != nil && executable != nil && [app.pathExtension isEqualToString:@"app"]
        && [executable hasPrefix:[app stringByAppendingString:@"/"]]
        && [app isEqualToString:values[@"app_bundle_path"]]
        && [executable isEqualToString:values[@"executable_path"]]
        && safe_label(values[@"bundle_identifier"])
        && safe_label(values[@"bundle_version"])
        && lower_hex(values[@"executable_sha256"], 64, 64)
        && parse_uint(values[@"executable_device"], UINT64_MAX, &device)
        && parse_uint(values[@"executable_inode"], UINT64_MAX, &inode)
        && parse_uint(values[@"executable_fsid"], UINT64_MAX, &fsid)
        && device == fsid
        && [values[@"signature_valid"] isEqualToString:@"true"]
        && safe_label(values[@"signing_identifier"])
        && safe_label(values[@"team_identifier"])
        && lower_hex(values[@"cdhash"], 8, 128)
        && parse_uint(values[@"process_pid"], INT32_MAX, &pid) && pid > 0
        && safe_field(values[@"process_start_identity"], 64)
        && [values[@"identity_status"] isEqualToString:@"frozen"];
    if (!valid) {
        return fail(error, @"subject-identity-value-invalid");
    }

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
    NSString *info_path = [self.appBundlePath stringByAppendingPathComponent:@"Contents/Info.plist"];
    if (_executableFD < 0 || fstat(_executableFD, &_executableStat) != 0
        || !S_ISREG(_executableStat.st_mode)
        || (uint64_t)_executableStat.st_dev != self.executableDevice
        || (uint64_t)_executableStat.st_ino != self.executableInode
        || stat(self.appBundlePath.fileSystemRepresentation, &_appBundleStat) != 0
        || !S_ISDIR(_appBundleStat.st_mode)
        || stat(info_path.fileSystemRepresentation, &_infoPlistStat) != 0
        || !S_ISREG(_infoPlistStat.st_mode)) {
        return fail(error, @"subject-filesystem-identity-mismatch");
    }
    NSString *hash = sha256_for_fd(_executableFD, error);
    if (hash == nil || ![hash isEqualToString:self.executableSHA256]) {
        return fail(error, @"subject-executable-hash-mismatch");
    }

    struct proc_bsdinfo information;
    memset(&information, 0, sizeof(information));
    int count = proc_pidinfo(self.pid,
                             PROC_PIDTBSDINFO,
                             0,
                             &information,
                             sizeof(information));
    char rendered[64];
    NSString *rendered_value = nil;
    if (count == sizeof(information) && information.pbi_status != SZOMB
        && information.pbi_uid == geteuid()
        && snprintf(rendered, sizeof(rendered), "%llu:%llu",
                    (unsigned long long)information.pbi_start_tvsec,
                    (unsigned long long)information.pbi_start_tvusec) > 0) {
        rendered_value = [NSString stringWithUTF8String:rendered];
    }
    if (rendered_value == nil || ![rendered_value isEqualToString:self.processStartIdentity]) {
        return fail(error, @"subject-process-start-identity-mismatch");
    }
    self.startSeconds = information.pbi_start_tvsec;
    self.startMicroseconds = information.pbi_start_tvusec;
    return [self verify:error];
}

- (BOOL)verifyIdentityFile:(NSString **)error {
    struct stat descriptor;
    struct stat path;
    if (fstat(_identityFD, &descriptor) != 0
        || stat(self.identityPath.fileSystemRepresentation, &path) != 0
        || !same_stat(&_identityStat, &descriptor)
        || !same_stat(&_identityStat, &path)
        || (_identityStat.st_mode & 0222) != 0) {
        return fail(error, @"subject-identity-file-changed");
    }
    NSString *hash = sha256_for_fd(_identityFD, error);
    return hash != nil && [hash isEqualToString:self.identitySHA256]
        ? YES : fail(error, @"subject-identity-digest-changed");
}

- (BOOL)verifyProcess:(NSString **)error {
    struct proc_bsdinfo information;
    memset(&information, 0, sizeof(information));
    int count = proc_pidinfo(self.pid,
                             PROC_PIDTBSDINFO,
                             0,
                             &information,
                             sizeof(information));
    if (count != sizeof(information) || information.pbi_status == SZOMB
        || information.pbi_uid != geteuid()
        || information.pbi_start_tvsec != self.startSeconds
        || information.pbi_start_tvusec != self.startMicroseconds) {
        return fail(error, @"subject-process-identity-changed");
    }
    char buffer[PROC_PIDPATHINFO_MAXSIZE];
    int length = proc_pidpath(self.pid, buffer, sizeof(buffer));
    if (length <= 0 || (size_t)length >= sizeof(buffer)) {
        return fail(error, @"subject-process-path-unavailable");
    }
    buffer[length] = '\0';
    NSString *reported = [[NSFileManager defaultManager]
        stringWithFileSystemRepresentation:buffer length:(NSUInteger)length];
    NSString *canonical = canonical_path(reported);
    return canonical != nil && [canonical isEqualToString:self.executablePath]
        ? YES : fail(error, @"subject-process-path-changed");
}

- (BOOL)verifyFiles:(NSString **)error {
    struct stat executable_descriptor;
    struct stat executable_path;
    struct stat app;
    struct stat info;
    NSString *info_path = [self.appBundlePath stringByAppendingPathComponent:@"Contents/Info.plist"];
    if (fstat(_executableFD, &executable_descriptor) != 0
        || stat(self.executablePath.fileSystemRepresentation, &executable_path) != 0
        || stat(self.appBundlePath.fileSystemRepresentation, &app) != 0
        || stat(info_path.fileSystemRepresentation, &info) != 0
        || !same_stat(&_executableStat, &executable_descriptor)
        || !same_stat(&_executableStat, &executable_path)
        || !same_stat(&_appBundleStat, &app)
        || !same_stat(&_infoPlistStat, &info)) {
        return fail(error, @"subject-package-vnode-changed");
    }
    NSString *hash = sha256_for_fd(_executableFD, error);
    return hash != nil && [hash isEqualToString:self.executableSHA256]
        ? YES : fail(error, @"subject-executable-hash-changed");
}

- (BOOL)signingInformation:(NSDictionary **)result dynamic:(BOOL)dynamic error:(NSString **)error {
    SecCodeRef code = NULL;
    OSStatus status;
    if (dynamic) {
        NSDictionary *attributes = @{(__bridge id)kSecGuestAttributePid: @(self.pid)};
        status = SecCodeCopyGuestWithAttributes(
            NULL, (__bridge CFDictionaryRef)attributes, kSecCSDefaultFlags, &code);
        if (status == errSecSuccess && code != NULL) {
            status = SecCodeCheckValidity(code, kSecCSStrictValidate, NULL);
        }
    } else {
        NSURL *url = [NSURL fileURLWithPath:self.appBundlePath isDirectory:YES];
        status = SecStaticCodeCreateWithPath(
            (__bridge CFURLRef)url, kSecCSDefaultFlags, (SecStaticCodeRef *)&code);
        if (status == errSecSuccess && code != NULL) {
            status = SecStaticCodeCheckValidity(
                (SecStaticCodeRef)code,
                kSecCSStrictValidate | kSecCSCheckAllArchitectures,
                NULL);
        }
    }
    if (status != errSecSuccess || code == NULL) {
        if (code != NULL) CFRelease(code);
        return fail(error, dynamic ? @"subject-live-code-invalid" : @"subject-static-code-invalid");
    }
    CFDictionaryRef raw = NULL;
    status = SecCodeCopySigningInformation(
        code,
        kSecCSSigningInformation | (dynamic ? kSecCSDynamicInformation : 0),
        &raw);
    CFRelease(code);
    if (status != errSecSuccess || raw == NULL) {
        return fail(error, @"subject-signing-information-unavailable");
    }
    *result = CFBridgingRelease(raw);
    return YES;
}

- (BOOL)verifySigning:(NSString **)error {
    for (NSNumber *dynamic_number in @[@YES, @NO]) {
        BOOL dynamic = dynamic_number.boolValue;
        NSDictionary *information = nil;
        if (![self signingInformation:&information dynamic:dynamic error:error]) {
            return NO;
        }
        NSString *identifier = information[(__bridge id)kSecCodeInfoIdentifier];
        NSString *team = information[(__bridge id)kSecCodeInfoTeamIdentifier];
        NSData *unique = information[(__bridge id)kSecCodeInfoUnique];
        NSURL *main_executable = information[(__bridge id)kSecCodeInfoMainExecutable];
        NSString *main_path = canonical_path(main_executable.path);
        BOOL team_matches = self.teamIdentifier == nil ? team == nil
            : [team isKindOfClass:[NSString class]] && [team isEqualToString:self.teamIdentifier];
        if (![identifier isEqualToString:self.signingIdentifier]
            || !team_matches
            || ![unique isKindOfClass:[NSData class]]
            || ![[hex_data(unique) lowercaseString] isEqualToString:self.cdhash]
            || main_path == nil || ![main_path isEqualToString:self.executablePath]) {
            return fail(error, @"subject-signing-identity-changed");
        }
    }
    return YES;
}

- (BOOL)verifyBundle:(NSString **)error {
    NSRunningApplication *application = [NSRunningApplication
        runningApplicationWithProcessIdentifier:self.pid];
    NSString *bundle_path = canonical_path(application.bundleURL.path);
    NSString *executable_path = canonical_path(application.executableURL.path);
    if (application == nil || application.terminated
        || ![application.bundleIdentifier isEqualToString:self.bundleIdentifier]
        || ![bundle_path isEqualToString:self.appBundlePath]
        || ![executable_path isEqualToString:self.executablePath]) {
        return fail(error, @"subject-running-bundle-identity-changed");
    }
    NSDictionary *info = [NSDictionary dictionaryWithContentsOfFile:
        [self.appBundlePath stringByAppendingPathComponent:@"Contents/Info.plist"]];
    NSString *marketing = info[@"CFBundleShortVersionString"];
    NSString *build = info[@"CFBundleVersion"];
    NSString *version = [NSString stringWithFormat:
        @"%@+%@",
        marketing != nil ? marketing : @"",
        build != nil ? build : @""];
    if (![info[@"CFBundleIdentifier"] isEqualToString:self.bundleIdentifier]
        || ![version isEqualToString:self.bundleVersion]) {
        return fail(error, @"subject-bundle-metadata-changed");
    }
    return YES;
}

- (BOOL)verify:(NSString **)error {
    return [self verifyIdentityFile:error]
        && [self verifyProcess:error]
        && [self verifyFiles:error]
        && [self verifyBundle:error]
        && [self verifySigning:error]
        && [self verifyProcess:error]
        && [self verifyFiles:error];
}

- (BOOL)residentKiB:(uint64_t *)rss error:(NSString **)error {
    struct rusage_info_v4 usage;
    memset(&usage, 0, sizeof(usage));
    if (proc_pid_rusage(self.pid, RUSAGE_INFO_V4, (rusage_info_t *)&usage) != 0) {
        return fail(error, @"subject-resident-memory-unavailable");
    }
    *rss = (usage.ri_resident_size + 1023) / 1024;
    return *rss > 0 ? YES : fail(error, @"subject-resident-memory-invalid");
}

@end

static NSDictionary<NSString *, NSString *> *parse_options(int argc,
                                                             const char *argv[],
                                                             NSString **error) {
    NSArray<NSString *> *required = @[
        @"--subject-identity", @"--plan-start-continuous-ns", @"--warmup-ms",
        @"--duration-ms", @"--plan-start-gate-sha256",
        @"--ready-receipt-sha256", @"--output",
    ];
    NSSet<NSString *> *allowed = [NSSet setWithArray:required];
    NSMutableDictionary<NSString *, NSString *> *result = [NSMutableDictionary dictionary];
    for (int index = 1; index < argc; index += 2) {
        if (index + 1 >= argc) {
            fail(error, @"option-missing-value");
            return nil;
        }
        NSString *key = [NSString stringWithUTF8String:argv[index]];
        NSString *value = [NSString stringWithUTF8String:argv[index + 1]];
        if (key == nil || value == nil || ![allowed containsObject:key] || result[key] != nil) {
            fail(error, @"unknown-or-duplicate-option");
            return nil;
        }
        result[key] = value;
    }
    for (NSString *key in required) {
        if (result[key] == nil) {
            fail(error, @"required-option-missing");
            return nil;
        }
    }
    return result;
}

typedef struct {
    int directory_fd;
    int file_fd;
    FILE *stream;
    char temporary_name[64];
    char final_name[NAME_MAX + 1];
} AtomicOutput;

static void discard_output(AtomicOutput *output) {
    if (output->stream != NULL) {
        fclose(output->stream);
        output->stream = NULL;
        output->file_fd = -1;
    } else if (output->file_fd >= 0) {
        close(output->file_fd);
        output->file_fd = -1;
    }
    if (output->directory_fd >= 0 && output->temporary_name[0] != '\0') {
        unlinkat(output->directory_fd, output->temporary_name, 0);
    }
    if (output->directory_fd >= 0) {
        close(output->directory_fd);
        output->directory_fd = -1;
    }
}

static BOOL create_output(NSString *path, AtomicOutput *output, NSString **error) {
    memset(output, 0, sizeof(*output));
    output->directory_fd = -1;
    output->file_fd = -1;
    NSString *directory = [path stringByDeletingLastPathComponent];
    if (directory.length == 0) directory = @".";
    NSString *name = path.lastPathComponent;
    if (!safe_field(name, NAME_MAX) || [name isEqualToString:@"."] || [name isEqualToString:@".."]
        || [name containsString:@"/"]) {
        return fail(error, @"output-name-invalid");
    }
    output->directory_fd = open(
        directory.fileSystemRepresentation,
        O_RDONLY | O_CLOEXEC | O_DIRECTORY | O_NOFOLLOW);
    if (output->directory_fd < 0) {
        return fail(error, @"output-directory-open-failed");
    }
    struct stat existing;
    if (fstatat(output->directory_fd, name.fileSystemRepresentation, &existing, AT_SYMLINK_NOFOLLOW) == 0
        || errno != ENOENT) {
        discard_output(output);
        return fail(error, @"output-already-exists-or-unavailable");
    }
    snprintf(output->temporary_name,
             sizeof(output->temporary_name),
             ".rss-samples.%d.XXXXXX",
             getpid());
    output->file_fd = mkostempsat_np(
        output->directory_fd,
        output->temporary_name,
        0,
        O_CLOEXEC);
    if (output->file_fd < 0) {
        discard_output(output);
        return fail(error, @"output-temporary-create-failed");
    }
    if (fchmod(output->file_fd, S_IRUSR | S_IWUSR) != 0) {
        discard_output(output);
        return fail(error, @"output-permission-set-failed");
    }
    output->stream = fdopen(output->file_fd, "w");
    if (output->stream == NULL) {
        discard_output(output);
        return fail(error, @"output-stream-create-failed");
    }
    strlcpy(output->final_name, name.fileSystemRepresentation, sizeof(output->final_name));
    return YES;
}

static BOOL publish_output(AtomicOutput *output, NSString **error) {
    BOOL valid = output->stream != NULL && ferror(output->stream) == 0;
    if (valid) valid = fflush(output->stream) == 0 && ferror(output->stream) == 0;
    if (valid) valid = fchmod(output->file_fd, S_IRUSR) == 0;
    if (valid) valid = fsync(output->file_fd) == 0;
    int close_status = output->stream != NULL ? fclose(output->stream) : -1;
    output->stream = NULL;
    output->file_fd = -1;
    valid = valid && close_status == 0;
    if (!valid) {
        discard_output(output);
        return fail(error, @"output-flush-failed");
    }
    if (renameatx_np(output->directory_fd,
                     output->temporary_name,
                     output->directory_fd,
                     output->final_name,
                     RENAME_EXCL) != 0) {
        discard_output(output);
        return fail(error, @"output-publish-failed");
    }
    output->temporary_name[0] = '\0';
    if (fsync(output->directory_fd) != 0) {
        unlinkat(output->directory_fd, output->final_name, 0);
        fsync(output->directory_fd);
        discard_output(output);
        return fail(error, @"output-directory-flush-failed");
    }
    close(output->directory_fd);
    output->directory_fd = -1;
    return YES;
}

int main(int argc, const char *argv[]) {
    @autoreleasepool {
        umask(077);
        setlocale(LC_ALL, "C");
        if (argc == 2 && (strcmp(argv[1], "--help") == 0 || strcmp(argv[1], "-h") == 0)) {
            usage(stdout);
            return 0;
        }
        NSString *error = nil;
        NSDictionary<NSString *, NSString *> *options = parse_options(argc, argv, &error);
        if (options == nil) {
            usage(stderr);
            fprintf(stderr, "error: %s\n", error.UTF8String);
            return 64;
        }
        uint64_t warmup = 0;
        uint64_t duration = 0;
        uint64_t plan_start = 0;
        if (!parse_uint(options[@"--plan-start-continuous-ns"], UINT64_MAX, &plan_start)
            || plan_start == 0
            || !parse_uint(options[@"--warmup-ms"], kMaximumWarmupMilliseconds, &warmup)
            || !parse_uint(options[@"--duration-ms"], kMaximumDurationMilliseconds, &duration)
            || duration == 0 || duration % kSampleIntervalMilliseconds != 0
            || warmup + duration + kFirstSampleDelayMilliseconds
                > (UINT64_MAX - plan_start) / 1000000ULL
            || !lower_hex(options[@"--plan-start-gate-sha256"], 64, 64)
            || !lower_hex(options[@"--ready-receipt-sha256"], 64, 64)) {
            fprintf(stderr, "error: duration must be a positive 10,000 ms multiple and bounds apply\n");
            return 64;
        }

        struct sigaction signal_action;
        memset(&signal_action, 0, sizeof(signal_action));
        signal_action.sa_handler = handle_signal;
        sigemptyset(&signal_action.sa_mask);
        sigaction(SIGINT, &signal_action, NULL);
        sigaction(SIGTERM, &signal_action, NULL);

        FrozenSubject *subject = [FrozenSubject new];
        if (![subject loadFromPath:options[@"--subject-identity"] error:&error]) {
            fprintf(stderr, "error: %s\n", error.UTF8String);
            return 65;
        }
        AtomicOutput output;
        if (!create_output(options[@"--output"], &output, &error)) {
            fprintf(stderr, "error: %s\n", error.UTF8String);
            return 73;
        }
        BOOL output_valid = fprintf(output.stream, "elapsed_ms\tcontinuous_ns\trss_kib\n") >= 0
            && fprintf(output.stream, "# format_version\t1\n") >= 0
            && fprintf(output.stream, "# sample_interval_ms\t%llu\n", kSampleIntervalMilliseconds) >= 0
            && fprintf(output.stream, "# requested_warmup_ms\t%llu\n", warmup) >= 0
            && fprintf(output.stream, "# requested_duration_ms\t%llu\n", duration) >= 0
            && fprintf(output.stream, "# plan_start_continuous_ns\t%llu\n", plan_start) >= 0
            && fprintf(output.stream,
                       "# measurement_start_continuous_ns\t%llu\n",
                       plan_start + warmup * 1000000ULL) >= 0
            && fprintf(output.stream, "# plan_start_gate_sha256\t%s\n",
                       options[@"--plan-start-gate-sha256"].UTF8String) >= 0
            && fprintf(output.stream, "# ready_receipt_sha256\t%s\n",
                       options[@"--ready-receipt-sha256"].UTF8String) >= 0
            && fprintf(output.stream,
                       "# subject_identity_sha256\t%s\n",
                       subject.identitySHA256.UTF8String) >= 0;

        if (!output_valid || ferror(output.stream) != 0) {
            discard_output(&output);
            fprintf(stderr, "error: output metadata write failed\n");
            return 74;
        }

        uint64_t sampler_ready = continuous_nanoseconds();
        if (plan_start < sampler_ready || plan_start - sampler_ready > 30000000000ULL) {
            discard_output(&output);
            fprintf(stderr, "error: plan start must be now through 30 seconds ahead\n");
            return 64;
        }
        uint64_t warmup_deadline = plan_start + warmup * 1000000ULL;
        while (!gInterrupted && continuous_nanoseconds() < warmup_deadline) {
            uint64_t now = continuous_nanoseconds();
            uint64_t next_check = now + kSampleIntervalMilliseconds * 1000000ULL;
            if (next_check > warmup_deadline) next_check = warmup_deadline;
            if (!wait_until(next_check) || ![subject verify:&error]) {
                break;
            }
        }
        if (gInterrupted || continuous_nanoseconds() < warmup_deadline || error != nil) {
            discard_output(&output);
            NSString *reason = error != nil ? error : @"interrupted during warm-up";
            fprintf(stderr, "error: %s\n", reason.UTF8String);
            return 70;
        }
        uint64_t measurement_started = warmup_deadline;
        NSUInteger sample_count = (NSUInteger)(duration / kSampleIntervalMilliseconds) + 1;
        BOOL samples_valid = output_valid;
        for (NSUInteger index = 0; index < sample_count && samples_valid; index += 1) {
            uint64_t scheduled_elapsed = kFirstSampleDelayMilliseconds
                + index * kSampleIntervalMilliseconds;
            uint64_t deadline = measurement_started + scheduled_elapsed * 1000000ULL;
            if (!wait_until(deadline) || ![subject verify:&error]) {
                samples_valid = NO;
                break;
            }
            uint64_t rss_kib = 0;
            if (![subject residentKiB:&rss_kib error:&error]) {
                samples_valid = NO;
                break;
            }
            uint64_t sample_time = continuous_nanoseconds();
            if (sample_time > deadline + kMaximumSampleLatenessNanoseconds
                || ![subject verify:&error]) {
                samples_valid = NO;
                break;
            }
            samples_valid = fprintf(output.stream,
                                    "%llu\t%llu\t%llu\n",
                                    scheduled_elapsed,
                                    sample_time,
                                    rss_kib) >= 0;
        }
        if (samples_valid) {
            samples_valid = [subject verify:&error]
                && fprintf(output.stream, "# status\tcomplete\n") >= 0
                && ferror(output.stream) == 0;
        }
        if (!samples_valid) {
            discard_output(&output);
            NSString *reason = error != nil ? error : @"RSS sampling failed";
            fprintf(stderr, "error: %s\n", reason.UTF8String);
            return 70;
        }
        if (!publish_output(&output, &error)) {
            fprintf(stderr, "error: %s\n", error.UTF8String);
            return 74;
        }
        return 0;
    }
}
