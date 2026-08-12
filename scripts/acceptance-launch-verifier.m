#import <AppKit/AppKit.h>
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
static const NSTimeInterval kProofTimeoutSeconds = 30.0;
static volatile sig_atomic_t interrupted = 0;

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

static bool positive_number(NSString *value) {
    NSScanner *scanner = [NSScanner scannerWithString:value];
    double number = 0;
    return [scanner scanDouble:&number] && scanner.isAtEnd && isfinite(number) && number > 0;
}

static NSData *challenge_data(NSString *nonce, const Options *options) {
    NSString *challenge = [NSString stringWithFormat:
        @"schema\tspaceterm.acceptance.native-launch-challenge/v1\n"
         "launch.nonce\t%@\nrun.id\t%@\npackage.app.sha256\t%@\n",
        nonce, options->runID, options->appSHA256];
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
        @"package.app.sha256", @"process.pid", @"process.executable.path",
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
        ![records[@"schema"] isEqualToString:@"spaceterm.acceptance.native-launch-proof/v2"] ||
        ![records[@"observation.source"] isEqualToString:@"production-app"] ||
        ![records[@"launch.nonce"] isEqualToString:nonce] ||
        ![records[@"run.id"] isEqualToString:options->runID] ||
        ![records[@"package.app.sha256"] isEqualToString:options->appSHA256] ||
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

static bool publish_exclusive(NSString *output, NSData *data) {
    NSString *parent = output.stringByDeletingLastPathComponent;
    struct stat parent_stat = {0};
    if (lstat(parent.fileSystemRepresentation, &parent_stat) != 0 ||
        !S_ISDIR(parent_stat.st_mode) || S_ISLNK(parent_stat.st_mode)) {
        return report(@"output parent is not a real directory");
    }
    NSString *temporary = [parent stringByAppendingPathComponent:
        [NSString stringWithFormat:@".acceptance-observation.%d.tmp", getpid()]];
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
    int directory = open(parent.fileSystemRepresentation, O_RDONLY | O_CLOEXEC | O_DIRECTORY);
    bool directory_synced = directory >= 0 && fsync(directory) == 0;
    int directory_close_result = directory >= 0 ? close(directory) : -1;
    if (!directory_synced || directory_close_result != 0) {
        return report(@"could not durably publish observation directory entry");
    }
    return true;
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

    expected_app = canonical_path(options->app);
    expected_path = canonical_path(options->executable);
    if (expected_app == nil || expected_path == nil ||
        ![expected_path hasPrefix:[expected_app stringByAppendingString:@"/"]]) {
        report(@"application or executable path is invalid");
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
    observation = final_observation(
        response, pidversion, &expected_fs, live_cdhash, live_identifier, live_team);
    close(peer);
    peer = -1;
    if (options->replay) {
        terminate_exact_application(application);
        if (!application.terminated) {
            report(@"replay application could not be terminated safely");
            goto cleanup;
        }
    } else {
        fprintf(stderr, "authenticated mounted app is ready; quit it after acceptance completes\n");
        while (!application.terminated && !interrupted) {
            [NSThread sleepForTimeInterval:0.1];
        }
        if (interrupted) {
            report(@"campaign was interrupted");
            goto cleanup;
        }
    }
    if (!publish_exclusive(options->output,
            [observation dataUsingEncoding:NSUTF8StringEncoding])) {
        goto cleanup;
    }
    result = 0;

cleanup:
    if (result != 0) terminate_exact_application(application);
    if (peer >= 0) close(peer);
    if (listener >= 0) close(listener);
    if (executable_fd >= 0) close(executable_fd);
    if (socket_path != nil) unlink(socket_path.fileSystemRepresentation);
    rmdir(socket_directory);
    return result;
}

int main(int argc, const char *argv[]) {
    @autoreleasepool {
        signal(SIGPIPE, SIG_IGN);
        signal(SIGINT, handle_signal);
        signal(SIGTERM, handle_signal);
        Options options = {0};
        if (!parse_options(argc, argv, &options)) {
            return 64;
        }
        return run_verifier(&options);
    }
}
