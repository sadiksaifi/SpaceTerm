#import <AppKit/AppKit.h>
#import <ApplicationServices/ApplicationServices.h>
#import <CommonCrypto/CommonDigest.h>
#import <Foundation/Foundation.h>
#import <Security/Security.h>

#include <errno.h>
#include <fcntl.h>
#include <fts.h>
#include <libproc.h>
#include <mach/mach_time.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/proc_info.h>
#include <sys/stat.h>
#include <unistd.h>

static NSString *const kSubjectSchema = @"spaceterm.acceptance.ax-subject/v1";
static NSString *const kResultSchema = @"spaceterm.acceptance.native-ax-observation/v1";
static NSString *const kLaunchObservationSchema = @"spaceterm.acceptance.native-launch-proof/v5";
static NSString *const kExpectedBundleIdentifier = @"io.github.sadiksaifi.spaceterm";
static NSString *const kExpectedPaneLabel = @"Terminal Pane";
static const NSUInteger kMaximumIdentityBytes = 64 * 1024;
static const NSUInteger kMaximumFixtureBytes = 16 * 1024 * 1024;
static const NSUInteger kMaximumProbeBytes = 32 * 1024 * 1024;
static const NSUInteger kMaximumAXNodes = 4096;
static const NSUInteger kMaximumAXDepth = 64;

typedef NS_ENUM(NSUInteger, PrivacyMode) {
    PrivacyModeMetadataOnly,
    PrivacyModeFixtureSentinel,
};

typedef struct {
    CFIndex location;
    CFIndex length;
} CheckedRange;

typedef struct {
    BOOL set;
    CheckedRange value;
} OptionalRange;

typedef struct {
    __strong NSString *runDirectory;
    __strong NSString *identityPath;
    __strong NSString *outputPath;
    __strong NSString *expectedRunID;
    BOOL expectedFailureActionEnabledSet;
    BOOL expectedFailureActionEnabled;
    PrivacyMode privacy;
    __strong NSString *fixturePath;
    __strong NSString *fixtureSHA256;
    __strong NSString *fixtureAfterPath;
    __strong NSString *fixtureAfterSHA256;
    NSUInteger expectedPaneCount;
    NSUInteger paneOrder;
    OptionalRange expectedBeforeSelection;
    OptionalRange requestedSelection;
    OptionalRange expectedAfterSelection;
    CFIndex probeLine;
    CFIndex probeIndex;
    CheckedRange probeRange;
    NSUInteger probeCoordinatesSet;
    NSUInteger observeMilliseconds;
    NSUInteger minimumValueNotifications;
    NSUInteger minimumSelectionNotifications;
    NSUInteger minimumFocusNotifications;
    NSUInteger minimumLayoutNotifications;
} Options;

typedef struct {
    pid_t pid;
    uint64_t startSeconds;
    uint64_t startMicroseconds;
    dev_t executableDevice;
    ino_t executableInode;
    int32_t fsid0;
    int32_t fsid1;
    __strong NSString *runID;
    __strong NSString *launchNonce;
    __strong NSString *launchObservationSHA256;
    __strong NSString *appSHA256;
    __strong NSString *bundlePath;
    __strong NSString *bundleIdentifier;
    __strong NSString *executablePath;
    __strong NSString *cdhash;
    __strong NSString *signingIdentifier;
    __strong NSString *teamIdentifier;
} SubjectIdentity;

typedef struct {
    CheckedRange visibleRange;
    CheckedRange selectedRange;
    CGRect frame;
    CFIndex characterCount;
    BOOL focused;
    BOOL valueQueried;
    BOOL valueMatches;
    __strong NSString *valueSHA256;
    BOOL selectedTextQueried;
    __strong NSString *selectedTextSHA256;
} PaneSnapshot;

typedef struct {
    uint64_t count;
    uint64_t firstContinuousNS;
    uint64_t lastContinuousNS;
} NotificationAggregate;

@interface ObserverState : NSObject
@property(nonatomic, strong) id targetObject;
@property(nonatomic, strong) id parentObject;
@property(nonatomic, strong) id applicationObject;
@property(nonatomic) pid_t expectedPID;
@property(nonatomic) BOOL identityMismatch;
@property(nonatomic) uint64_t baselineContinuousNS;
@property(nonatomic) uint64_t selectionDispatchContinuousNS;
@property(nonatomic) uint64_t observationDeadlineContinuousNS;
@property(nonatomic) NotificationAggregate value;
@property(nonatomic) NotificationAggregate selection;
@property(nonatomic) NotificationAggregate focus;
@property(nonatomic) NotificationAggregate focusTarget;
@property(nonatomic) NotificationAggregate focusOther;
@property(nonatomic) NotificationAggregate layout;
@end

@implementation ObserverState
@end

static BOOL fixture_substring(NSString *fixture, CheckedRange range, NSString **result);
static NSString *canonical_path(NSString *path);
static BOOL validate_live_subject(const SubjectIdentity *subject);

static BOOL report(NSString *message) {
    fprintf(stderr, "native-ax-probe: %s\n", message.UTF8String);
    return NO;
}

static void usage(FILE *stream) {
    fputs(
        "Usage:\n"
        "  native-ax-probe --self-test\n"
        "  native-ax-probe --self-test-bundle BUNDLE EXPECTED_TREE_SHA256\n"
        "  native-ax-probe --run-dir DIR --identity FILE --output FILE [OPTIONS]\n\n"
        "Required options:\n"
        "  --expected-run-id RUN_ID\n"
        "  --expected-failure-action-enabled true|false\n"
        "  --privacy metadata-only|fixture-sentinel\n"
        "  --expected-pane-count COUNT --pane-order ZERO_BASED_ORDER\n\n"
        "Fixture-sentinel mode additionally requires:\n"
        "  --fixture-file FILE --fixture-sha256 LOWER_HEX_SHA256\n"
        "  [--fixture-after-file FILE --fixture-after-sha256 LOWER_HEX_SHA256]\n\n"
        "Query and mutation options:\n"
        "  --expected-before-selected LOCATION:LENGTH\n"
        "  --set-selected LOCATION:LENGTH\n"
        "  --expected-after-selected LOCATION:LENGTH\n"
        "  --probe-line LINE --probe-index INDEX --probe-range LOCATION:LENGTH\n"
        "  --observe-ms MILLISECONDS\n"
        "  --expect-value COUNT --expect-selection COUNT\n"
        "  --expect-focus COUNT --expect-layout COUNT\n\n"
        "The probe never launches or discovers an application. It requires Accessibility\n"
        "permission without prompting, validates the exact frozen live subject, queries only\n"
        "the selected AXTextArea, and creates evidence exclusively without overwrite.\n",
        stream);
}

static BOOL is_decimal(NSString *value) {
    if (value.length == 0) {
        return NO;
    }
    NSCharacterSet *nonDigits = [[NSCharacterSet decimalDigitCharacterSet] invertedSet];
    return [value rangeOfCharacterFromSet:nonDigits].location == NSNotFound;
}

static BOOL parse_uint64(NSString *value, uint64_t *result) {
    if (!is_decimal(value)) {
        return NO;
    }
    const char *bytes = value.UTF8String;
    errno = 0;
    char *end = NULL;
    unsigned long long parsed = strtoull(bytes, &end, 10);
    if (errno != 0 || end == bytes || *end != '\0') {
        return NO;
    }
    *result = (uint64_t)parsed;
    return YES;
}

static BOOL parse_positive_pid(NSString *value, pid_t *result) {
    uint64_t parsed = 0;
    if (!parse_uint64(value, &parsed) || parsed == 0 || parsed > INT32_MAX) {
        return NO;
    }
    *result = (pid_t)parsed;
    return YES;
}

static BOOL parse_int32(NSString *value, int32_t *result) {
    if (value.length == 0) {
        return NO;
    }
    const char *bytes = value.UTF8String;
    errno = 0;
    char *end = NULL;
    long long parsed = strtoll(bytes, &end, 10);
    if (errno != 0 || end == bytes || *end != '\0' || parsed < INT32_MIN || parsed > INT32_MAX) {
        return NO;
    }
    *result = (int32_t)parsed;
    return YES;
}

static BOOL parse_index(NSString *value, CFIndex *result) {
    uint64_t parsed = 0;
    if (!parse_uint64(value, &parsed) || parsed > (uint64_t)LONG_MAX) {
        return NO;
    }
    *result = (CFIndex)parsed;
    return YES;
}

static BOOL range_is_valid(CheckedRange range, CFIndex maximum) {
    return range.location >= 0 && range.length >= 0 && range.location <= maximum &&
        range.length <= maximum - range.location;
}

static BOOL parse_range(NSString *value, CheckedRange *result) {
    NSArray<NSString *> *parts = [value componentsSeparatedByString:@":"];
    if (parts.count != 2) {
        return NO;
    }
    return parse_index(parts[0], &result->location) &&
        parse_index(parts[1], &result->length) &&
        range_is_valid(*result, LONG_MAX);
}

static BOOL lower_hex(NSString *value, NSUInteger length) {
    if (value.length != length) {
        return NO;
    }
    NSCharacterSet *allowed = [NSCharacterSet characterSetWithCharactersInString:@"0123456789abcdef"];
    return [value rangeOfCharacterFromSet:allowed.invertedSet].location == NSNotFound;
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

static NSString *sha256_file_streaming(NSString *path) {
    int descriptor = open(path.fileSystemRepresentation, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    if (descriptor < 0) {
        return nil;
    }
    CC_SHA256_CTX context;
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
    BOOL success = CC_SHA256_Init(&context) == 1;
    uint8_t buffer[64 * 1024];
    while (success) {
        ssize_t count = read(descriptor, buffer, sizeof(buffer));
        if (count == 0) break;
        if (count < 0) {
            if (errno == EINTR) continue;
            success = NO;
            break;
        }
        success = CC_SHA256_Update(&context, buffer, (CC_LONG)count) == 1;
    }
    uint8_t digest[CC_SHA256_DIGEST_LENGTH];
    success = success && CC_SHA256_Final(digest, &context) == 1;
#pragma clang diagnostic pop
    if (close(descriptor) != 0) success = NO;
    if (!success) return nil;
    NSMutableString *result = [NSMutableString stringWithCapacity:CC_SHA256_DIGEST_LENGTH * 2];
    for (NSUInteger index = 0; index < CC_SHA256_DIGEST_LENGTH; index++) {
        [result appendFormat:@"%02x", digest[index]];
    }
    return result;
}

static NSString *tree_encode_value(NSString *value) {
    NSMutableString *encoded = [value mutableCopy];
    [encoded replaceOccurrencesOfString:@"%" withString:@"%25"
        options:0 range:NSMakeRange(0, encoded.length)];
    [encoded replaceOccurrencesOfString:@"\t" withString:@"%09"
        options:0 range:NSMakeRange(0, encoded.length)];
    [encoded replaceOccurrencesOfString:@"\r" withString:@"%0D"
        options:0 range:NSMakeRange(0, encoded.length)];
    [encoded replaceOccurrencesOfString:@"\n" withString:@"%0A"
        options:0 range:NSMakeRange(0, encoded.length)];
    return encoded;
}

static NSComparisonResult filesystem_path_compare(NSString *left, NSString *right) {
    int comparison = strcmp(left.fileSystemRepresentation, right.fileSystemRepresentation);
    if (comparison < 0) return NSOrderedAscending;
    if (comparison > 0) return NSOrderedDescending;
    return NSOrderedSame;
}

static NSString *canonical_bundle_tree_sha256(NSString *root) {
    NSString *canonicalRoot = canonical_path(root);
    if (canonicalRoot == nil) {
        return nil;
    }
    char *rootPath = strdup(canonicalRoot.fileSystemRepresentation);
    if (rootPath == NULL) {
        return nil;
    }
    char *paths[] = {rootPath, NULL};
    FTS *tree = fts_open(paths, FTS_PHYSICAL | FTS_NOCHDIR, NULL);
    if (tree == NULL) {
        free(rootPath);
        return nil;
    }
    NSMutableArray<NSString *> *entries = [NSMutableArray array];
    FTSENT *entry = NULL;
    BOOL valid = YES;
    size_t rootLength = strlen(rootPath);
    while ((entry = fts_read(tree)) != NULL) {
        if (entry->fts_level == 0) {
            continue;
        }
        if (entry->fts_info == FTS_DP) {
            continue;
        }
        if (entry->fts_info == FTS_ERR || entry->fts_info == FTS_DNR ||
            entry->fts_info == FTS_NS || entry->fts_info == FTS_DC ||
            strncmp(entry->fts_path, rootPath, rootLength) != 0 ||
            entry->fts_path[rootLength] != '/') {
            valid = NO;
            break;
        }
        NSString *suffix = [NSString stringWithUTF8String:entry->fts_path + rootLength];
        if (suffix == nil) {
            valid = NO;
            break;
        }
        [entries addObject:[@"." stringByAppendingString:suffix]];
    }
    if (fts_close(tree) != 0) {
        valid = NO;
    }
    free(rootPath);
    if (!valid) {
        return nil;
    }
    [entries sortUsingComparator:^NSComparisonResult(NSString *left, NSString *right) {
        return filesystem_path_compare(left, right);
    }];
    NSMutableData *stream = [NSMutableData data];
    for (NSString *relative in entries) {
        NSString *absolute = [canonicalRoot stringByAppendingPathComponent:
            [relative substringFromIndex:2]];
        struct stat status = {0};
        if (lstat(absolute.fileSystemRepresentation, &status) != 0) {
            return nil;
        }
        NSString *mode = [NSString stringWithFormat:@"%o", status.st_mode & 07777];
        NSString *row = nil;
        if (S_ISLNK(status.st_mode)) {
            size_t capacity = status.st_size > 0 ? (size_t)status.st_size + 1 : PATH_MAX;
            char *targetBytes = calloc(capacity, 1);
            if (targetBytes == NULL) return nil;
            ssize_t count = readlink(absolute.fileSystemRepresentation, targetBytes, capacity - 1);
            NSString *target = count >= 0
                ? [[NSString alloc] initWithBytes:targetBytes length:(NSUInteger)count
                    encoding:NSUTF8StringEncoding] : nil;
            free(targetBytes);
            if (target == nil) return nil;
            row = [NSString stringWithFormat:@"symlink\t%@\t%@\t%@\n",
                tree_encode_value(relative), mode, tree_encode_value(target)];
        } else if (S_ISDIR(status.st_mode)) {
            row = [NSString stringWithFormat:@"directory\t%@\t%@\n",
                tree_encode_value(relative), mode];
        } else if (S_ISREG(status.st_mode)) {
            NSString *digest = sha256_file_streaming(absolute);
            if (digest == nil) return nil;
            row = [NSString stringWithFormat:@"file\t%@\t%@\t%@\n",
                tree_encode_value(relative), mode, digest];
        } else {
            return nil;
        }
        NSData *rowData = [row dataUsingEncoding:NSUTF8StringEncoding];
        if (rowData == nil) return nil;
        [stream appendData:rowData];
    }
    return lower_sha256(stream);
}

static NSString *canonical_path(NSString *path) {
    char resolved[PATH_MAX];
    if (realpath(path.fileSystemRepresentation, resolved) == NULL) {
        return nil;
    }
    return [NSString stringWithUTF8String:resolved];
}

static BOOL private_real_directory(NSString *path) {
    struct stat status = {0};
    if (lstat(path.fileSystemRepresentation, &status) != 0 || !S_ISDIR(status.st_mode) ||
        S_ISLNK(status.st_mode) || status.st_uid != geteuid() || (status.st_mode & 077) != 0) {
        return NO;
    }
    NSString *canonical = canonical_path(path);
    return canonical != nil && [canonical isEqualToString:path];
}

static NSData *read_private_regular_file(NSString *path, NSUInteger maximum, NSString **error) {
    struct stat status = {0};
    NSString *parent = path.stringByDeletingLastPathComponent;
    if (lstat(path.fileSystemRepresentation, &status) != 0 || !S_ISREG(status.st_mode) ||
        S_ISLNK(status.st_mode) || status.st_uid != geteuid() || (status.st_mode & 077) != 0 ||
        status.st_size < 0 || (uint64_t)status.st_size > maximum || !private_real_directory(parent)) {
        *error = @"file is not an owner-private regular file in an owner-private directory";
        return nil;
    }
    int descriptor = open(path.fileSystemRepresentation, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    if (descriptor < 0) {
        *error = @"could not open owner-private file";
        return nil;
    }
    NSMutableData *data = [NSMutableData dataWithLength:(NSUInteger)status.st_size];
    size_t offset = 0;
    while (offset < data.length) {
        ssize_t count = read(descriptor, (uint8_t *)data.mutableBytes + offset, data.length - offset);
        if (count <= 0) {
            close(descriptor);
            *error = @"could not read complete owner-private file";
            return nil;
        }
        offset += (size_t)count;
    }
    struct stat after = {0};
    BOOL unchanged = fstat(descriptor, &after) == 0 && after.st_dev == status.st_dev &&
        after.st_ino == status.st_ino && after.st_size == status.st_size &&
        after.st_mtimespec.tv_sec == status.st_mtimespec.tv_sec &&
        after.st_mtimespec.tv_nsec == status.st_mtimespec.tv_nsec;
    close(descriptor);
    if (!unchanged) {
        *error = @"owner-private file changed while it was read";
        return nil;
    }
    return data;
}

static NSString *decode_manifest_value(NSString *encoded) {
    NSMutableString *decoded = [NSMutableString string];
    for (NSUInteger index = 0; index < encoded.length; index++) {
        unichar character = [encoded characterAtIndex:index];
        if (character != '%') {
            [decoded appendFormat:@"%C", character];
            continue;
        }
        if (index + 2 >= encoded.length) {
            return nil;
        }
        NSString *hex = [encoded substringWithRange:NSMakeRange(index + 1, 2)];
        unsigned int value = 0;
        NSScanner *scanner = [NSScanner scannerWithString:hex];
        if (![scanner scanHexInt:&value] || !scanner.isAtEnd) {
            return nil;
        }
        switch (value) {
            case 0x25: [decoded appendString:@"%"];; break;
            case 0x09: [decoded appendString:@"\t"];; break;
            case 0x0d: [decoded appendString:@"\r"];; break;
            case 0x0a: [decoded appendString:@"\n"];; break;
            default: return nil;
        }
        index += 2;
    }
    return decoded;
}

static NSDictionary<NSString *, NSString *> *parse_manifest(NSData *data, NSString **error) {
    NSString *text = [[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding];
    if (text == nil || ![text hasSuffix:@"\n"] || [text containsString:@"\r"] ||
        [text rangeOfString:@"\0"].location != NSNotFound) {
        *error = @"identity is not canonical LF-terminated UTF-8";
        return nil;
    }
    NSMutableDictionary<NSString *, NSString *> *records = [NSMutableDictionary dictionary];
    NSArray<NSString *> *lines = [text componentsSeparatedByString:@"\n"];
    NSCharacterSet *keyCharacters = [NSCharacterSet characterSetWithCharactersInString:
        @"abcdefghijklmnopqrstuvwxyz0123456789._-"];
    for (NSUInteger index = 0; index + 1 < lines.count; index++) {
        NSArray<NSString *> *fields = [lines[index] componentsSeparatedByString:@"\t"];
        if (fields.count != 2 || ((NSString *)fields[0]).length == 0 ||
            [fields[0] rangeOfCharacterFromSet:keyCharacters.invertedSet].location != NSNotFound ||
            records[fields[0]] != nil) {
            *error = @"identity contains an invalid or duplicate record";
            return nil;
        }
        NSString *decoded = decode_manifest_value(fields[1]);
        if (decoded == nil) {
            *error = @"identity contains invalid percent encoding";
            return nil;
        }
        records[fields[0]] = decoded;
    }
    return records;
}

static BOOL exact_keys(NSDictionary<NSString *, NSString *> *records, NSArray<NSString *> *keys) {
    if (records.count != keys.count) {
        return NO;
    }
    for (NSString *key in keys) {
        if (records[key] == nil) {
            return NO;
        }
    }
    return YES;
}

static BOOL manifest_has_key_order(NSData *data, NSArray<NSString *> *keys) {
    NSString *text = [[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding];
    if (text == nil || ![text hasSuffix:@"\n"]) return NO;
    NSArray<NSString *> *lines = [text componentsSeparatedByString:@"\n"];
    if (lines.count != keys.count + 1 || ![lines.lastObject isEqualToString:@""]) return NO;
    for (NSUInteger index = 0; index < keys.count; index++) {
        NSRange separator = [lines[index] rangeOfString:@"\t"];
        if (separator.location == NSNotFound ||
            ![[lines[index] substringToIndex:separator.location] isEqualToString:keys[index]]) {
            return NO;
        }
    }
    return YES;
}

static NSString *hex_data(NSData *data) {
    const uint8_t *bytes = data.bytes;
    NSMutableString *result = [NSMutableString stringWithCapacity:data.length * 2];
    for (NSUInteger index = 0; index < data.length; index++) {
        [result appendFormat:@"%02x", bytes[index]];
    }
    return result;
}

static BOOL load_subject_identity(NSString *path, SubjectIdentity *subject) {
    NSString *readError = nil;
    NSData *data = read_private_regular_file(path, kMaximumIdentityBytes, &readError);
    if (data == nil) {
        return report([NSString stringWithFormat:@"identity file is not an owner-private regular file: %@",
            readError]);
    }
    NSString *parseError = nil;
    NSDictionary<NSString *, NSString *> *records = parse_manifest(data, &parseError);
    NSArray<NSString *> *keys = @[
        @"schema", @"run.id", @"launch.nonce", @"package.app.sha256", @"package.app.path",
        @"package.app.bundle.identifier", @"package.app.executable.path", @"process.pid",
        @"process.start.tv-sec", @"process.start.tv-usec", @"process.executable.device",
        @"process.executable.inode", @"process.executable.fsid", @"process.signature.cdhash",
        @"process.signature.identifier", @"process.signature.team-identifier",
        @"process.mount.read-only", @"launch.controller", @"launch.source",
        @"launch.observation.sha256", @"launch.observation.complete"
    ];
    if (records == nil || !exact_keys(records, keys)) {
        NSString *reason = parseError != nil ? parseError : @"unexpected schema fields";
        return report([NSString stringWithFormat:@"frozen subject identity is invalid: %@", reason]);
    }
    uint64_t device = 0;
    uint64_t inode = 0;
    uint64_t startSeconds = 0;
    uint64_t startMicroseconds = 0;
    NSArray<NSString *> *fsidParts = [records[@"process.executable.fsid"] componentsSeparatedByString:@":"];
    int32_t fsid0 = 0;
    int32_t fsid1 = 0;
    NSCharacterSet *runCharacters = [NSCharacterSet characterSetWithCharactersInString:
        @"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789._-"];
    BOOL cdhashValid = (records[@"process.signature.cdhash"].length == 40 ||
        records[@"process.signature.cdhash"].length == 64) &&
        lower_hex(records[@"process.signature.cdhash"], records[@"process.signature.cdhash"].length);
    NSString *expectedExecutablePath = [records[@"package.app.path"]
        stringByAppendingPathComponent:@"Contents/MacOS/SpaceTerm"];
    if (![records[@"schema"] isEqualToString:kSubjectSchema] ||
        !lower_hex(records[@"launch.nonce"], 64) || !lower_hex(records[@"package.app.sha256"], 64) ||
        !lower_hex(records[@"launch.observation.sha256"], 64) ||
        !parse_positive_pid(records[@"process.pid"], &subject->pid) ||
        !parse_uint64(records[@"process.start.tv-sec"], &startSeconds) ||
        !parse_uint64(records[@"process.start.tv-usec"], &startMicroseconds) || startMicroseconds >= 1000000 ||
        !parse_uint64(records[@"process.executable.device"], &device) ||
        !parse_uint64(records[@"process.executable.inode"], &inode) ||
        fsidParts.count != 2 || !parse_int32(fsidParts[0], &fsid0) ||
        !parse_int32(fsidParts[1], &fsid1) ||
        !cdhashValid ||
        ![records[@"package.app.bundle.identifier"] isEqualToString:kExpectedBundleIdentifier] ||
        ![records[@"process.mount.read-only"] isEqualToString:@"true"] ||
        ![records[@"launch.controller"] isEqualToString:@"acceptance-launch-verifier"] ||
        ![records[@"launch.source"] isEqualToString:@"mounted-dmg"] ||
        ![records[@"launch.observation.complete"] isEqualToString:@"true"] ||
        ![records[@"package.app.path"] hasPrefix:@"/"] ||
        ![records[@"package.app.executable.path"] hasPrefix:@"/"] ||
        ![records[@"package.app.path"] hasSuffix:@"/SpaceTerm.app"] ||
        ![records[@"package.app.executable.path"] isEqualToString:expectedExecutablePath] ||
        records[@"run.id"].length == 0 || records[@"run.id"].length > 80 ||
        [records[@"run.id"] rangeOfCharacterFromSet:runCharacters.invertedSet].location != NSNotFound ||
        records[@"process.signature.identifier"].length == 0 ||
        records[@"process.signature.identifier"].length > 256 ||
        records[@"process.signature.team-identifier"].length > 256) {
        return report(@"frozen subject identity fields are invalid");
    }
    subject->startSeconds = startSeconds;
    subject->startMicroseconds = startMicroseconds;
    subject->executableDevice = (dev_t)device;
    subject->executableInode = (ino_t)inode;
    if ((uint64_t)subject->executableDevice != device ||
        (uint64_t)subject->executableInode != inode) {
        return report(@"frozen subject vnode identity overflows native types");
    }
    subject->fsid0 = fsid0;
    subject->fsid1 = fsid1;
    subject->runID = records[@"run.id"];
    subject->launchNonce = records[@"launch.nonce"];
    subject->launchObservationSHA256 = records[@"launch.observation.sha256"];
    subject->appSHA256 = records[@"package.app.sha256"];
    subject->bundlePath = records[@"package.app.path"];
    subject->bundleIdentifier = records[@"package.app.bundle.identifier"];
    subject->executablePath = records[@"package.app.executable.path"];
    subject->cdhash = records[@"process.signature.cdhash"];
    subject->signingIdentifier = records[@"process.signature.identifier"];
    subject->teamIdentifier = records[@"process.signature.team-identifier"];
    return YES;
}

static NSArray<NSString *> *launch_observation_keys(void) {
    return @[
        @"schema", @"observation.source", @"launch.nonce", @"run.id",
        @"package.app.sha256", @"runtime.schema", @"runtime.sample_interval_ms",
        @"runtime.transition_capacity", @"failure.action.schema", @"failure.action.enabled",
        @"process.pid",
        @"process.pidversion", @"process.executable.path", @"process.executable.device",
        @"process.executable.inode", @"process.executable.fsid", @"process.signature.cdhash",
        @"process.signature.identifier", @"process.signature.team_identifier",
        @"terminal_font_selected", @"initial_grid.rows", @"initial_grid.columns",
        @"initial_grid.logical_width", @"initial_grid.logical_height",
        @"initial_grid.backing_pixel_width", @"initial_grid.backing_pixel_height",
        @"observation.complete"
    ];
}

static BOOL launch_observation_matches_subject(NSDictionary<NSString *, NSString *> *records,
    const SubjectIdentity *subject, BOOL expectedFailureActionEnabled) {
    NSString *pid = @(subject->pid).stringValue;
    NSString *device = @((uint64_t)subject->executableDevice).stringValue;
    NSString *inode = @((uint64_t)subject->executableInode).stringValue;
    NSString *fsid = [NSString stringWithFormat:@"%d:%d", subject->fsid0, subject->fsid1];
    pid_t ignoredPID = 0;
    uint64_t pidversion = 0;
    return records != nil && exact_keys(records, launch_observation_keys()) &&
        [records[@"schema"] isEqualToString:kLaunchObservationSchema] &&
        [records[@"observation.source"] isEqualToString:@"production-app"] &&
        [records[@"launch.nonce"] isEqualToString:subject->launchNonce] &&
        [records[@"run.id"] isEqualToString:subject->runID] &&
        [records[@"package.app.sha256"] isEqualToString:subject->appSHA256] &&
        [records[@"runtime.schema"] isEqualToString:@"spaceterm.acceptance.runtime-stream/v1"] &&
        [records[@"runtime.sample_interval_ms"] isEqualToString:@"1000"] &&
        [records[@"runtime.transition_capacity"] isEqualToString:@"64"] &&
        [records[@"failure.action.schema"] isEqualToString:@"spaceterm.acceptance.failure-action/v1"] &&
        [records[@"failure.action.enabled"] isEqualToString:
            expectedFailureActionEnabled ? @"true" : @"false"] &&
        [records[@"process.pid"] isEqualToString:pid] &&
        parse_positive_pid(records[@"process.pid"], &ignoredPID) &&
        parse_uint64(records[@"process.pidversion"], &pidversion) && pidversion > 0 &&
        [records[@"process.executable.path"] isEqualToString:subject->executablePath] &&
        [records[@"process.executable.device"] isEqualToString:device] &&
        [records[@"process.executable.inode"] isEqualToString:inode] &&
        [records[@"process.executable.fsid"] isEqualToString:fsid] &&
        [records[@"process.signature.cdhash"].lowercaseString
            isEqualToString:subject->cdhash.lowercaseString] &&
        [records[@"process.signature.identifier"] isEqualToString:subject->signingIdentifier] &&
        [records[@"process.signature.team_identifier"] isEqualToString:subject->teamIdentifier] &&
        records[@"terminal_font_selected"].length > 0 &&
        [records[@"observation.complete"] isEqualToString:@"true"];
}

static BOOL validate_authenticated_launch_observation(NSString *identityPath,
    const SubjectIdentity *subject, BOOL expectedFailureActionEnabled) {
    NSString *expectedIdentityPath = [identityPath.stringByDeletingLastPathComponent
        stringByAppendingPathComponent:@"ax-subject.tsv"];
    if (![identityPath isEqualToString:expectedIdentityPath]) {
        return report(@"AX subject must use the authenticated controller's fixed identity path");
    }
    NSString *observationPath = [identityPath.stringByDeletingLastPathComponent
        stringByAppendingPathComponent:@"native-observation-live.tsv"];
    NSString *readError = nil;
    NSData *data = read_private_regular_file(observationPath, kMaximumIdentityBytes, &readError);
    if (data == nil || ![lower_sha256(data) isEqualToString:subject->launchObservationSHA256]) {
        return report(@"authenticated provisional launch observation is missing or has the wrong digest");
    }
    NSString *parseError = nil;
    NSDictionary<NSString *, NSString *> *records = parse_manifest(data, &parseError);
    if (!manifest_has_key_order(data, launch_observation_keys()) ||
        !launch_observation_matches_subject(records, subject, expectedFailureActionEnabled)) {
        return report([NSString stringWithFormat:
            @"authenticated provisional launch observation disagrees with AX subject: %@",
            parseError != nil ? parseError : @"bound field mismatch"]);
    }
    return YES;
}

static BOOL validate_bound_live_subject(NSString *identityPath,
    const SubjectIdentity *subject, BOOL expectedFailureActionEnabled) {
    return validate_authenticated_launch_observation(identityPath, subject,
        expectedFailureActionEnabled) &&
        validate_live_subject(subject);
}

static BOOL live_signature(pid_t pid, NSString **cdhash, NSString **identifier, NSString **team) {
    NSDictionary *attributes = @{(__bridge id)kSecGuestAttributePid: @(pid)};
    SecCodeRef code = NULL;
    OSStatus status = SecCodeCopyGuestWithAttributes(NULL, (__bridge CFDictionaryRef)attributes,
        kSecCSDefaultFlags, &code);
    if (status != errSecSuccess || code == NULL) {
        return NO;
    }
    CFErrorRef validityError = NULL;
    status = SecCodeCheckValidityWithErrors(code, kSecCSStrictValidate, NULL, &validityError);
    if (validityError != NULL) {
        CFRelease(validityError);
    }
    if (status != errSecSuccess) {
        CFRelease(code);
        return NO;
    }
    CFDictionaryRef information = NULL;
    status = SecCodeCopySigningInformation(code, kSecCSSigningInformation, &information);
    CFRelease(code);
    if (status != errSecSuccess || information == NULL) {
        return NO;
    }
    NSDictionary *values = CFBridgingRelease(information);
    NSData *unique = values[(__bridge id)kSecCodeInfoUnique];
    NSString *liveIdentifier = values[(__bridge id)kSecCodeInfoIdentifier];
    NSString *liveTeam = values[(__bridge id)kSecCodeInfoTeamIdentifier];
    if (liveTeam == nil) {
        liveTeam = @"";
    }
    if (![unique isKindOfClass:[NSData class]] || ![liveIdentifier isKindOfClass:[NSString class]]) {
        return NO;
    }
    *cdhash = hex_data(unique);
    *identifier = liveIdentifier;
    *team = liveTeam;
    return YES;
}

static BOOL validate_live_subject(const SubjectIdentity *subject) {
    struct proc_bsdinfo process = {0};
    int processBytes = proc_pidinfo(subject->pid, PROC_PIDTBSDINFO, 0, &process, sizeof(process));
    if (processBytes != (int)sizeof(process) || process.pbi_start_tvsec != subject->startSeconds ||
        process.pbi_start_tvusec != subject->startMicroseconds || process.pbi_uid != geteuid()) {
        return report(@"live process identity or start time disagrees with the frozen subject");
    }
    char executableBuffer[PROC_PIDPATHINFO_MAXSIZE];
    int pathBytes = proc_pidpath(subject->pid, executableBuffer, sizeof(executableBuffer));
    if (pathBytes <= 0) {
        return report(@"live process executable path is unavailable");
    }
    NSString *liveExecutable = canonical_path([NSString stringWithUTF8String:executableBuffer]);
    NSString *expectedExecutable = canonical_path(subject->executablePath);
    NSString *expectedBundle = canonical_path(subject->bundlePath);
    NSRunningApplication *application = [NSRunningApplication runningApplicationWithProcessIdentifier:subject->pid];
    NSString *liveBundle = canonical_path(application.bundleURL.path);
    if (liveExecutable == nil || expectedExecutable == nil || expectedBundle == nil || liveBundle == nil ||
        ![liveExecutable isEqualToString:expectedExecutable] || ![liveBundle isEqualToString:expectedBundle] ||
        ![application.bundleIdentifier isEqualToString:subject->bundleIdentifier]) {
        return report(@"live application bundle or executable disagrees with the frozen subject");
    }
    struct statfs bundleFilesystem = {0};
    NSString *bundleDigest = canonical_bundle_tree_sha256(expectedBundle);
    if (statfs(expectedBundle.fileSystemRepresentation, &bundleFilesystem) != 0 ||
        (bundleFilesystem.f_flags & MNT_RDONLY) == 0 || bundleDigest == nil ||
        ![bundleDigest isEqualToString:subject->appSHA256]) {
        return report(@"mounted application tree digest disagrees with the authenticated subject");
    }
    struct stat executable = {0};
    struct statfs filesystem = {0};
    if (lstat(expectedExecutable.fileSystemRepresentation, &executable) != 0 ||
        !S_ISREG(executable.st_mode) || S_ISLNK(executable.st_mode) ||
        executable.st_dev != subject->executableDevice || executable.st_ino != subject->executableInode ||
        statfs(expectedExecutable.fileSystemRepresentation, &filesystem) != 0 ||
        filesystem.f_fsid.val[0] != subject->fsid0 || filesystem.f_fsid.val[1] != subject->fsid1 ||
        (filesystem.f_flags & MNT_RDONLY) == 0) {
        return report(@"live executable vnode/filesystem is not the frozen read-only mounted subject");
    }
    NSString *cdhash = nil;
    NSString *identifier = nil;
    NSString *team = nil;
    if (!live_signature(subject->pid, &cdhash, &identifier, &team) ||
        ![cdhash.lowercaseString isEqualToString:subject->cdhash.lowercaseString] ||
        ![identifier isEqualToString:subject->signingIdentifier] ||
        ![team isEqualToString:subject->teamIdentifier]) {
        return report(@"live code signature disagrees with the frozen subject");
    }
    process = (struct proc_bsdinfo){0};
    processBytes = proc_pidinfo(subject->pid, PROC_PIDTBSDINFO, 0, &process, sizeof(process));
    if (processBytes != (int)sizeof(process) || process.pbi_start_tvsec != subject->startSeconds ||
        process.pbi_start_tvusec != subject->startMicroseconds || process.pbi_uid != geteuid()) {
        return report(@"live process identity changed during subject revalidation");
    }
    return YES;
}

static BOOL ax_copy_attribute(AXUIElementRef element, CFStringRef attribute,
    CFTypeRef expectedType, CFTypeRef *result) {
    CFTypeRef value = NULL;
    AXError error = AXUIElementCopyAttributeValue(element, attribute, &value);
    if (error != kAXErrorSuccess || value == NULL ||
        (expectedType != NULL && CFGetTypeID(value) != CFGetTypeID(expectedType))) {
        if (value != NULL) {
            CFRelease(value);
        }
        return NO;
    }
    *result = value;
    return YES;
}

static BOOL ax_copy_string(AXUIElementRef element, CFStringRef attribute, NSString **result) {
    CFTypeRef value = NULL;
    if (!ax_copy_attribute(element, attribute, CFSTR(""), &value)) {
        return NO;
    }
    *result = CFBridgingRelease(value);
    return YES;
}

static BOOL ax_copy_bool(AXUIElementRef element, CFStringRef attribute, BOOL *result) {
    CFTypeRef value = NULL;
    if (!ax_copy_attribute(element, attribute, kCFBooleanFalse, &value)) {
        return NO;
    }
    *result = CFBooleanGetValue(value);
    CFRelease(value);
    return YES;
}

static BOOL ax_copy_number(AXUIElementRef element, CFStringRef attribute, CFIndex *result) {
    CFTypeRef value = NULL;
    if (!ax_copy_attribute(element, attribute, (__bridge CFTypeRef)@0, &value)) {
        return NO;
    }
    BOOL valid = CFNumberGetValue(value, kCFNumberCFIndexType, result) && *result >= 0;
    CFRelease(value);
    return valid;
}

static BOOL ax_copy_label(AXUIElementRef element, NSString **result) {
    return ax_copy_string(element, kAXDescriptionAttribute, result) ||
        ax_copy_string(element, kAXTitleAttribute, result);
}

static BOOL ax_value_range(CFTypeRef value, CheckedRange *result) {
    if (value == NULL || CFGetTypeID(value) != AXValueGetTypeID() ||
        AXValueGetType((AXValueRef)value) != kAXValueCFRangeType) {
        return NO;
    }
    CFRange range = {0};
    if (!AXValueGetValue((AXValueRef)value, kAXValueCFRangeType, &range)) {
        return NO;
    }
    result->location = range.location;
    result->length = range.length;
    return range_is_valid(*result, LONG_MAX);
}

static BOOL ax_copy_range(AXUIElementRef element, CFStringRef attribute, CheckedRange *result) {
    CFTypeRef value = NULL;
    AXError error = AXUIElementCopyAttributeValue(element, attribute, &value);
    BOOL valid = error == kAXErrorSuccess && ax_value_range(value, result);
    if (value != NULL) {
        CFRelease(value);
    }
    return valid;
}

static BOOL ax_value_rect(CFTypeRef value, CGRect *result) {
    return value != NULL && CFGetTypeID(value) == AXValueGetTypeID() &&
        AXValueGetType((AXValueRef)value) == kAXValueCGRectType &&
        AXValueGetValue((AXValueRef)value, kAXValueCGRectType, result) &&
        isfinite(result->origin.x) && isfinite(result->origin.y) &&
        isfinite(result->size.width) && isfinite(result->size.height) &&
        result->size.width >= 0 && result->size.height >= 0;
}

static BOOL ax_copy_frame(AXUIElementRef element, CGRect *result) {
    CFTypeRef positionValue = NULL;
    CFTypeRef sizeValue = NULL;
    AXError positionError = AXUIElementCopyAttributeValue(element, kAXPositionAttribute, &positionValue);
    AXError sizeError = AXUIElementCopyAttributeValue(element, kAXSizeAttribute, &sizeValue);
    CGPoint position = CGPointZero;
    CGSize size = CGSizeZero;
    BOOL valid = positionError == kAXErrorSuccess && sizeError == kAXErrorSuccess &&
        positionValue != NULL && sizeValue != NULL &&
        CFGetTypeID(positionValue) == AXValueGetTypeID() &&
        CFGetTypeID(sizeValue) == AXValueGetTypeID() &&
        AXValueGetType((AXValueRef)positionValue) == kAXValueCGPointType &&
        AXValueGetType((AXValueRef)sizeValue) == kAXValueCGSizeType &&
        AXValueGetValue((AXValueRef)positionValue, kAXValueCGPointType, &position) &&
        AXValueGetValue((AXValueRef)sizeValue, kAXValueCGSizeType, &size) &&
        isfinite(position.x) && isfinite(position.y) && isfinite(size.width) &&
        isfinite(size.height) && size.width > 0 && size.height > 0;
    if (positionValue != NULL) CFRelease(positionValue);
    if (sizeValue != NULL) CFRelease(sizeValue);
    if (valid) {
        *result = CGRectMake(position.x, position.y, size.width, size.height);
    }
    return valid;
}

static BOOL array_contains_element(NSArray *elements, AXUIElementRef candidate) {
    for (id value in elements) {
        if (CFEqual((__bridge CFTypeRef)value, candidate)) {
            return YES;
        }
    }
    return NO;
}

static BOOL pid_identity_matches(pid_t expectedPID, pid_t observedPID, AXError error) {
    return error == kAXErrorSuccess && expectedPID > 0 && observedPID == expectedPID;
}

static BOOL element_has_pid(AXUIElementRef element, pid_t expectedPID) {
    pid_t observedPID = 0;
    AXError error = AXUIElementGetPid(element, &observedPID);
    return pid_identity_matches(expectedPID, observedPID, error);
}

static BOOL collect_panes_recursive(AXUIElementRef element, pid_t expectedPID, NSUInteger depth,
    NSMutableArray *visited, NSMutableArray *panes, BOOL *bounded) {
    if (depth > kMaximumAXDepth || visited.count >= kMaximumAXNodes) {
        *bounded = NO;
        return NO;
    }
    if (!element_has_pid(element, expectedPID)) {
        return NO;
    }
    if (array_contains_element(visited, element)) {
        return YES;
    }
    [visited addObject:(__bridge id)element];
    NSString *role = nil;
    if (ax_copy_string(element, kAXRoleAttribute, &role) && [role isEqualToString:(__bridge NSString *)kAXTextAreaRole]) {
        NSString *label = nil;
        (void)ax_copy_label(element, &label);
        if ([label isEqualToString:kExpectedPaneLabel]) {
            [panes addObject:(__bridge id)element];
        }
        return YES;
    }
    CFTypeRef childrenValue = NULL;
    AXError navigationError = AXUIElementCopyAttributeValue(
        element, CFSTR("AXChildrenInNavigationOrder"), &childrenValue);
    if (navigationError != kAXErrorSuccess || childrenValue == NULL ||
        CFGetTypeID(childrenValue) != CFArrayGetTypeID()) {
        if (childrenValue != NULL) {
            CFRelease(childrenValue);
        }
        childrenValue = NULL;
        AXError childrenError = AXUIElementCopyAttributeValue(element, kAXChildrenAttribute, &childrenValue);
        if (childrenError == kAXErrorNoValue || childrenError == kAXErrorAttributeUnsupported) {
            return YES;
        }
        if (childrenError != kAXErrorSuccess || childrenValue == NULL ||
            CFGetTypeID(childrenValue) != CFArrayGetTypeID()) {
            if (childrenValue != NULL) {
                CFRelease(childrenValue);
            }
            return NO;
        }
    }
    NSArray *children = CFBridgingRelease(childrenValue);
    for (id child in children) {
        if (CFGetTypeID((__bridge CFTypeRef)child) != AXUIElementGetTypeID() ||
            !collect_panes_recursive((__bridge AXUIElementRef)child, expectedPID, depth + 1,
                visited, panes, bounded)) {
            return NO;
        }
    }
    return YES;
}

static NSArray *find_target_panes(pid_t pid) {
    AXUIElementRef application = AXUIElementCreateApplication(pid);
    if (application == NULL) {
        return nil;
    }
    NSMutableArray *visited = [NSMutableArray array];
    NSMutableArray *panes = [NSMutableArray array];
    BOOL bounded = YES;
    BOOL success = collect_panes_recursive(application, pid, 0, visited, panes, &bounded);
    CFRelease(application);
    return success && bounded ? panes : nil;
}

static BOOL copy_fixture_value(AXUIElementRef pane, NSString **value) {
    // This is the only whole AXValue read in the program. Its caller is reachable only after
    // explicit fixture-sentinel opt-in and exact private fixture digest validation.
    return ax_copy_string(pane, kAXValueAttribute, value);
}

static BOOL pane_snapshot(AXUIElementRef pane, pid_t expectedPID, PrivacyMode privacy,
    NSString *expectedFixture, PaneSnapshot *snapshot) {
    NSString *role = nil;
    NSString *label = nil;
    if (!element_has_pid(pane, expectedPID) ||
        !ax_copy_string(pane, kAXRoleAttribute, &role) ||
        ![role isEqualToString:(__bridge NSString *)kAXTextAreaRole] ||
        !ax_copy_label(pane, &label) ||
        ![label isEqualToString:kExpectedPaneLabel] ||
        !ax_copy_bool(pane, kAXFocusedAttribute, &snapshot->focused) ||
        !ax_copy_number(pane, kAXNumberOfCharactersAttribute, &snapshot->characterCount) ||
        !ax_copy_range(pane, kAXVisibleCharacterRangeAttribute, &snapshot->visibleRange) ||
        !ax_copy_range(pane, kAXSelectedTextRangeAttribute, &snapshot->selectedRange) ||
        !ax_copy_frame(pane, &snapshot->frame) ||
        !range_is_valid(snapshot->visibleRange, snapshot->characterCount) ||
        !range_is_valid(snapshot->selectedRange, snapshot->characterCount)) {
        return report(@"target AXTextArea returned an invalid required attribute");
    }
    snapshot->valueQueried = privacy == PrivacyModeFixtureSentinel;
    snapshot->valueMatches = NO;
    snapshot->valueSHA256 = nil;
    snapshot->selectedTextQueried = snapshot->valueQueried;
    snapshot->selectedTextSHA256 = nil;
    if (snapshot->valueQueried) {
        NSString *value = nil;
        if (!copy_fixture_value(pane, &value)) {
            return report(@"fixture-gated AXValue query failed");
        }
        NSData *encoded = [value dataUsingEncoding:NSUTF8StringEncoding];
        snapshot->valueMatches = expectedFixture != nil && [value isEqualToString:expectedFixture];
        if (!snapshot->valueMatches || value.length != (NSUInteger)snapshot->characterCount) {
            return report(@"fixture-gated AXValue did not exactly match the approved sentinel");
        }
        snapshot->valueSHA256 = lower_sha256(encoded);
        NSString *selectedText = nil;
        NSString *expectedSelectedText = nil;
        if (!ax_copy_string(pane, kAXSelectedTextAttribute, &selectedText) ||
            !fixture_substring(expectedFixture, snapshot->selectedRange, &expectedSelectedText) ||
            ![selectedText isEqualToString:expectedSelectedText]) {
            return report(@"fixture-gated AXSelectedText did not match the approved sentinel range");
        }
        snapshot->selectedTextSHA256 = lower_sha256(
            [selectedText dataUsingEncoding:NSUTF8StringEncoding]);
    }
    return element_has_pid(pane, expectedPID)
        ? YES : report(@"target AXTextArea PID changed during its attribute snapshot");
}

static AXValueRef range_value(CheckedRange range) {
    CFRange value = CFRangeMake(range.location, range.length);
    return AXValueCreate(kAXValueCFRangeType, &value);
}

static BOOL parameterized_value(AXUIElementRef pane, CFStringRef attribute, CFTypeRef parameter,
    CFTypeRef *result) {
    CFTypeRef value = NULL;
    AXError error = AXUIElementCopyParameterizedAttributeValue(pane, attribute, parameter, &value);
    if (error != kAXErrorSuccess || value == NULL) {
        if (value != NULL) {
            CFRelease(value);
        }
        return NO;
    }
    *result = value;
    return YES;
}

static BOOL parameterized_range(AXUIElementRef pane, CFStringRef attribute, CFTypeRef parameter,
    CFIndex maximum, CheckedRange *result) {
    CFTypeRef value = NULL;
    BOOL valid = parameterized_value(pane, attribute, parameter, &value) &&
        ax_value_range(value, result) && range_is_valid(*result, maximum);
    if (value != NULL) {
        CFRelease(value);
    }
    return valid;
}

static BOOL parameterized_number(AXUIElementRef pane, CFStringRef attribute, CFTypeRef parameter,
    CFIndex *result) {
    CFTypeRef value = NULL;
    BOOL valid = parameterized_value(pane, attribute, parameter, &value) &&
        CFGetTypeID(value) == CFNumberGetTypeID() &&
        CFNumberGetValue((CFNumberRef)value, kCFNumberCFIndexType, result) && *result >= 0;
    if (value != NULL) {
        CFRelease(value);
    }
    return valid;
}

static BOOL fixture_substring(NSString *fixture, CheckedRange range, NSString **result) {
    if (!range_is_valid(range, (CFIndex)fixture.length)) {
        return NO;
    }
    *result = [fixture substringWithRange:NSMakeRange((NSUInteger)range.location, (NSUInteger)range.length)];
    return YES;
}

static BOOL probe_parameterized_attributes(AXUIElementRef pane, const Options *options,
    pid_t pid, const PaneSnapshot *snapshot, NSString *fixture, NSMutableDictionary *output) {
    if (!element_has_pid(pane, pid)) {
        return report(@"target AXTextArea has the wrong PID before parameterized queries");
    }
    NSNumber *lineParameter = @(options->probeLine);
    NSNumber *indexParameter = @(options->probeIndex);
    CheckedRange rangeForLine = {0};
    CheckedRange rangeForIndex = {0};
    CFIndex lineForIndex = -1;
    AXValueRef rangeParameter = range_value(options->probeRange);
    CFTypeRef stringValue = NULL;
    CFTypeRef boundsValue = NULL;
    if (rangeParameter == NULL ||
        !parameterized_range(pane, kAXRangeForLineParameterizedAttribute,
            (__bridge CFTypeRef)lineParameter, snapshot->characterCount, &rangeForLine) ||
        !parameterized_number(pane, kAXLineForIndexParameterizedAttribute,
            (__bridge CFTypeRef)indexParameter, &lineForIndex) ||
        !parameterized_range(pane, kAXRangeForIndexParameterizedAttribute,
            (__bridge CFTypeRef)indexParameter, snapshot->characterCount, &rangeForIndex) ||
        !parameterized_value(pane, kAXBoundsForRangeParameterizedAttribute, rangeParameter, &boundsValue)) {
        if (rangeParameter != NULL) CFRelease(rangeParameter);
        if (boundsValue != NULL) CFRelease(boundsValue);
        return report(@"target AXTextArea parameterized line/index/range query failed");
    }
    CGRect bounds = CGRectZero;
    BOOL boundsValid = ax_value_rect(boundsValue, &bounds);
    CFRelease(boundsValue);
    if (!boundsValid || bounds.size.width <= 0 || bounds.size.height <= 0) {
        CFRelease(rangeParameter);
        return report(@"target AXTextArea returned invalid range bounds");
    }
    CGPoint point = CGPointMake(CGRectGetMidX(bounds), CGRectGetMidY(bounds));
    AXValueRef pointParameter = AXValueCreate(kAXValueCGPointType, &point);
    CheckedRange rangeForPosition = {0};
    BOOL positionValid = pointParameter != NULL &&
        parameterized_range(pane, kAXRangeForPositionParameterizedAttribute, pointParameter,
            snapshot->characterCount, &rangeForPosition);
    if (pointParameter != NULL) CFRelease(pointParameter);
    BOOL indexInside = rangeForIndex.length > 0 &&
        options->probeIndex >= rangeForIndex.location &&
        options->probeIndex < rangeForIndex.location + rangeForIndex.length;
    BOOL lineConsistent = lineForIndex == options->probeLine && rangeForLine.length > 0 &&
        options->probeIndex >= rangeForLine.location &&
        options->probeIndex < rangeForLine.location + rangeForLine.length;
    BOOL positionInside = rangeForPosition.length > 0 &&
        rangeForPosition.location < options->probeRange.location + options->probeRange.length &&
        options->probeRange.location < rangeForPosition.location + rangeForPosition.length;
    AXUIElementRef application = AXUIElementCreateApplication(pid);
    AXUIElementRef hit = NULL;
    AXError hitError = application == NULL ? kAXErrorFailure
        : AXUIElementCopyElementAtPosition(application, (float)point.x, (float)point.y, &hit);
    BOOL hitMatches = hitError == kAXErrorSuccess && hit != NULL && CFEqual(hit, pane);
    if (hit != NULL) CFRelease(hit);
    if (application != NULL) CFRelease(application);
    if (!positionValid || !indexInside || !lineConsistent || !positionInside || !hitMatches) {
        CFRelease(rangeParameter);
        return report(@"target AXTextArea parameterized or hit-test consistency check failed");
    }
    if (options->privacy == PrivacyModeFixtureSentinel) {
        if (!parameterized_value(pane, kAXStringForRangeParameterizedAttribute, rangeParameter,
                &stringValue) || CFGetTypeID(stringValue) != CFStringGetTypeID()) {
            if (stringValue != NULL) CFRelease(stringValue);
            CFRelease(rangeParameter);
            return report(@"fixture-gated AXStringForRange query failed");
        }
        NSString *actual = CFBridgingRelease(stringValue);
        NSString *expected = nil;
        if (!fixture_substring(fixture, options->probeRange, &expected) ||
            ![actual isEqualToString:expected]) {
            CFRelease(rangeParameter);
            return report(@"AXStringForRange did not match the approved fixture range");
        }
        output[@"parameter.string.sha256"] = lower_sha256(
            [actual dataUsingEncoding:NSUTF8StringEncoding]);
        output[@"parameter.string.utf16-length"] = @(actual.length).stringValue;
        output[@"parameter.string.matches"] = @"true";
    } else {
        output[@"parameter.string.queried"] = @"false";
    }
    CFRelease(rangeParameter);
    output[@"parameter.range-for-line"] = [NSString stringWithFormat:@"%ld:%ld",
        (long)rangeForLine.location, (long)rangeForLine.length];
    output[@"parameter.line-for-index"] = @(lineForIndex).stringValue;
    output[@"parameter.range-for-index"] = [NSString stringWithFormat:@"%ld:%ld",
        (long)rangeForIndex.location, (long)rangeForIndex.length];
    output[@"parameter.range-for-position"] = [NSString stringWithFormat:@"%ld:%ld",
        (long)rangeForPosition.location, (long)rangeForPosition.length];
    output[@"parameter.bounds.x"] = [NSString stringWithFormat:@"%.3f", bounds.origin.x];
    output[@"parameter.bounds.y"] = [NSString stringWithFormat:@"%.3f", bounds.origin.y];
    output[@"parameter.bounds.width"] = [NSString stringWithFormat:@"%.3f", bounds.size.width];
    output[@"parameter.bounds.height"] = [NSString stringWithFormat:@"%.3f", bounds.size.height];
    output[@"parameter.hit-test.matches"] = @"true";
    return element_has_pid(pane, pid)
        ? YES : report(@"target AXTextArea PID changed during parameterized queries");
}

static uint64_t continuous_nanoseconds(void) {
    static mach_timebase_info_data_t timebase = {0};
    if (timebase.denom == 0) {
        (void)mach_timebase_info(&timebase);
    }
    __uint128_t scaled = (__uint128_t)mach_continuous_time() * timebase.numer / timebase.denom;
    return (uint64_t)scaled;
}

static void update_aggregate_at(NotificationAggregate *aggregate, uint64_t now) {
    if (aggregate->count == 0) {
        aggregate->firstContinuousNS = now;
    }
    aggregate->lastContinuousNS = now;
    aggregate->count += 1;
}

static BOOL notification_is_after(uint64_t timestamp, uint64_t baseline) {
    return baseline > 0 && timestamp >= baseline;
}

static BOOL notification_is_in_window(uint64_t timestamp, uint64_t baseline,
    uint64_t deadline) {
    return notification_is_after(timestamp, baseline) &&
        (deadline == 0 || timestamp <= deadline);
}

static BOOL selection_notification_is_causal(uint64_t timestamp, uint64_t baseline,
    uint64_t dispatch) {
    return notification_is_after(timestamp, baseline) &&
        (dispatch == 0 || notification_is_after(timestamp, dispatch));
}

typedef NS_ENUM(NSInteger, FocusNotificationDisposition) {
    FocusNotificationForeign = -1,
    FocusNotificationOther = 0,
    FocusNotificationTarget = 1,
};

static FocusNotificationDisposition focus_notification_disposition(pid_t expectedPID,
    pid_t observedPID, BOOL target) {
    if (expectedPID <= 0 || observedPID != expectedPID) {
        return FocusNotificationForeign;
    }
    return target ? FocusNotificationTarget : FocusNotificationOther;
}

static BOOL update_focus_aggregates(NotificationAggregate *all,
    NotificationAggregate *target, NotificationAggregate *other,
    FocusNotificationDisposition disposition, uint64_t timestamp) {
    if (disposition == FocusNotificationForeign) {
        return NO;
    }
    update_aggregate_at(all, timestamp);
    update_aggregate_at(disposition == FocusNotificationTarget ? target : other, timestamp);
    return YES;
}

static BOOL target_focus_minimum_satisfied(NotificationAggregate target,
    NSUInteger minimum) {
    return target.count >= minimum;
}

static void reset_observer_baseline(ObserverState *state, uint64_t baseline) {
    state.value = (NotificationAggregate){0};
    state.selection = (NotificationAggregate){0};
    state.focus = (NotificationAggregate){0};
    state.focusTarget = (NotificationAggregate){0};
    state.focusOther = (NotificationAggregate){0};
    state.layout = (NotificationAggregate){0};
    state.baselineContinuousNS = baseline;
    state.selectionDispatchContinuousNS = 0;
    state.observationDeadlineContinuousNS = 0;
}

static void observer_callback(AXObserverRef observer, AXUIElementRef element,
    CFStringRef notification, void *reference) {
    (void)observer;
    ObserverState *state = (__bridge ObserverState *)reference;
    pid_t pid = 0;
    if (AXUIElementGetPid(element, &pid) != kAXErrorSuccess || pid != state.expectedPID) {
        state.identityMismatch = YES;
        return;
    }
    uint64_t now = continuous_nanoseconds();
    if (!notification_is_in_window(now, state.baselineContinuousNS,
            state.observationDeadlineContinuousNS)) {
        return;
    }
    AXUIElementRef targetElement = (__bridge AXUIElementRef)state.targetObject;
    AXUIElementRef parentElement = (__bridge AXUIElementRef)state.parentObject;
    BOOL target = targetElement != NULL && CFEqual(element, targetElement);
    BOOL parent = parentElement != NULL && CFEqual(element, parentElement);
    if (CFEqual(notification, kAXValueChangedNotification) && target) {
        NotificationAggregate value = state.value;
        update_aggregate_at(&value, now);
        state.value = value;
    } else if (CFEqual(notification, kAXSelectedTextChangedNotification) && target &&
        selection_notification_is_causal(now, state.baselineContinuousNS,
            state.selectionDispatchContinuousNS)) {
        NotificationAggregate value = state.selection;
        update_aggregate_at(&value, now);
        state.selection = value;
    } else if (CFEqual(notification, kAXFocusedUIElementChangedNotification)) {
        FocusNotificationDisposition disposition = focus_notification_disposition(
            state.expectedPID, pid, target);
        if (disposition == FocusNotificationForeign) {
            state.identityMismatch = YES;
            return;
        }
        NotificationAggregate all = state.focus;
        NotificationAggregate targetFocus = state.focusTarget;
        NotificationAggregate otherFocus = state.focusOther;
        if (!update_focus_aggregates(&all, &targetFocus, &otherFocus, disposition, now)) {
            state.identityMismatch = YES;
            return;
        }
        state.focus = all;
        state.focusTarget = targetFocus;
        state.focusOther = otherFocus;
    } else if (CFEqual(notification, kAXLayoutChangedNotification) && (target || parent)) {
        NotificationAggregate value = state.layout;
        update_aggregate_at(&value, now);
        state.layout = value;
    } else {
        state.identityMismatch = YES;
    }
}

static BOOL add_notification_if_required(AXObserverRef observer, AXUIElementRef element,
    CFStringRef notification, ObserverState *state, BOOL required) {
    if (!required) {
        return YES;
    }
    AXError error = AXObserverAddNotification(observer, element, notification,
        (__bridge void *)state);
    return error == kAXErrorSuccess;
}

static BOOL install_observer(AXUIElementRef pane, pid_t pid, const Options *options,
    AXObserverRef *observerResult, CFRunLoopSourceRef *sourceResult,
    ObserverState **stateResult) {
    CFTypeRef parentValue = NULL;
    if (AXUIElementCopyAttributeValue(pane, kAXParentAttribute, &parentValue) != kAXErrorSuccess ||
        parentValue == NULL || CFGetTypeID(parentValue) != AXUIElementGetTypeID()) {
        if (parentValue != NULL) CFRelease(parentValue);
        return report(@"target AXTextArea has no observer-safe parent");
    }
    ObserverState *state = [ObserverState new];
    state.targetObject = CFBridgingRelease(CFRetain(pane));
    state.parentObject = CFBridgingRelease(parentValue);
    state.applicationObject = CFBridgingRelease(AXUIElementCreateApplication(pid));
    state.expectedPID = pid;
    if (state.applicationObject == nil) {
        return report(@"could not create exact application AX element");
    }
    AXUIElementRef applicationElement = (__bridge AXUIElementRef)state.applicationObject;
    AXUIElementRef parentElement = (__bridge AXUIElementRef)state.parentObject;
    AXObserverRef observer = NULL;
    AXError error = AXObserverCreate(pid, observer_callback, &observer);
    if (error != kAXErrorSuccess || observer == NULL) {
        return report(@"could not create AXObserver for exact target PID");
    }
    BOOL registered =
        add_notification_if_required(observer, pane, kAXValueChangedNotification, state,
            options->minimumValueNotifications > 0) &&
        add_notification_if_required(observer, pane, kAXSelectedTextChangedNotification, state,
            options->minimumSelectionNotifications > 0 && !options->requestedSelection.set) &&
        add_notification_if_required(observer, applicationElement,
            kAXFocusedUIElementChangedNotification, state,
            options->minimumFocusNotifications > 0) &&
        add_notification_if_required(observer, parentElement, kAXLayoutChangedNotification, state,
            options->minimumLayoutNotifications > 0);
    if (!registered) {
        CFRelease(observer);
        return report(@"required target AX notification registration failed");
    }
    CFRunLoopSourceRef source = AXObserverGetRunLoopSource(observer);
    if (source == NULL) {
        CFRelease(observer);
        return report(@"AXObserver has no run-loop source");
    }
    CFRetain(source);
    CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopDefaultMode);
    *observerResult = observer;
    *sourceResult = source;
    *stateResult = state;
    return YES;
}

static void remove_observer(AXObserverRef observer, CFRunLoopSourceRef source,
    ObserverState *state, const Options *options) {
    if (source != NULL) {
        CFRunLoopRemoveSource(CFRunLoopGetCurrent(), source, kCFRunLoopDefaultMode);
    }
    if (observer != NULL && state != nil) {
        AXUIElementRef targetElement = (__bridge AXUIElementRef)state.targetObject;
        AXUIElementRef parentElement = (__bridge AXUIElementRef)state.parentObject;
        AXUIElementRef applicationElement = (__bridge AXUIElementRef)state.applicationObject;
        if (options->minimumValueNotifications > 0) {
            (void)AXObserverRemoveNotification(observer, targetElement, kAXValueChangedNotification);
        }
        if (options->minimumSelectionNotifications > 0) {
            (void)AXObserverRemoveNotification(observer, targetElement,
                kAXSelectedTextChangedNotification);
        }
        if (options->minimumFocusNotifications > 0 && applicationElement != NULL) {
            (void)AXObserverRemoveNotification(observer, applicationElement,
                kAXFocusedUIElementChangedNotification);
        }
        if (options->minimumLayoutNotifications > 0 && parentElement != NULL) {
            (void)AXObserverRemoveNotification(observer, parentElement, kAXLayoutChangedNotification);
        }
    }
    if (source != NULL) CFRelease(source);
    if (observer != NULL) CFRelease(observer);
}

static BOOL same_target_at_order(const SubjectIdentity *subject, NSUInteger expectedCount,
    NSUInteger order, AXUIElementRef original) {
    NSArray *panes = find_target_panes(subject->pid);
    return panes != nil && panes.count == expectedCount && order < panes.count &&
        CFEqual((__bridge CFTypeRef)panes[order], original);
}

static BOOL set_selection(AXUIElementRef pane, pid_t expectedPID, CheckedRange range) {
    if (!element_has_pid(pane, expectedPID)) {
        return NO;
    }
    AXValueRef value = range_value(range);
    if (value == NULL) {
        return NO;
    }
    Boolean settable = false;
    AXError settableError = AXUIElementIsAttributeSettable(pane, kAXSelectedTextRangeAttribute,
        &settable);
    AXError setError = settableError == kAXErrorSuccess && settable
        ? AXUIElementSetAttributeValue(pane, kAXSelectedTextRangeAttribute, value)
        : kAXErrorAttributeUnsupported;
    CFRelease(value);
    return setError == kAXErrorSuccess && element_has_pid(pane, expectedPID);
}

static uint64_t continuous_deadline(uint64_t start, NSUInteger milliseconds) {
    uint64_t duration = (uint64_t)milliseconds * UINT64_C(1000000);
    return start > UINT64_MAX - duration ? UINT64_MAX : start + duration;
}

static void run_loop_until_continuous(uint64_t deadline) {
    for (;;) {
        uint64_t now = continuous_nanoseconds();
        if (now >= deadline) {
            break;
        }
        uint64_t remaining = deadline - now;
        CFTimeInterval slice = (CFTimeInterval)MIN(remaining, UINT64_C(50000000)) /
            1000000000.0;
        (void)CFRunLoopRunInMode(kCFRunLoopDefaultMode, slice, true);
    }
}

static BOOL drain_pending_run_loop_sources(void) {
    for (NSUInteger index = 0; index < kMaximumAXNodes; index++) {
        SInt32 status = CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.000001, true);
        if (status != kCFRunLoopRunHandledSource) {
            return YES;
        }
    }
    return report(@"AX notification barrier did not become quiescent");
}

static BOOL append_record(NSMutableString *output, NSString *key, NSString *value) {
    NSCharacterSet *safeKey = [NSCharacterSet characterSetWithCharactersInString:
        @"abcdefghijklmnopqrstuvwxyz0123456789._-"];
    if (key.length == 0 || [key rangeOfCharacterFromSet:safeKey.invertedSet].location != NSNotFound ||
        [value rangeOfCharacterFromSet:[NSCharacterSet controlCharacterSet]].location != NSNotFound ||
        [value containsString:@"\t"] || [value containsString:@"%"] || value.length > 512) {
        return NO;
    }
    [output appendFormat:@"%@\t%@\n", key, value];
    return YES;
}

static BOOL publish_exclusive(NSString *path, NSData *data) {
    NSString *parent = path.stringByDeletingLastPathComponent;
    if (!private_real_directory(parent)) {
        return report(@"output parent is not an owner-private real directory");
    }
    int descriptor = open(path.fileSystemRepresentation,
        O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC | O_NOFOLLOW, 0600);
    if (descriptor < 0) {
        return report(@"could not exclusively create output");
    }
    const uint8_t *bytes = data.bytes;
    size_t offset = 0;
    BOOL success = YES;
    while (offset < data.length) {
        ssize_t written = write(descriptor, bytes + offset, data.length - offset);
        if (written <= 0) {
            success = NO;
            break;
        }
        offset += (size_t)written;
    }
    success = success && fsync(descriptor) == 0 && close(descriptor) == 0;
    if (!success) {
        unlink(path.fileSystemRepresentation);
        return report(@"could not durably publish output");
    }
    return YES;
}

static BOOL add_snapshot_records(NSMutableDictionary *records, NSString *prefix,
    const PaneSnapshot *snapshot) {
    records[[prefix stringByAppendingString:@".frame.x"]] = [NSString stringWithFormat:@"%.3f", snapshot->frame.origin.x];
    records[[prefix stringByAppendingString:@".frame.y"]] = [NSString stringWithFormat:@"%.3f", snapshot->frame.origin.y];
    records[[prefix stringByAppendingString:@".frame.width"]] = [NSString stringWithFormat:@"%.3f", snapshot->frame.size.width];
    records[[prefix stringByAppendingString:@".frame.height"]] = [NSString stringWithFormat:@"%.3f", snapshot->frame.size.height];
    records[[prefix stringByAppendingString:@".focused"]] = snapshot->focused ? @"true" : @"false";
    records[[prefix stringByAppendingString:@".utf16-count"]] = @(snapshot->characterCount).stringValue;
    records[[prefix stringByAppendingString:@".visible-range"]] = [NSString stringWithFormat:@"%ld:%ld",
        (long)snapshot->visibleRange.location, (long)snapshot->visibleRange.length];
    records[[prefix stringByAppendingString:@".selected-range"]] = [NSString stringWithFormat:@"%ld:%ld",
        (long)snapshot->selectedRange.location, (long)snapshot->selectedRange.length];
    records[[prefix stringByAppendingString:@".cursor-empty"]] = snapshot->selectedRange.length == 0 ? @"true" : @"false";
    records[[prefix stringByAppendingString:@".value-queried"]] = snapshot->valueQueried ? @"true" : @"false";
    records[[prefix stringByAppendingString:@".selected-text-queried"]] = snapshot->selectedTextQueried ? @"true" : @"false";
    if (snapshot->valueQueried) {
        records[[prefix stringByAppendingString:@".value-sha256"]] = snapshot->valueSHA256;
        records[[prefix stringByAppendingString:@".value-matches"]] = snapshot->valueMatches ? @"true" : @"false";
        records[[prefix stringByAppendingString:@".selected-text-sha256"]] = snapshot->selectedTextSHA256;
    }
    return YES;
}

static void add_notification_records(NSMutableDictionary *records, NSString *kind,
    NotificationAggregate aggregate) {
    NSString *prefix = [@"notifications." stringByAppendingString:kind];
    records[[prefix stringByAppendingString:@".count"]] = @(aggregate.count).stringValue;
    records[[prefix stringByAppendingString:@".first-continuous-ns"]] = @(aggregate.firstContinuousNS).stringValue;
    records[[prefix stringByAppendingString:@".last-continuous-ns"]] = @(aggregate.lastContinuousNS).stringValue;
}

static BOOL parse_options(int argc, const char *argv[], Options *options) {
    options->privacy = PrivacyModeMetadataOnly;
    options->probeCoordinatesSet = 0;
    for (int index = 1; index < argc; index++) {
        NSString *argument = [NSString stringWithUTF8String:argv[index]];
        if ([argument isEqualToString:@"--help"] || [argument isEqualToString:@"-h"]) {
            usage(stdout);
            exit(0);
        }
        if (index + 1 >= argc) {
            return report([NSString stringWithFormat:@"missing value for %@", argument]);
        }
        NSString *value = [NSString stringWithUTF8String:argv[++index]];
        if ([argument isEqualToString:@"--run-dir"]) options->runDirectory = value;
        else if ([argument isEqualToString:@"--identity"]) options->identityPath = value;
        else if ([argument isEqualToString:@"--output"]) options->outputPath = value;
        else if ([argument isEqualToString:@"--expected-run-id"]) options->expectedRunID = value;
        else if ([argument isEqualToString:@"--expected-failure-action-enabled"]) {
            if ([value isEqualToString:@"true"]) options->expectedFailureActionEnabled = YES;
            else if ([value isEqualToString:@"false"]) options->expectedFailureActionEnabled = NO;
            else return report(@"expected failure-action enabled must be true or false");
            options->expectedFailureActionEnabledSet = YES;
        }
        else if ([argument isEqualToString:@"--fixture-file"]) options->fixturePath = value;
        else if ([argument isEqualToString:@"--fixture-sha256"]) options->fixtureSHA256 = value;
        else if ([argument isEqualToString:@"--fixture-after-file"]) options->fixtureAfterPath = value;
        else if ([argument isEqualToString:@"--fixture-after-sha256"]) options->fixtureAfterSHA256 = value;
        else if ([argument isEqualToString:@"--privacy"]) {
            if ([value isEqualToString:@"metadata-only"]) options->privacy = PrivacyModeMetadataOnly;
            else if ([value isEqualToString:@"fixture-sentinel"]) options->privacy = PrivacyModeFixtureSentinel;
            else return report(@"privacy must be metadata-only or fixture-sentinel");
        } else if ([argument isEqualToString:@"--expected-pane-count"]) {
            uint64_t parsed = 0;
            if (!parse_uint64(value, &parsed) || parsed == 0 || parsed > 128) return report(@"invalid expected pane count");
            options->expectedPaneCount = (NSUInteger)parsed;
        } else if ([argument isEqualToString:@"--pane-order"]) {
            uint64_t parsed = 0;
            if (!parse_uint64(value, &parsed) || parsed > 127) return report(@"invalid pane order");
            options->paneOrder = (NSUInteger)parsed;
        } else if ([argument isEqualToString:@"--expected-before-selected"]) {
            options->expectedBeforeSelection.set = parse_range(value, &options->expectedBeforeSelection.value);
            if (!options->expectedBeforeSelection.set) return report(@"invalid expected-before range");
        } else if ([argument isEqualToString:@"--set-selected"]) {
            options->requestedSelection.set = parse_range(value, &options->requestedSelection.value);
            if (!options->requestedSelection.set) return report(@"invalid set-selected range");
        } else if ([argument isEqualToString:@"--expected-after-selected"]) {
            options->expectedAfterSelection.set = parse_range(value, &options->expectedAfterSelection.value);
            if (!options->expectedAfterSelection.set) return report(@"invalid expected-after range");
        } else if ([argument isEqualToString:@"--probe-line"]) {
            if (!parse_index(value, &options->probeLine)) return report(@"invalid probe line");
            options->probeCoordinatesSet |= 1;
        } else if ([argument isEqualToString:@"--probe-index"]) {
            if (!parse_index(value, &options->probeIndex)) return report(@"invalid probe index");
            options->probeCoordinatesSet |= 2;
        } else if ([argument isEqualToString:@"--probe-range"]) {
            if (!parse_range(value, &options->probeRange)) return report(@"invalid probe range");
            options->probeCoordinatesSet |= 4;
        } else if ([argument isEqualToString:@"--observe-ms"] ||
            [argument isEqualToString:@"--expect-value"] ||
            [argument isEqualToString:@"--expect-selection"] ||
            [argument isEqualToString:@"--expect-focus"] ||
            [argument isEqualToString:@"--expect-layout"]) {
            uint64_t parsed = 0;
            if (!parse_uint64(value, &parsed) || parsed > 3600000) return report(@"invalid bounded count/duration");
            if ([argument isEqualToString:@"--observe-ms"]) options->observeMilliseconds = (NSUInteger)parsed;
            else if ([argument isEqualToString:@"--expect-value"]) options->minimumValueNotifications = (NSUInteger)parsed;
            else if ([argument isEqualToString:@"--expect-selection"]) options->minimumSelectionNotifications = (NSUInteger)parsed;
            else if ([argument isEqualToString:@"--expect-focus"]) options->minimumFocusNotifications = (NSUInteger)parsed;
            else options->minimumLayoutNotifications = (NSUInteger)parsed;
        } else {
            return report([NSString stringWithFormat:@"unknown argument: %@", argument]);
        }
    }
    if (options->runDirectory == nil || options->identityPath == nil || options->outputPath == nil ||
        options->expectedRunID == nil || options->expectedRunID.length == 0 ||
        options->expectedRunID.length > 80 ||
        !options->expectedFailureActionEnabledSet ||
        options->expectedPaneCount == 0 || options->paneOrder >= options->expectedPaneCount ||
        options->probeCoordinatesSet != 7 || options->probeRange.length == 0) {
        return report(@"run directory, identity, output, controller mode, pane count/order, line/index/range are required");
    }
    if (options->requestedSelection.set != options->expectedAfterSelection.set) {
        return report(@"set-selected and expected-after-selected must be supplied together");
    }
    if (options->requestedSelection.set &&
        (!options->expectedBeforeSelection.set || options->observeMilliseconds == 0 ||
            options->minimumSelectionNotifications == 0 ||
            options->privacy != PrivacyModeFixtureSentinel)) {
        return report(@"selection mutation requires fixture-sentinel privacy, expected ranges, observation, and notification");
    }
    if (options->privacy == PrivacyModeFixtureSentinel &&
        (options->fixturePath == nil || !lower_hex(options->fixtureSHA256, 64))) {
        return report(@"fixture-sentinel privacy requires an exact fixture file and SHA-256 opt-in");
    }
    if ((options->fixtureAfterPath == nil) != (options->fixtureAfterSHA256 == nil) ||
        (options->fixtureAfterSHA256 != nil && !lower_hex(options->fixtureAfterSHA256, 64))) {
        return report(@"fixture-after file and SHA-256 must be supplied together");
    }
    return YES;
}

static BOOL path_is_below(NSString *path, NSString *root) {
    NSString *canonicalParent = canonical_path(path.stringByDeletingLastPathComponent);
    if (canonicalParent == nil) return NO;
    NSString *prefix = [root stringByAppendingString:@"/"];
    return [canonicalParent isEqualToString:root] || [canonicalParent hasPrefix:prefix];
}

static BOOL load_fixture(NSString *path, NSString *expectedSHA256, NSString **fixture) {
    NSString *error = nil;
    NSData *data = read_private_regular_file(path, kMaximumFixtureBytes, &error);
    if (data == nil || ![lower_sha256(data) isEqualToString:expectedSHA256]) {
        return report(@"fixture sentinel is missing, unsafe, changed, or has the wrong explicit SHA-256");
    }
    NSString *text = [[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding];
    if (text == nil || [text rangeOfString:@"\0"].location != NSNotFound) {
        return report(@"fixture sentinel is not valid content-safe UTF-8");
    }
    *fixture = text;
    return YES;
}

static BOOL range_equal(CheckedRange left, CheckedRange right) {
    return left.location == right.location && left.length == right.length;
}

static BOOL same_generation_snapshot(const PaneSnapshot *left, const PaneSnapshot *right) {
    return left->characterCount == right->characterCount &&
        range_equal(left->visibleRange, right->visibleRange) &&
        range_equal(left->selectedRange, right->selectedRange) &&
        CGRectEqualToRect(left->frame, right->frame) && left->focused == right->focused &&
        left->valueQueried == right->valueQueried &&
        (!left->valueQueried || [left->valueSHA256 isEqualToString:right->valueSHA256]);
}

static int run_probe(const Options *options) {
    NSString *runRoot = canonical_path(options->runDirectory);
    NSString *probeExecutable = canonical_path(NSProcessInfo.processInfo.arguments.firstObject);
    NSString *expectedProbeExecutable = [runRoot stringByAppendingPathComponent:@"identity/native-ax-probe"];
    NSString *expectedIdentityPath = [runRoot stringByAppendingPathComponent:@"identity/ax-subject.tsv"];
    if (runRoot == nil || ![runRoot isEqualToString:options->runDirectory] ||
        probeExecutable == nil || ![probeExecutable isEqualToString:expectedProbeExecutable] ||
        ![options->identityPath isEqualToString:expectedIdentityPath] ||
        !private_real_directory(runRoot) || !path_is_below(options->identityPath, runRoot) ||
        !path_is_below(options->outputPath, runRoot) ||
        (options->fixturePath != nil && !path_is_below(options->fixturePath, runRoot)) ||
        (options->fixtureAfterPath != nil && !path_is_below(options->fixtureAfterPath, runRoot)) ||
        [[NSFileManager defaultManager] fileExistsAtPath:options->outputPath]) {
        report(@"run, identity, or new output path is not safely run-owned");
        return 1;
    }
    NSString *probeReadError = nil;
    NSData *probeData = read_private_regular_file(probeExecutable, kMaximumProbeBytes, &probeReadError);
    if (probeData == nil) {
        report(@"run-owned probe binary could not be frozen");
        return 1;
    }
    SubjectIdentity subject = {0};
    if (!load_subject_identity(options->identityPath, &subject) ||
        ![subject.runID isEqualToString:options->expectedRunID] ||
        !validate_bound_live_subject(options->identityPath, &subject,
            options->expectedFailureActionEnabled)) {
        if (subject.runID != nil && ![subject.runID isEqualToString:options->expectedRunID]) {
            report(@"authenticated subject run ID disagrees with the expected campaign");
        }
        return 1;
    }
    if (!AXIsProcessTrusted()) {
        report(@"Accessibility permission is required; the probe never prompts for it");
        return 1;
    }
    NSString *fixtureBefore = nil;
    NSString *fixtureAfter = nil;
    if (options->privacy == PrivacyModeFixtureSentinel) {
        if (!load_fixture(options->fixturePath, options->fixtureSHA256, &fixtureBefore)) return 1;
        if (options->fixtureAfterPath != nil) {
            if (!load_fixture(options->fixtureAfterPath, options->fixtureAfterSHA256, &fixtureAfter)) return 1;
        } else {
            fixtureAfter = fixtureBefore;
        }
    }
    NSArray *panes = find_target_panes(subject.pid);
    if (panes == nil || panes.count != options->expectedPaneCount || options->paneOrder >= panes.count) {
        report(@"target AXTextArea hierarchy is missing, ambiguous, or unexpectedly ordered");
        return 1;
    }
    AXUIElementRef pane = (__bridge AXUIElementRef)panes[options->paneOrder];
    CFRetain(pane);
    PaneSnapshot before = {0};
    PaneSnapshot after = {0};
    NSMutableDictionary<NSString *, NSString *> *records = [NSMutableDictionary dictionary];
    if (!pane_snapshot(pane, subject.pid, options->privacy, fixtureBefore, &before) ||
        !range_is_valid(options->probeRange, before.characterCount) ||
        options->probeIndex >= before.characterCount ||
        (options->expectedBeforeSelection.set &&
            !range_equal(before.selectedRange, options->expectedBeforeSelection.value)) ||
        !probe_parameterized_attributes(pane, options, subject.pid, &before, fixtureBefore, records)) {
        CFRelease(pane);
        return 1;
    }
    AXObserverRef observer = NULL;
    CFRunLoopSourceRef observerSource = NULL;
    ObserverState *observerState = nil;
    if (!install_observer(pane, subject.pid, options, &observer, &observerSource, &observerState)) {
        CFRelease(pane);
        return 1;
    }
    uint64_t observerBaselineContinuousNS = 0;
    uint64_t selectionSubscriptionContinuousNS = 0;
    uint64_t selectionDispatchContinuousNS = 0;
    uint64_t observationDeadlineContinuousNS = 0;
    run_loop_until_continuous(continuous_deadline(continuous_nanoseconds(), 50));
    BOOL success = drain_pending_run_loop_sources() && !observerState.identityMismatch &&
        validate_bound_live_subject(options->identityPath, &subject,
            options->expectedFailureActionEnabled) &&
        same_target_at_order(&subject, options->expectedPaneCount, options->paneOrder, pane);
    if (success && options->requestedSelection.set) {
        PaneSnapshot mutationGuard = {0};
        success = pane_snapshot(pane, subject.pid, options->privacy, fixtureBefore, &mutationGuard) &&
            same_generation_snapshot(&before, &mutationGuard) &&
            validate_bound_live_subject(options->identityPath, &subject,
                options->expectedFailureActionEnabled) &&
            same_target_at_order(&subject, options->expectedPaneCount, options->paneOrder, pane) &&
            range_is_valid(options->requestedSelection.value, before.characterCount);
        if (success) {
            // Flush notifications queued before the mutation. Reset only after that barrier, then
            // make the final positive PID check and dispatch without running the loop in between.
            run_loop_until_continuous(continuous_deadline(continuous_nanoseconds(), 50));
            PaneSnapshot dispatchGuard = {0};
            success = drain_pending_run_loop_sources() && !observerState.identityMismatch &&
                pane_snapshot(pane, subject.pid, options->privacy, fixtureBefore,
                &dispatchGuard) && same_generation_snapshot(&before, &dispatchGuard) &&
                validate_bound_live_subject(options->identityPath, &subject,
                    options->expectedFailureActionEnabled) &&
                same_target_at_order(&subject, options->expectedPaneCount,
                    options->paneOrder, pane);
            if (success) {
                success = drain_pending_run_loop_sources() && !observerState.identityMismatch;
            }
            if (success) {
                observerBaselineContinuousNS = continuous_nanoseconds();
                reset_observer_baseline(observerState, observerBaselineContinuousNS);
                success = element_has_pid(pane, subject.pid);
            }
            if (success) {
                success = add_notification_if_required(observer, pane,
                    kAXSelectedTextChangedNotification, observerState, YES);
            }
            if (success) {
                selectionSubscriptionContinuousNS = continuous_nanoseconds();
                selectionDispatchContinuousNS = continuous_nanoseconds();
                observerState.selectionDispatchContinuousNS = selectionDispatchContinuousNS;
                observationDeadlineContinuousNS = continuous_deadline(
                    selectionDispatchContinuousNS, options->observeMilliseconds);
                observerState.observationDeadlineContinuousNS =
                    observationDeadlineContinuousNS;
                success = set_selection(pane, subject.pid, options->requestedSelection.value);
            }
        }
    } else if (success) {
        run_loop_until_continuous(continuous_deadline(continuous_nanoseconds(), 50));
        success = drain_pending_run_loop_sources() && !observerState.identityMismatch;
        observerBaselineContinuousNS = continuous_nanoseconds();
        reset_observer_baseline(observerState, observerBaselineContinuousNS);
        observationDeadlineContinuousNS = continuous_deadline(
            observerBaselineContinuousNS, options->observeMilliseconds);
        observerState.observationDeadlineContinuousNS = observationDeadlineContinuousNS;
    }
    if (success) {
        run_loop_until_continuous(observationDeadlineContinuousNS);
        success = !observerState.identityMismatch &&
            validate_bound_live_subject(options->identityPath, &subject,
                options->expectedFailureActionEnabled) &&
            same_target_at_order(&subject, options->expectedPaneCount, options->paneOrder, pane) &&
            pane_snapshot(pane, subject.pid, options->privacy, fixtureAfter, &after);
    }
    if (success && options->expectedAfterSelection.set) {
        success = range_equal(after.selectedRange, options->expectedAfterSelection.value);
    }
    success = success && observerState.value.count >= options->minimumValueNotifications &&
        observerState.selection.count >= options->minimumSelectionNotifications &&
        target_focus_minimum_satisfied(observerState.focusTarget,
            options->minimumFocusNotifications) &&
        observerState.layout.count >= options->minimumLayoutNotifications;
    remove_observer(observer, observerSource, observerState, options);
    if (!success) {
        CFRelease(pane);
        report(@"stale target, unexpected Selection, or required Pane-scoped notification check failed");
        return 1;
    }
    records[@"schema"] = kResultSchema;
    records[@"probe.binary.sha256"] = lower_sha256(probeData);
    records[@"run.id"] = subject.runID;
    records[@"subject.package.app.sha256"] = subject.appSHA256;
    records[@"subject.launch.nonce.sha256"] = lower_sha256(
        [subject.launchNonce dataUsingEncoding:NSUTF8StringEncoding]);
    records[@"subject.launch.observation.sha256"] = subject.launchObservationSHA256;
    records[@"subject.failure-action.enabled"] = options->expectedFailureActionEnabled
        ? @"true" : @"false";
    records[@"subject.process.pid"] = @(subject.pid).stringValue;
    records[@"subject.process.start-sec"] = @(subject.startSeconds).stringValue;
    records[@"subject.executable.device"] = @((uint64_t)subject.executableDevice).stringValue;
    records[@"subject.executable.inode"] = @((uint64_t)subject.executableInode).stringValue;
    records[@"subject.signature.cdhash"] = subject.cdhash.lowercaseString;
    records[@"subject.signature.identifier.sha256"] = lower_sha256(
        [subject.signingIdentifier dataUsingEncoding:NSUTF8StringEncoding]);
    records[@"subject.revalidated.before-query"] = @"true";
    records[@"subject.revalidated.before-mutation"] = options->requestedSelection.set
        ? @"true" : @"not-applicable";
    records[@"subject.revalidated.after-observation"] = @"true";
    records[@"privacy.mode"] = options->privacy == PrivacyModeFixtureSentinel
        ? @"fixture-sentinel" : @"metadata-only";
    records[@"privacy.axvalue-content-emitted"] = @"false";
    records[@"privacy.fixture-sha256"] = options->privacy == PrivacyModeFixtureSentinel
        ? options->fixtureSHA256 : @"none";
    records[@"pane.role"] = @"AXTextArea";
    records[@"pane.label.sha256"] = lower_sha256(
        [kExpectedPaneLabel dataUsingEncoding:NSUTF8StringEncoding]);
    records[@"pane.label.matches"] = @"true";
    records[@"pane.count"] = @(options->expectedPaneCount).stringValue;
    records[@"pane.navigation-order"] = @(options->paneOrder).stringValue;
    add_snapshot_records(records, @"before", &before);
    add_snapshot_records(records, @"after", &after);
    records[@"selection.requested"] = options->requestedSelection.set ? @"true" : @"false";
    records[@"selection.generation-guard"] = options->requestedSelection.set
        ? @"pass" : @"not-applicable";
    records[@"notifications.baseline-continuous-ns"] =
        @(observerBaselineContinuousNS).stringValue;
    records[@"notifications.selection.dispatch-continuous-ns"] =
        @(selectionDispatchContinuousNS).stringValue;
    records[@"notifications.selection.subscription-continuous-ns"] =
        @(selectionSubscriptionContinuousNS).stringValue;
    records[@"notifications.observation-deadline-continuous-ns"] =
        @(observationDeadlineContinuousNS).stringValue;
    records[@"notifications.clock"] = @"mach-continuous";
    if (options->requestedSelection.set) {
        records[@"selection.requested-range"] = [NSString stringWithFormat:@"%ld:%ld",
            (long)options->requestedSelection.value.location, (long)options->requestedSelection.value.length];
        records[@"selection.notification-causality"] =
            @"post-guard-subscription-dispatch";
    }
    add_notification_records(records, @"value", observerState.value);
    add_notification_records(records, @"selection", observerState.selection);
    add_notification_records(records, @"focus", observerState.focus);
    add_notification_records(records, @"focus-target", observerState.focusTarget);
    add_notification_records(records, @"focus-other", observerState.focusOther);
    add_notification_records(records, @"layout", observerState.layout);
    records[@"notifications.target-identity"] = @"pane-parent-and-same-pid-focus";
    records[@"observation.complete"] = @"true";
    NSMutableString *output = [NSMutableString string];
    NSArray<NSString *> *sortedKeys = [records.allKeys sortedArrayUsingSelector:@selector(compare:)];
    for (NSString *key in sortedKeys) {
        if (!append_record(output, key, records[key])) {
            CFRelease(pane);
            report(@"privacy-safe output record construction failed");
            return 1;
        }
    }
    CFRelease(pane);
    NSData *outputData = [output dataUsingEncoding:NSUTF8StringEncoding];
    if (outputData == nil || !publish_exclusive(options->outputPath, outputData)) {
        return 1;
    }
    return 0;
}

static BOOL fake_subject_matches(const SubjectIdentity *expected, const SubjectIdentity *live) {
    return expected->pid == live->pid && expected->startSeconds == live->startSeconds &&
        expected->startMicroseconds == live->startMicroseconds &&
        expected->executableDevice == live->executableDevice &&
        expected->executableInode == live->executableInode && expected->fsid0 == live->fsid0 &&
        expected->fsid1 == live->fsid1 && [expected->bundlePath isEqualToString:live->bundlePath] &&
        [expected->runID isEqualToString:live->runID] &&
        [expected->launchNonce isEqualToString:live->launchNonce] &&
        [expected->launchObservationSHA256 isEqualToString:live->launchObservationSHA256] &&
        [expected->appSHA256 isEqualToString:live->appSHA256] &&
        [expected->bundleIdentifier isEqualToString:live->bundleIdentifier] &&
        [expected->executablePath isEqualToString:live->executablePath] &&
        [expected->cdhash isEqualToString:live->cdhash] &&
        [expected->signingIdentifier isEqualToString:live->signingIdentifier] &&
        [expected->teamIdentifier isEqualToString:live->teamIdentifier];
}

static BOOL fake_target_matches(NSUInteger expectedCount, NSUInteger expectedOrder,
    NSUInteger liveCount, NSUInteger liveOrder, BOOL sameElement) {
    return expectedCount > 0 && expectedCount == liveCount && expectedOrder == liveOrder &&
        expectedOrder < expectedCount && sameElement;
}

static NSDictionary<NSString *, NSString *> *fake_launch_observation(
    const SubjectIdentity *subject) {
    return @{
        @"schema": kLaunchObservationSchema,
        @"observation.source": @"production-app",
        @"launch.nonce": subject->launchNonce,
        @"run.id": subject->runID,
        @"package.app.sha256": subject->appSHA256,
        @"runtime.schema": @"spaceterm.acceptance.runtime-stream/v1",
        @"runtime.sample_interval_ms": @"1000",
        @"runtime.transition_capacity": @"64",
        @"failure.action.schema": @"spaceterm.acceptance.failure-action/v1",
        @"failure.action.enabled": @"true",
        @"process.pid": @(subject->pid).stringValue,
        @"process.pidversion": @"9",
        @"process.executable.path": subject->executablePath,
        @"process.executable.device": @((uint64_t)subject->executableDevice).stringValue,
        @"process.executable.inode": @((uint64_t)subject->executableInode).stringValue,
        @"process.executable.fsid": [NSString stringWithFormat:@"%d:%d", subject->fsid0,
            subject->fsid1],
        @"process.signature.cdhash": subject->cdhash,
        @"process.signature.identifier": subject->signingIdentifier,
        @"process.signature.team_identifier": subject->teamIdentifier,
        @"terminal_font_selected": @"Fixture Mono",
        @"initial_grid.rows": @"24",
        @"initial_grid.columns": @"80",
        @"initial_grid.logical_width": @"800",
        @"initial_grid.logical_height": @"600",
        @"initial_grid.backing_pixel_width": @"1600",
        @"initial_grid.backing_pixel_height": @"1200",
        @"observation.complete": @"true",
    };
}

static NSData *fake_launch_observation_data(const SubjectIdentity *subject, NSString *enabled) {
    NSString *text = [NSString stringWithFormat:
        @"schema\t%@\nobservation.source\tproduction-app\nlaunch.nonce\t%@\nrun.id\t%@\n"
        @"package.app.sha256\t%@\nruntime.schema\tspaceterm.acceptance.runtime-stream/v1\n"
        @"runtime.sample_interval_ms\t1000\nruntime.transition_capacity\t64\n"
        @"failure.action.schema\tspaceterm.acceptance.failure-action/v1\n"
        @"failure.action.enabled\t%@\nprocess.pid\t%d\nprocess.pidversion\t9\n"
        @"process.executable.path\t%@\nprocess.executable.device\t%llu\n"
        @"process.executable.inode\t%llu\nprocess.executable.fsid\t%d:%d\n"
        @"process.signature.cdhash\t%@\nprocess.signature.identifier\t%@\n"
        @"process.signature.team_identifier\t%@\nterminal_font_selected\tFixture Mono\n"
        @"initial_grid.rows\t24\ninitial_grid.columns\t80\ninitial_grid.logical_width\t800\n"
        @"initial_grid.logical_height\t600\ninitial_grid.backing_pixel_width\t1600\n"
        @"initial_grid.backing_pixel_height\t1200\nobservation.complete\ttrue\n",
        kLaunchObservationSchema, subject->launchNonce, subject->runID, subject->appSHA256,
        enabled, subject->pid, subject->executablePath,
        (unsigned long long)(uint64_t)subject->executableDevice,
        (unsigned long long)(uint64_t)subject->executableInode, subject->fsid0, subject->fsid1,
        subject->cdhash, subject->signingIdentifier, subject->teamIdentifier];
    return [text dataUsingEncoding:NSUTF8StringEncoding];
}

static BOOL self_test(void) {
    CheckedRange range = {0};
    if (!parse_range(@"4:2", &range) || range.location != 4 || range.length != 2 ||
        parse_range(@"-1:2", &range) || parse_range(@"1:18446744073709551615", &range) ||
        range_is_valid((CheckedRange){LONG_MAX, 1}, LONG_MAX) ||
        !lower_hex(@"0123456789abcdef", 16) || lower_hex(@"ABCDEF", 6)) {
        return NO;
    }
    NSString *manifestText =
        @"schema\tspaceterm.acceptance.ax-subject/v1\nvalue\tpercent%25tab%09line%0a\n";
    NSString *error = nil;
    NSDictionary *records = parse_manifest([manifestText dataUsingEncoding:NSUTF8StringEncoding], &error);
    if (records == nil || ![records[@"value"] isEqualToString:@"percent%tab\tline\n"]) {
        return NO;
    }
    NSString *duplicate = @"schema\tone\nschema\ttwo\n";
    if (parse_manifest([duplicate dataUsingEncoding:NSUTF8StringEncoding], &error) != nil) {
        return NO;
    }
    NSMutableString *safe = [NSMutableString string];
    if (!append_record(safe, @"safe.key", @"42") || append_record(safe, @"bad", @"secret\ntext") ||
        append_record(safe, @"bad", @"percent%value")) {
        return NO;
    }
    // Fake-backend adversarial checks: range overflow, stale identity, wrong pane identity,
    // duplicate records, and content-bearing output must all fail before any AX/TCC operation.
    SubjectIdentity expected = {.pid = 10, .startSeconds = 20, .startMicroseconds = 21,
        .executableDevice = 30, .executableInode = 40, .fsid0 = -50, .fsid1 = 51,
        .runID = @"run-fixture",
        .launchNonce = @"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        .launchObservationSHA256 =
            @"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        .appSHA256 = @"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
        .bundlePath = @"/Volumes/Fixture/SpaceTerm.app",
        .bundleIdentifier = @"io.github.sadiksaifi.spaceterm",
        .executablePath = @"/Volumes/Fixture/SpaceTerm.app/Contents/MacOS/SpaceTerm",
        .cdhash = @"0123456789abcdef0123456789abcdef01234567",
        .signingIdentifier = @"io.github.sadiksaifi.spaceterm", .teamIdentifier = @""};
    SubjectIdentity exact = expected;
    SubjectIdentity stale = expected;
    stale.startSeconds += 1;
    SubjectIdentity wrongPID = expected;
    wrongPID.pid += 1;
    SubjectIdentity wrongApp = expected;
    wrongApp.bundlePath = @"/Volumes/Fixture/Other.app";
    SubjectIdentity wrongSignature = expected;
    wrongSignature.cdhash = @"ffffffffffffffffffffffffffffffffffffffff";
    SubjectIdentity wrongRun = expected;
    wrongRun.runID = @"run-other";
    SubjectIdentity wrongNonce = expected;
    wrongNonce.launchNonce =
        @"dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
    SubjectIdentity wrongObservation = expected;
    wrongObservation.launchObservationSHA256 =
        @"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
    SubjectIdentity wrongAppDigest = expected;
    wrongAppDigest.appSHA256 =
        @"ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
    if (!fake_subject_matches(&expected, &exact) || fake_subject_matches(&expected, &stale) ||
        fake_subject_matches(&expected, &wrongPID) || fake_subject_matches(&expected, &wrongApp) ||
        fake_subject_matches(&expected, &wrongSignature) || fake_subject_matches(&expected, &wrongRun) ||
        fake_subject_matches(&expected, &wrongNonce) ||
        fake_subject_matches(&expected, &wrongObservation) ||
        fake_subject_matches(&expected, &wrongAppDigest) ||
        !fake_target_matches(2, 1, 2, 1, YES) || fake_target_matches(2, 1, 1, 0, YES) ||
        fake_target_matches(2, 1, 2, 1, NO) || expected.pid == 0 ||
        range_equal((CheckedRange){0, 1}, (CheckedRange){1, 0})) {
        return NO;
    }
    NSDictionary<NSString *, NSString *> *launch = fake_launch_observation(&expected);
    NSMutableDictionary<NSString *, NSString *> *wrongLaunchRun = [launch mutableCopy];
    wrongLaunchRun[@"run.id"] = @"run-other";
    NSMutableDictionary<NSString *, NSString *> *wrongLaunchNonce = [launch mutableCopy];
    wrongLaunchNonce[@"launch.nonce"] = wrongNonce.launchNonce;
    NSMutableDictionary<NSString *, NSString *> *wrongLaunchApp = [launch mutableCopy];
    wrongLaunchApp[@"package.app.sha256"] = wrongAppDigest.appSHA256;
    NSMutableDictionary<NSString *, NSString *> *wrongLaunchExecutable = [launch mutableCopy];
    wrongLaunchExecutable[@"process.executable.path"] = @"/Volumes/Fixture/Other.app/SpaceTerm";
    NSMutableDictionary<NSString *, NSString *> *missingFailureEnabled = [launch mutableCopy];
    [missingFailureEnabled removeObjectForKey:@"failure.action.enabled"];
    NSMutableDictionary<NSString *, NSString *> *wrongFailureEnabled = [launch mutableCopy];
    wrongFailureEnabled[@"failure.action.enabled"] = @"sometimes";
    NSMutableDictionary<NSString *, NSString *> *disabledFailureAction = [launch mutableCopy];
    disabledFailureAction[@"failure.action.enabled"] = @"false";
    NSData *observationBytes = fake_launch_observation_data(&expected, @"true");
    NSString *observationText = [[NSString alloc] initWithData:observationBytes
        encoding:NSUTF8StringEncoding];
    NSData *duplicateFailureEnabledData = [[observationText
        stringByReplacingOccurrencesOfString:@"failure.action.enabled\ttrue\n"
        withString:@"failure.action.enabled\ttrue\nfailure.action.enabled\tfalse\n"]
        dataUsingEncoding:NSUTF8StringEncoding];
    NSData *missingFailureEnabledData = [[observationText
        stringByReplacingOccurrencesOfString:@"failure.action.enabled\ttrue\n" withString:@""]
        dataUsingEncoding:NSUTF8StringEncoding];
    NSData *wrongFailureEnabledData = [[observationText
        stringByReplacingOccurrencesOfString:@"failure.action.enabled\ttrue\n"
        withString:@"failure.action.enabled\tsometimes\n"]
        dataUsingEncoding:NSUTF8StringEncoding];
    NSData *misorderedFailureEnabledData = [[observationText
        stringByReplacingOccurrencesOfString:
            @"failure.action.schema\tspaceterm.acceptance.failure-action/v1\n"
             @"failure.action.enabled\ttrue\n"
        withString:
            @"failure.action.enabled\ttrue\n"
             @"failure.action.schema\tspaceterm.acceptance.failure-action/v1\n"]
        dataUsingEncoding:NSUTF8StringEncoding];
    NSString *duplicateFailureError = nil;
    NSString *observationFixtureSHA = lower_sha256(observationBytes);
    NSString *observationParseError = nil;
    NSDictionary<NSString *, NSString *> *observationFixtureRecords =
        parse_manifest(observationBytes, &observationParseError);
    NSDictionary<NSString *, NSString *> *missingFailureFixture =
        parse_manifest(missingFailureEnabledData, &observationParseError);
    NSDictionary<NSString *, NSString *> *wrongFailureFixture =
        parse_manifest(wrongFailureEnabledData, &observationParseError);
    NotificationAggregate aggregate = {0};
    update_aggregate_at(&aggregate, 200);
    update_aggregate_at(&aggregate, 250);
    FocusNotificationDisposition focusSequence[] = {
        focus_notification_disposition(10, 10, YES),
        focus_notification_disposition(10, 10, NO),
        focus_notification_disposition(10, 10, YES),
    };
    NotificationAggregate focusAll = {0};
    NotificationAggregate focusTarget = {0};
    NotificationAggregate focusOther = {0};
    if (!update_focus_aggregates(&focusAll, &focusTarget, &focusOther,
            FocusNotificationOther, 300) ||
        target_focus_minimum_satisfied(focusTarget, 1) ||
        !update_focus_aggregates(&focusAll, &focusTarget, &focusOther,
            FocusNotificationTarget, 350) ||
        !target_focus_minimum_satisfied(focusTarget, 1) || focusAll.count != 2 ||
        focusTarget.count != 1 || focusOther.count != 1) {
        return NO;
    }
    if (!launch_observation_matches_subject(launch, &expected, YES) ||
        launch_observation_matches_subject(launch, &expected, NO) ||
        !launch_observation_matches_subject(disabledFailureAction, &expected, NO) ||
        launch_observation_matches_subject(wrongLaunchRun, &expected, YES) ||
        launch_observation_matches_subject(wrongLaunchNonce, &expected, YES) ||
        launch_observation_matches_subject(wrongLaunchApp, &expected, YES) ||
        launch_observation_matches_subject(wrongLaunchExecutable, &expected, YES) ||
        launch_observation_matches_subject(missingFailureEnabled, &expected, YES) ||
        launch_observation_matches_subject(wrongFailureEnabled, &expected, YES) ||
        parse_manifest(duplicateFailureEnabledData, &duplicateFailureError) != nil ||
        observationFixtureRecords.count != 27 ||
        !manifest_has_key_order(observationBytes, launch_observation_keys()) ||
        manifest_has_key_order(misorderedFailureEnabledData, launch_observation_keys()) ||
        !launch_observation_matches_subject(observationFixtureRecords, &expected, YES) ||
        launch_observation_matches_subject(missingFailureFixture, &expected, YES) ||
        launch_observation_matches_subject(wrongFailureFixture, &expected, YES) ||
        observationFixtureSHA == nil || !lower_hex(observationFixtureSHA, 64) ||
        [observationFixtureSHA isEqualToString:lower_sha256(missingFailureEnabledData)] ||
        [observationFixtureSHA isEqualToString:lower_sha256(duplicateFailureEnabledData)] ||
        [observationFixtureSHA isEqualToString:lower_sha256(wrongFailureEnabledData)] ||
        [observationFixtureSHA isEqualToString:expected.launchObservationSHA256] ||
        notification_is_after(99, 100) || !notification_is_after(100, 100) ||
        notification_is_in_window(99, 100, 200) ||
        !notification_is_in_window(200, 100, 200) ||
        notification_is_in_window(201, 100, 200) ||
        selection_notification_is_causal(150, 100, 200) ||
        !selection_notification_is_causal(200, 100, 200) ||
        !selection_notification_is_causal(150, 100, 0) ||
        aggregate.count != 2 || aggregate.firstContinuousNS != 200 ||
        aggregate.lastContinuousNS != 250 ||
        focusSequence[0] != FocusNotificationTarget ||
        focusSequence[1] != FocusNotificationOther ||
        focusSequence[2] != FocusNotificationTarget ||
        focus_notification_disposition(10, 10, NO) != FocusNotificationOther ||
        focus_notification_disposition(10, 10, YES) != FocusNotificationTarget ||
        focus_notification_disposition(10, 11, NO) != FocusNotificationForeign ||
        !pid_identity_matches(10, 10, kAXErrorSuccess) ||
        pid_identity_matches(10, 11, kAXErrorSuccess) ||
        pid_identity_matches(10, 10, kAXErrorInvalidUIElement) ||
        continuous_deadline(100, 2) != UINT64_C(2000100) ||
        continuous_deadline(UINT64_MAX - 1, 2) != UINT64_MAX) {
        return NO;
    }
    return YES;
}

int main(int argc, const char *argv[]) {
    @autoreleasepool {
        if (argc == 2 && strcmp(argv[1], "--self-test") == 0) {
            if (!self_test()) {
                return report(@"self-test failed") ? 0 : 1;
            }
            puts("native AX probe self-test: PASS");
            return 0;
        }
        if (argc == 4 && strcmp(argv[1], "--self-test-bundle") == 0) {
            NSString *bundle = [NSString stringWithUTF8String:argv[2]];
            NSString *expected = [NSString stringWithUTF8String:argv[3]];
            NSString *observed = canonical_bundle_tree_sha256(bundle);
            if (!lower_hex(expected, 64) || observed == nil ||
                ![observed isEqualToString:expected]) {
                return report(@"bundle-tree self-test rejected a missing or mismatched digest") ? 0 : 1;
            }
            puts("native AX probe bundle-tree self-test: PASS");
            return 0;
        }
        Options options = {0};
        if (!parse_options(argc, argv, &options)) {
            usage(stderr);
            return 2;
        }
        return run_probe(&options);
    }
}
