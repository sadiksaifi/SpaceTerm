#include "capability_internal.h"
#include "capability_environment.h"
#import <AppKit/AppKit.h>
#import <AVFoundation/AVFoundation.h>
#import <ScreenCaptureKit/ScreenCaptureKit.h>
#import <Security/Security.h>
#include <ApplicationServices/ApplicationServices.h>
#include <Carbon/Carbon.h>
#include <bsm/libbsm.h>
#include <errno.h>
#include <fcntl.h>
#include <libproc.h>
#include <poll.h>
#include <pwd.h>
#include <spawn.h>
#include <signal.h>
#include <sys/time.h>
#include <sys/sysctl.h>
#include <sys/resource.h>
#include <stdlib.h>
#include <string.h>
#include <sys/acl.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <unistd.h>

static st_capability_targets bound_targets;
static int targets_bound;
static int owned_tty = -1;
static pid_t foreground_child = -1;
static pid_t previous_foreground = -1;

static int foreground(int tty, pid_t group) {
    sigset_t blocked, original;
    sigemptyset(&blocked); sigaddset(&blocked, SIGTTOU);
    if (sigprocmask(SIG_BLOCK, &blocked, &original) != 0) return 0;
    int result = tcsetpgrp(tty, group) == 0;
    if (sigprocmask(SIG_SETMASK, &original, NULL) != 0) return 0;
    return result;
}
int st_capability_begin_foreground(pid_t child) {
    if (child <= 0 || owned_tty >= 0) return 0;
    owned_tty = open("/dev/tty", O_RDWR | O_CLOEXEC | O_NOCTTY);
    foreground_child = child;
    previous_foreground = owned_tty >= 0 ? tcgetpgrp(owned_tty) : -1;
    return previous_foreground == getpgrp() && foreground(owned_tty, child);
}
st_native_result st_capability_finish_spawn(pid_t child, int retired) {
    (void)retired;
    if (owned_tty < 0) return ST_PROBE_OK;
    int okay = child == foreground_child && tcgetpgrp(owned_tty) == child &&
        foreground(owned_tty, previous_foreground);
    /* Never replace a newer foreground-group change. */
    close(owned_tty); owned_tty = -1; foreground_child = -1; previous_foreground = -1;
    return okay ? ST_PROBE_OK : ST_PROBE_CLEANUP_UNRESOLVED;
}

static int expired(uint64_t deadline) { return st_capability_monotonic_ns() >= deadline; }
static void pump(uint64_t deadline) {
    if (!expired(deadline)) [[NSRunLoop currentRunLoop] runUntilDate:[NSDate dateWithTimeIntervalSinceNow:0.005]];
}
static void unavailable(st_capability_observation *out, int unrun) {
    out->preflight_class = ST_PREFLIGHT_UNAVAILABLE;
    out->outcome = unrun ? ST_OUTCOME_UNRUN : ST_OUTCOME_UNAVAILABLE;
}
static void verified(st_capability_observation *out) {
    out->preflight_class = ST_PREFLIGHT_ALLOWED;
    out->effect_class = ST_EFFECT_VERIFIED;
    out->outcome = ST_OUTCOME_PASSED;
}

static NSString *run_hex(void) {
    char text[33];
    static const char hex[] = "0123456789abcdef";
    for (size_t i = 0; i < 16; ++i) {
        text[i * 2] = hex[bound_targets.run_id[i] >> 4];
        text[i * 2 + 1] = hex[bound_targets.run_id[i] & 15];
    }
    text[32] = 0;
    return [NSString stringWithUTF8String:text];
}

st_native_result st_capability_bind_targets(const st_capability_targets *targets) {
    if (!targets || targets->schema != 1 || targets->reserved != 0 ||
        targets->companion_pid > INT32_MAX || targets->selected_volume[4095] != 0 ||
        (!targets->companion_pid && targets->companion_window)) return ST_PROBE_INVALID;
    int terminated = 0;
    for (size_t i = 0; i < sizeof(targets->selected_volume); ++i) {
        if (targets->selected_volume[i] == 0) terminated = 1;
        else if (terminated) return ST_PROBE_INVALID;
    }
    int nonzero = 0;
    for (size_t i = 0; i < 16; ++i) nonzero |= targets->run_id[i];
    if (!nonzero) return ST_PROBE_INVALID;
    if (targets->selected_volume[0]) {
        if (targets->selected_volume[0] != '/') return ST_PROBE_INVALID;
        NSString *path = [NSString stringWithUTF8String:targets->selected_volume];
        if (!path) return ST_PROBE_INVALID;
        for (NSString *part in path.pathComponents)
            if ([part isEqualToString:@"."] || [part isEqualToString:@".."] ||
                [part rangeOfCharacterFromSet:[NSCharacterSet controlCharacterSet]].location != NSNotFound)
                return ST_PROBE_INVALID;
    }
    bound_targets = *targets;
    targets_bound = 1;
    return ST_PROBE_OK;
}

/* Account-writable code is never an admitted worker. Root-owned, non-symlink
 * components with no write ACL are a necessary evidence preflight. The protected
 * publisher remains responsible for fresh-copy authentication and historical
 * exclusion; this check is not publication authority. */
static int protected_path(const char *path) {
    char partial[4096];
    size_t length = strlen(path);
    if (length == 0 || length >= sizeof(partial) || path[0] != '/') return 0;
    for (size_t i = 1; i <= length; ++i) {
        if (i != 1 && i != length && path[i] != '/') continue;
        memcpy(partial, path, i);
        partial[i] = 0;
        struct stat info;
        if (lstat(partial, &info) != 0 || info.st_uid != 0 ||
            (info.st_mode & (S_IWGRP | S_IWOTH)) || S_ISLNK(info.st_mode) ||
            (i != length && !S_ISDIR(info.st_mode))) return 0;
        acl_t acl = acl_get_file(partial, ACL_TYPE_EXTENDED);
        if (!acl) return 0;
        acl_entry_t entry;
        int valid_acl = acl_valid(acl) == 0;
        errno = 0;
        int has_entry = acl_get_entry(acl, ACL_FIRST_ENTRY, &entry);
        int entry_error = errno;
        acl_free(acl);
        /* Darwin returns 0 for a present entry, -1/EINVAL at an empty ACL. */
        if (!valid_acl || has_entry != -1 || entry_error != EINVAL) return 0;
    }
    return 1;
}
static SecRequirementRef requirement(NSString *identifier) {
    NSString *text = [NSString stringWithFormat:@"identifier \"%@\" and certificate leaf = H\"%s\"", identifier, ST_CAPABILITY_CERTIFICATE];
    SecRequirementRef result = NULL;
    if (SecRequirementCreateWithString((__bridge CFStringRef)text, kSecCSDefaultFlags, &result) != errSecSuccess) return NULL;
    return result;
}
static int signed_process(pid_t pid, NSString *identifier) {
    struct proc_bsdinfo info;
    if (proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &info, sizeof(info)) != sizeof(info) || info.pbi_uid != getuid()) return 0;
    SecCodeRef code = NULL;
    NSDictionary *attributes = @{(__bridge NSString *)kSecGuestAttributePid: @(pid)};
    SecRequirementRef required = requirement(identifier);
    if (!required) return 0;
    OSStatus status = SecCodeCopyGuestWithAttributes(NULL, (__bridge CFDictionaryRef)attributes, kSecCSDefaultFlags, &code);
    if (status == errSecSuccess) status = SecCodeCheckValidity(code, kSecCSStrictValidate, required);
    if (code) CFRelease(code);
    CFRelease(required);
    return status == errSecSuccess;
}
static int admitted_worker(void) {
    if (!protected_path(ST_CAPABILITY_INSTALLED_PATH)) return 0;
    SecStaticCodeRef code = NULL;
    NSURL *url = [NSURL fileURLWithPath:@ST_CAPABILITY_INSTALLED_PATH];
    SecRequirementRef required = requirement(@ST_CAPABILITY_IDENTIFIER);
    if (!required) return 0;
    OSStatus status = SecStaticCodeCreateWithPath((__bridge CFURLRef)url, kSecCSDefaultFlags, &code);
    if (status == errSecSuccess) status = SecStaticCodeCheckValidity(code,
        kSecCSStrictValidate | kSecCSCheckAllArchitectures, required);
    CFDictionaryRef information = NULL;
    if (status == errSecSuccess) status = SecCodeCopySigningInformation(code, kSecCSSigningInformation, &information);
    if (status == errSecSuccess) {
        NSDictionary *dictionary = (__bridge NSDictionary *)information;
        NSNumber *flags = dictionary[(__bridge NSString *)kSecCodeInfoFlags];
        NSDictionary *entitlements = dictionary[(__bridge NSString *)kSecCodeInfoEntitlementsDict];
        if (!(flags.unsignedIntValue & 0x10000u) || entitlements.count != 0) status = errSecCSUnsigned;
    }
    if (information) CFRelease(information);
    if (code) CFRelease(code);
    CFRelease(required);
    return status == errSecSuccess;
}
static int observed_context(uint32_t requested) {
    if (!isatty(STDIN_FILENO) && !isatty(STDERR_FILENO)) {
        int tty = open("/dev/tty", O_RDONLY | O_CLOEXEC | O_NOCTTY);
        if (tty < 0) return 0;
        close(tty);
    }
    pid_t process = getppid();
    for (unsigned depth = 0; depth < 64 && process > 1; ++depth) {
        if (signed_process(process, @"io.github.sadiksaifi.spaceterm") ||
            signed_process(process, @"io.github.sadiksaifi.spaceterm-preflight") ||
            signed_process(process, @"io.github.sadiksaifi.spaceterm-development") ||
            signed_process(process, @ST_CAPABILITY_COMPANION_IDENTIFIER)) return requested == ST_CONTEXT_GUI;
        if (signed_process(process, @"io.github.sadiksaifi.spaceterm.server") ||
            signed_process(process, @"io.github.sadiksaifi.spaceterm-preflight.server") ||
            signed_process(process, @"io.github.sadiksaifi.spaceterm-development.server") ||
            signed_process(process, @"io.github.sadiksaifi.spaceterm-development.server.native-evidence.server")) return requested == ST_CONTEXT_DAEMON;
        struct proc_bsdinfo info;
        if (proc_pidinfo(process, PROC_PIDTBSDINFO, 0, &info, sizeof(info)) != sizeof(info) ||
            info.pbi_ppid == (uint32_t)process) return 0;
        process = (pid_t)info.pbi_ppid;
    }
    return 0;
}

static char **worker_environment(st_capability_case capability) {
    /* Import only the probed PATH/locale or agent binding, never the complete
     * GUI environment. A bound violation fails instead of dropping a value and
     * turning different actual shell environments into equal observations. */
    const char *names[ST_CAPABILITY_LOCALE_COUNT + 3] = {"PATH"};
    for (size_t i = 0; i < ST_CAPABILITY_LOCALE_COUNT; ++i) names[i + 1] = st_capability_locale_names[i];
    names[ST_CAPABILITY_LOCALE_COUNT + 1] = "SSH_AUTH_SOCK";
    names[ST_CAPABILITY_LOCALE_COUNT + 2] = "GPG_AGENT_INFO";
    char **result = calloc(ST_CAPABILITY_LOCALE_COUNT + 5, sizeof(char *));
    if (!result) return NULL;
    size_t used = 0;
    for (size_t i = 0; i < ST_CAPABILITY_LOCALE_COUNT + 3; ++i) {
        if ((i <= ST_CAPABILITY_LOCALE_COUNT && capability != ST_CAP_LOGIN_SHELL) ||
            (i == ST_CAPABILITY_LOCALE_COUNT + 1 && capability != ST_CAP_SSH_AGENT) ||
            (i == ST_CAPABILITY_LOCALE_COUNT + 2 && capability != ST_CAP_GPG_AGENT)) continue;
        const char *value = getenv(names[i]);
        if (!value) continue;
        if (strlen(value) > 4096 || asprintf(&result[used], "%s=%s", names[i], value) < 0) {
            for (size_t j = 0; j < used; ++j) { st_capability_discard_owned(result[j], strlen(result[j])); free(result[j]); }
            free(result); return NULL;
        }
        ++used;
    }
    if (asprintf(&result[used], "TMPDIR=/private/tmp") < 0) {
        for (size_t j = 0; j < used; ++j) { st_capability_discard_owned(result[j], strlen(result[j])); free(result[j]); }
        free(result); return NULL;
    }
    return result;
}

st_native_result st_capability_spawn(st_capability_case capability, st_probe_context context,
    uint64_t deadline, pid_t *owned_child, int *owned_output) {
    @autoreleasepool {
        if (!targets_bound || getuid() == 0 || geteuid() != getuid() || !admitted_worker() ||
            !observed_context(context) || owned_tty >= 0) return ST_PROBE_UNAVAILABLE;
        if (capability == ST_CAP_TOUCH_ID_SUDO) {
            int tty = open("/dev/tty", O_RDWR | O_CLOEXEC | O_NOCTTY);
            if (tty < 0 || tcgetpgrp(tty) != getpgrp()) { if (tty >= 0) close(tty); return ST_PROBE_UNAVAILABLE; }
            /* The config is withheld until the worker owns the foreground PTY. */
            close(tty);
        }
        int output[2], input[2];
        if (pipe(output) != 0) return ST_PROBE_IO_UNAVAILABLE;
        if (pipe(input) != 0) { close(output[0]); close(output[1]); return ST_PROBE_IO_UNAVAILABLE; }
        for (size_t i = 0; i < 2; ++i) { fcntl(output[i], F_SETFD, FD_CLOEXEC); fcntl(input[i], F_SETFD, FD_CLOEXEC); }
        char cap[16], ctx[16], time[32];
        snprintf(cap, sizeof(cap), "%u", capability);
        snprintf(ctx, sizeof(ctx), "%u", context);
        snprintf(time, sizeof(time), "%llu", (unsigned long long)deadline);
        char *argv[] = {ST_CAPABILITY_INSTALLED_PATH, "--worker", cap, ctx, time, NULL};
        char **environment = worker_environment(capability);
        if (!environment) { close(output[0]); close(output[1]); close(input[0]); close(input[1]); return ST_PROBE_IO_UNAVAILABLE; }
        posix_spawn_file_actions_t actions;
        posix_spawnattr_t attributes;
        int error = posix_spawn_file_actions_init(&actions);
        if (error == 0) error = posix_spawnattr_init(&attributes);
        if (error != 0) {
            for (size_t i = 0; environment[i]; ++i) free(environment[i]);
            free(environment);
            close(output[0]); close(output[1]); close(input[0]); close(input[1]);
            return ST_PROBE_IO_UNAVAILABLE;
        }
        error = posix_spawn_file_actions_adddup2(&actions, input[0], STDIN_FILENO);
        if (!error) error = posix_spawn_file_actions_adddup2(&actions, output[1], STDOUT_FILENO);
        if (!error) error = posix_spawn_file_actions_addopen(&actions, STDERR_FILENO, "/dev/null", O_WRONLY, 0);
        if (!error) error = posix_spawnattr_setflags(&attributes, POSIX_SPAWN_CLOEXEC_DEFAULT | POSIX_SPAWN_SETPGROUP);
        if (!error) error = posix_spawnattr_setpgroup(&attributes, 0);
        pid_t child = -1;
        if (!error) error = posix_spawn(&child, ST_CAPABILITY_INSTALLED_PATH, &actions, &attributes, argv, environment);
        posix_spawn_file_actions_destroy(&actions);
        posix_spawnattr_destroy(&attributes);
        for (size_t i = 0; environment[i]; ++i) { st_capability_discard_owned(environment[i], strlen(environment[i])); free(environment[i]); }
        free(environment);
        close(output[1]); close(input[0]);
        if (error) { close(output[0]); close(input[1]); return ST_PROBE_IO_UNAVAILABLE; }
        int ready = 1;
        if (capability == ST_CAP_TOUCH_ID_SUDO) {
            ready = st_capability_begin_foreground(child);
        }
        /* Config is one fixed bounded, nonsecret record. Never put paths in argv. */
        int written = ready && st_capability_transfer(input[1], &bound_targets, sizeof(bound_targets), 1, deadline);
        close(input[1]);
        *owned_child = child;
        *owned_output = output[0];
        /* A short config yields no successful record; the common owner still
         * retires/reaps the spawned scope. */
        (void)written;
        return ST_PROBE_OK;
    }
}

static int companion(void) {
    return bound_targets.companion_pid > 0 && signed_process((pid_t)bound_targets.companion_pid,
        @ST_CAPABILITY_COMPANION_IDENTIFIER);
}
static int marker_at(NSURL *directory) {
    NSString *leaf = [NSString stringWithFormat:@"spaceterm-native-520-%@.marker", run_hex()];
    NSURL *url = [directory URLByAppendingPathComponent:leaf isDirectory:NO];
    int fd = open(url.fileSystemRepresentation, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    if (fd < 0) return errno == EACCES || errno == EPERM ? -1 : 0;
    struct stat info;
    uint8_t marker[17] = {0};
    int okay = fstat(fd, &info) == 0 && S_ISREG(info.st_mode) && info.st_uid == getuid() &&
        info.st_size == 16 && read(fd, marker, sizeof(marker)) == 16 &&
        memcmp(marker, bound_targets.run_id, 16) == 0;
    st_capability_discard_owned(marker, sizeof(marker));
    close(fd);
    return okay;
}
static void folders(st_capability_observation *out) {
    const NSSearchPathDirectory directories[] = {NSDesktopDirectory, NSDocumentDirectory, NSDownloadsDirectory};
    for (size_t i = 0; i < 3; ++i) {
        NSArray<NSURL *> *urls = [[NSFileManager defaultManager] URLsForDirectory:directories[i] inDomains:NSUserDomainMask];
        if (urls.count != 1) { unavailable(out, 1); return; }
        int result = marker_at(urls[0]);
        if (result < 0) { out->outcome = ST_OUTCOME_DENIED; out->effect_class = ST_EFFECT_DENIED; return; }
        if (!result) { unavailable(out, 1); return; }
    }
    verified(out);
}
static void volume(st_capability_observation *out) {
    if (!bound_targets.selected_volume[0]) { unavailable(out, 1); return; }
    NSURL *url = [NSURL fileURLWithPath:[NSString stringWithUTF8String:bound_targets.selected_volume] isDirectory:YES];
    NSNumber *removable = nil, *local = nil;
    if (![url getResourceValue:&removable forKey:NSURLVolumeIsRemovableKey error:nil] ||
        ![url getResourceValue:&local forKey:NSURLVolumeIsLocalKey error:nil] ||
        (!removable.boolValue && local.boolValue)) { unavailable(out, 1); return; }
    int result = marker_at(url);
    if (result == 1) verified(out);
    else if (result < 0) { out->outcome = ST_OUTCOME_DENIED; out->effect_class = ST_EFFECT_DENIED; }
    else unavailable(out, 1);
}

@interface STDiscardAudio : NSObject <AVCaptureAudioDataOutputSampleBufferDelegate>
@property(atomic) BOOL observed;
@end
@implementation STDiscardAudio
- (void)captureOutput:(AVCaptureOutput *)output didOutputSampleBuffer:(CMSampleBufferRef)buffer fromConnection:(AVCaptureConnection *)connection {
    (void)output; (void)connection;
    /* The callback retains no sample buffer and reads no audio payload. */
    if (CMSampleBufferGetNumSamples(buffer) > 0) self.observed = YES;
}
@end
static void microphone(st_capability_observation *out, uint64_t deadline) {
    AVAuthorizationStatus authorization = [AVCaptureDevice authorizationStatusForMediaType:AVMediaTypeAudio];
    if (authorization != AVAuthorizationStatusAuthorized) {
        out->preflight_class = authorization == AVAuthorizationStatusNotDetermined ? ST_PREFLIGHT_NOT_DETERMINED : ST_PREFLIGHT_DENIED;
        out->outcome = authorization == AVAuthorizationStatusNotDetermined ? ST_OUTCOME_UNRUN : ST_OUTCOME_DENIED;
        return;
    }
    out->preflight_class = ST_PREFLIGHT_ALLOWED;
    AVCaptureDevice *device = [AVCaptureDevice defaultDeviceWithMediaType:AVMediaTypeAudio];
    if (!device) { unavailable(out, 1); return; }
    AVCaptureDeviceInput *input = [AVCaptureDeviceInput deviceInputWithDevice:device error:nil];
    AVCaptureSession *session = [[AVCaptureSession alloc] init];
    AVCaptureAudioDataOutput *output = [[AVCaptureAudioDataOutput alloc] init];
    if (!input || ![session canAddInput:input] || ![session canAddOutput:output]) { unavailable(out, 0); return; }
    STDiscardAudio *sink = [[STDiscardAudio alloc] init];
    dispatch_queue_t queue = dispatch_queue_create("spaceterm.native.evidence.audio-discard", DISPATCH_QUEUE_SERIAL);
    [output setSampleBufferDelegate:sink queue:queue];
    [session addInput:input]; [session addOutput:output];
    [session startRunning];
    while (!sink.observed && !expired(deadline)) pump(deadline);
    [session stopRunning];
    [output setSampleBufferDelegate:nil queue:NULL];
    dispatch_sync(queue, ^{});
    out->cleanup_class = ST_CLEANUP_VERIFIED;
    if (sink.observed) verified(out);
    else out->outcome = ST_OUTCOME_TIMED_OUT;
}

@interface STScreenObservation : NSObject
@property(atomic) BOOL complete;
@property(atomic) BOOL captured;
@property(atomic, strong) SCWindow *target;
@end
@implementation STScreenObservation
@end

static void screen(st_capability_observation *out, uint64_t deadline) {
    if (!companion() || !bound_targets.companion_window) { unavailable(out, 1); return; }
    if (!CGPreflightScreenCaptureAccess()) { out->preflight_class = ST_PREFLIGHT_DENIED; out->outcome = ST_OUTCOME_DENIED; return; }
    out->preflight_class = ST_PREFLIGHT_ALLOWED;
    STScreenObservation *state = [[STScreenObservation alloc] init];
    NSString *title = [@"SpaceTerm Native Evidence 520 " stringByAppendingString:run_hex()];
    [SCShareableContent getShareableContentExcludingDesktopWindows:YES onScreenWindowsOnly:YES completionHandler:^(SCShareableContent *content, NSError *error) {
        (void)error;
        for (SCWindow *window in content.windows) {
            if (window.windowID == bound_targets.companion_window &&
                window.owningApplication.processID == (pid_t)bound_targets.companion_pid && [window.title isEqualToString:title]) state.target = window;
        }
        state.complete = YES;
    }];
    while (!state.complete && !expired(deadline)) pump(deadline);
    if (!state.complete) { out->outcome = ST_OUTCOME_TIMED_OUT; return; }
    if (!state.target) { unavailable(out, 1); return; }
    SCContentFilter *filter = [[SCContentFilter alloc] initWithDesktopIndependentWindow:state.target];
    SCStreamConfiguration *configuration = [[SCStreamConfiguration alloc] init];
    configuration.width = 16; configuration.height = 16; configuration.showsCursor = NO;
    state.complete = NO;
    [SCScreenshotManager captureImageWithFilter:filter configuration:configuration completionHandler:^(CGImageRef image, NSError *error) {
        (void)error;
        /* No image payload is copied, persisted, or sent to evidence. */
        state.captured = image && CGImageGetWidth(image) > 0 && CGImageGetHeight(image) > 0;
        state.complete = YES;
    }];
    while (!state.complete && !expired(deadline)) pump(deadline);
    if (state.captured) { verified(out); out->cleanup_class = ST_CLEANUP_VERIFIED; }
    else out->outcome = state.complete ? ST_OUTCOME_FAILED : ST_OUTCOME_TIMED_OUT;
}

static int synthetic_control(AXUIElementRef element, NSString *identifier, NSString *value, unsigned depth, unsigned *visited) {
    if (depth > 5 || ++*visited > 128) return 0;
    CFTypeRef found = NULL;
    if (AXUIElementCopyAttributeValue(element, kAXIdentifierAttribute, &found) == kAXErrorSuccess && found) {
        int matches = CFGetTypeID(found) == CFStringGetTypeID() && [(__bridge NSString *)found isEqualToString:identifier];
        CFRelease(found);
        if (matches) {
            CFTypeRef content = NULL;
            int valid_value = AXUIElementCopyAttributeValue(element, kAXValueAttribute, &content) == kAXErrorSuccess &&
                content && CFGetTypeID(content) == CFStringGetTypeID() && [(__bridge NSString *)content isEqualToString:value];
            if (content) CFRelease(content);
            return valid_value;
        }
    }
    CFTypeRef children = NULL;
    if (AXUIElementCopyAttributeValue(element, kAXChildrenAttribute, &children) != kAXErrorSuccess || !children) return 0;
    int result = 0;
    if (CFGetTypeID(children) == CFArrayGetTypeID() && CFArrayGetCount(children) <= 128) {
        for (CFIndex i = 0; i < CFArrayGetCount(children) && !result; ++i) {
            CFTypeRef child = CFArrayGetValueAtIndex(children, i);
            if (CFGetTypeID(child) == AXUIElementGetTypeID()) result = synthetic_control((AXUIElementRef)child, identifier, value, depth + 1, visited);
        }
    }
    CFRelease(children);
    return result;
}
static void accessibility(st_capability_observation *out) {
    if (!companion()) { unavailable(out, 1); return; }
    if (!AXIsProcessTrusted()) { out->preflight_class = ST_PREFLIGHT_DENIED; out->outcome = ST_OUTCOME_DENIED; return; }
    out->preflight_class = ST_PREFLIGHT_ALLOWED;
    AXUIElementRef application = AXUIElementCreateApplication((pid_t)bound_targets.companion_pid);
    AXUIElementSetMessagingTimeout(application, 0.1f);
    NSString *identifier = [@"spaceterm-native-520-control-" stringByAppendingString:run_hex()];
    NSString *value = [@"SpaceTerm520 " stringByAppendingString:run_hex()];
    unsigned visited = 0;
    if (synthetic_control(application, identifier, value, 0, &visited)) verified(out);
    else out->outcome = ST_OUTCOME_FAILED;
    CFRelease(application);
}

static OSStatus event_receipt(AEEventID event_id, uint64_t deadline) {
    pid_t pid = (pid_t)bound_targets.companion_pid;
    AEAddressDesc address = {typeNull, NULL};
    AppleEvent event = {typeNull, NULL}, reply = {typeNull, NULL};
    OSStatus status = AECreateDesc(typeKernelProcessID, &pid, sizeof(pid), &address);
    if (status == noErr) status = AEDeterminePermissionToAutomateTarget(&address, 'ST52', event_id, false);
    if (status == noErr) status = AECreateAppleEvent('ST52', event_id, &address, kAutoGenerateReturnID, kAnyTransactionID, &event);
    const char *run = run_hex().UTF8String;
    if (status == noErr) status = AEPutParamPtr(&event, keyDirectObject, typeUTF8Text, run, 32);
    if (status == noErr && !expired(deadline)) {
        uint64_t remaining = deadline - st_capability_monotonic_ns();
        SInt32 ticks = (SInt32)(remaining / UINT64_C(16666667));
        status = AESendMessage(&event, &reply, kAEWaitReply | kAENeverInteract, ticks > 0 ? ticks : 1);
    }
    if (status == noErr) {
        char response[33] = {0}; DescType type = typeNull; Size size = 0;
        status = AEGetParamPtr(&reply, keyDirectObject, typeUTF8Text, &type, response, 33, &size);
        if (status == noErr && (type != typeUTF8Text || size != 32 || memcmp(response, run, 32))) status = errAEEventNotHandled;
        st_capability_discard_owned(response, sizeof(response));
    }
    AEDisposeDesc(&reply); AEDisposeDesc(&event); AEDisposeDesc(&address);
    return status;
}
@interface STUrlReceipt : NSObject
@property(atomic) BOOL complete;
@property(atomic) BOOL delivered;
@end
@implementation STUrlReceipt
@end

st_native_result st_capability_open_url(uint64_t deadline) {
    if (!companion()) return ST_PROBE_UNAVAILABLE;
    NSURL *url = [NSURL URLWithString:[@"spaceterm-native-520://probe/" stringByAppendingString:run_hex()]];
    NSURL *application_url = [[NSWorkspace sharedWorkspace] URLForApplicationToOpenURL:url];
    NSBundle *application = application_url ? [NSBundle bundleWithURL:application_url] : nil;
    if (![application.bundleIdentifier isEqualToString:@ST_CAPABILITY_COMPANION_IDENTIFIER] ||
        !application.executablePath || !protected_path(application.executablePath.fileSystemRepresentation)) return ST_PROBE_UNAVAILABLE;
    SecStaticCodeRef target_code = NULL;
    SecRequirementRef required = requirement(@ST_CAPABILITY_COMPANION_IDENTIFIER);
    OSStatus authenticated = required ? SecStaticCodeCreateWithPath((__bridge CFURLRef)application_url, kSecCSDefaultFlags, &target_code) : errSecCSUnsigned;
    if (authenticated == errSecSuccess) authenticated = SecStaticCodeCheckValidity(target_code,
        kSecCSStrictValidate | kSecCSCheckAllArchitectures, required);
    if (target_code) CFRelease(target_code);
    if (required) CFRelease(required);
    if (authenticated != errSecSuccess) return ST_PROBE_UNAVAILABLE;
    STUrlReceipt *receipt = [[STUrlReceipt alloc] init];
    NSWorkspaceOpenConfiguration *configuration = [NSWorkspaceOpenConfiguration configuration];
    configuration.activates = NO; configuration.createsNewApplicationInstance = NO;
    [[NSWorkspace sharedWorkspace] openURLs:@[url] withApplicationAtURL:application_url configuration:configuration completionHandler:^(NSRunningApplication *running, NSError *error) {
        (void)error;
        receipt.delivered = running.processIdentifier == (pid_t)bound_targets.companion_pid;
        receipt.complete = YES;
    }];
    while (!receipt.complete && !expired(deadline)) pump(deadline);
    if (!receipt.complete) return ST_PROBE_TIMED_OUT;
    if (!receipt.delivered) return ST_PROBE_UNAVAILABLE;
    return ST_PROBE_OK;
}
st_native_result st_capability_apple_event_receipt(uint64_t deadline) {
    if (!companion()) return ST_PROBE_UNAVAILABLE;
    OSStatus status = event_receipt('ping', deadline);
    if (status == noErr) return ST_PROBE_OK;
    if (status == errAEEventNotPermitted) return ST_PROBE_AUTHORIZATION_DENIED;
    if (status == errAETimeout || expired(deadline)) return ST_PROBE_TIMED_OUT;
    return ST_PROBE_UNAVAILABLE;
}

#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
static void login_keychain(st_capability_observation *out) {
    /* Scoped synthetic item, never System Keychain and never a real credential.
     * Receipt cleanup uses the deterministic service/account below if this
     * isolated worker dies after enrollment. No collision is adopted/deleted. */
    SecKeychainRef keychain = NULL;
    struct passwd *native_account = getpwuid(getuid());
    char keychain_path[4096];
    if (!native_account || !native_account->pw_dir || native_account->pw_dir[0] != '/' ||
        strlen(native_account->pw_dir) > 3000) { unavailable(out, 1); return; }
    snprintf(keychain_path, sizeof(keychain_path), "%s/Library/Keychains/login.keychain-db", native_account->pw_dir);
    struct stat file;
    if (lstat(keychain_path, &file) != 0 || !S_ISREG(file.st_mode) || file.st_uid != getuid() ||
        SecKeychainSetUserInteractionAllowed(false) != errSecSuccess ||
        SecKeychainOpen(keychain_path, &keychain) != errSecSuccess || !keychain) { unavailable(out, 1); return; }
    char opened_path[4096]; UInt32 opened_length = sizeof(opened_path);
    if (SecKeychainGetPath(keychain, &opened_length, opened_path) != errSecSuccess || opened_length >= sizeof(opened_path)) {
        CFRelease(keychain); unavailable(out, 1); return;
    }
    opened_path[opened_length] = 0;
    if (strcmp(opened_path, keychain_path) != 0) { CFRelease(keychain); unavailable(out, 1); return; }
    NSString *name = [@"SpaceTerm Native Evidence 520 capability " stringByAppendingString:run_hex()];
    const char *service = name.UTF8String, *account = run_hex().UTF8String;
    uint8_t secret[32] = {0};
    SecKeychainItemRef item = NULL;
    OSStatus status = SecRandomCopyBytes(kSecRandomDefault, sizeof(secret), secret);
    if (status != errSecSuccess) {
        st_capability_discard_owned(secret, sizeof(secret)); CFRelease(keychain); unavailable(out, 1); return;
    }
    status = SecKeychainAddGenericPassword(keychain, (UInt32)strlen(service), service,
        32, account, sizeof(secret), secret, &item);
    if (status != errSecSuccess || !item) {
        st_capability_discard_owned(secret, sizeof(secret));
        if (item) CFRelease(item);
        CFRelease(keychain);
        if (status == errSecDuplicateItem) unavailable(out, 1);
        else {
            /* Enrollment may have changed native state before returning failure.
             * Never delete by name or relabel unknown publication as Unrun. */
            out->outcome = ST_OUTCOME_CLEANUP_UNRESOLVED;
            out->cleanup_class = ST_CLEANUP_UNRESOLVED;
        }
        return;
    }
    out->preflight_class = ST_PREFLIGHT_ALLOWED;
    void *bytes = NULL; UInt32 length = 0;
    status = SecKeychainItemCopyAttributesAndData(item, NULL, NULL, NULL, &length, &bytes);
    int matched = status == errSecSuccess && length == sizeof(secret) && bytes && memcmp(bytes, secret, sizeof(secret)) == 0;
    if (bytes) { st_capability_discard_owned(bytes, length); SecKeychainItemFreeAttributesAndData(NULL, bytes); }
    st_capability_discard_owned(secret, sizeof(secret));
    status = SecKeychainItemDelete(item);
    CFRelease(item);
    if (status == errSecSuccess) status = SecKeychainFindGenericPassword(keychain,
        (UInt32)strlen(service), service, 32, account, NULL, NULL, NULL);
    CFRelease(keychain);
    if (status != errSecItemNotFound) { out->cleanup_class = ST_CLEANUP_UNRESOLVED; out->outcome = ST_OUTCOME_CLEANUP_UNRESOLVED; return; }
    out->cleanup_class = ST_CLEANUP_VERIFIED;
    if (matched) verified(out); else out->outcome = ST_OUTCOME_FAILED;
}
#pragma clang diagnostic pop

static int agent_socket(const char *path, uint64_t deadline) {
    if (!path || path[0] != '/' || strlen(path) >= sizeof(((struct sockaddr_un *)0)->sun_path)) return -1;
    struct stat info;
    if (lstat(path, &info) != 0 || !S_ISSOCK(info.st_mode) || info.st_uid != getuid()) return -1;
    int fd = socket(AF_UNIX, SOCK_STREAM, 0);
    if (fd < 0) return -1;
    fcntl(fd, F_SETFD, FD_CLOEXEC); fcntl(fd, F_SETFL, O_NONBLOCK);
    struct sockaddr_un address = {0};
    address.sun_family = AF_UNIX; strcpy(address.sun_path, path);
    if (connect(fd, (struct sockaddr *)&address, sizeof(address)) != 0) {
        if (errno != EINPROGRESS || !st_capability_wait_fd(fd, POLLOUT, deadline)) { close(fd); return -1; }
        int error = 0; socklen_t size = sizeof(error);
        if (getsockopt(fd, SOL_SOCKET, SO_ERROR, &error, &size) != 0 || error != 0) { close(fd); return -1; }
    }
    uid_t uid; gid_t gid;
    if (getpeereid(fd, &uid, &gid) != 0 || uid != getuid()) { close(fd); return -1; }
    return fd;
}
st_native_result st_capability_url_receipt(uint64_t deadline) {
    if (!companion()) return ST_PROBE_UNAVAILABLE;
    struct proc_bsdinfo before, after;
    pid_t pid = (pid_t)bound_targets.companion_pid;
    if (proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &before, sizeof(before)) != sizeof(before))
        return ST_PROBE_UNAVAILABLE;
    char path[sizeof(((struct sockaddr_un *)0)->sun_path)];
    snprintf(path, sizeof(path), "/private/tmp/spaceterm-native-520-%s.url.sock", run_hex().UTF8String);
    int fd = agent_socket(path, deadline);
    if (fd < 0) return expired(deadline) ? ST_PROBE_TIMED_OUT : ST_PROBE_UNAVAILABLE;
    pid_t peer = -1; socklen_t size = sizeof(peer);
    int authenticated = getsockopt(fd, SOL_LOCAL, LOCAL_PEERPID, &peer, &size) == 0 &&
        size == sizeof(peer) && peer == pid && companion();
    uint8_t reply[17] = {0};
    int matched = authenticated && st_capability_transfer(fd, bound_targets.run_id, 16, 1, deadline) &&
        shutdown(fd, SHUT_WR) == 0 && st_capability_transfer(fd, reply, 16, 0, deadline) &&
        memcmp(reply, bound_targets.run_id, 16) == 0 && st_capability_wait_fd(fd, POLLIN, deadline) &&
        read(fd, reply + 16, 1) == 0 &&
        proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &after, sizeof(after)) == sizeof(after) &&
        before.pbi_start_tvsec == after.pbi_start_tvsec && before.pbi_start_tvusec == after.pbi_start_tvusec;
    st_capability_discard_owned(reply, sizeof(reply)); close(fd);
    if (matched && !expired(deadline)) return ST_PROBE_OK;
    return expired(deadline) ? ST_PROBE_TIMED_OUT : ST_PROBE_UNAVAILABLE;
}

static void ssh_agent(st_capability_observation *out, uint64_t deadline) {
    int fd = agent_socket(getenv("SSH_AUTH_SOCK"), deadline);
    if (fd < 0) { unavailable(out, 1); return; }
    out->preflight_class = ST_PREFLIGHT_ALLOWED;
    /* An unsupported synthetic extension round trip. It never enumerates keys,
     * adds/removes identities, signs with a user key, or exports fingerprints. */
    static const char extension[] = "spaceterm-native-520-probe";
    uint8_t request[9 + sizeof(extension) - 1] = {0};
    request[3] = sizeof(request) - 4; request[4] = 27; request[8] = sizeof(extension) - 1;
    memcpy(request + 9, extension, sizeof(extension) - 1);
    uint8_t response[5] = {0};
    int success = st_capability_transfer(fd, request, sizeof(request), 1, deadline) && st_capability_transfer(fd, response, 5, 0, deadline) &&
        response[0] == 0 && response[1] == 0 && response[2] == 0 && response[3] == 1 &&
        (response[4] == 5 || response[4] == 28);
    st_capability_discard_owned(response, sizeof(response)); close(fd);
    if (success) verified(out); else out->outcome = expired(deadline) ? ST_OUTCOME_TIMED_OUT : ST_OUTCOME_FAILED;
}
static int assuan_line(int fd, uint64_t deadline, int *okay) {
    char line[1025] = {0}; size_t used = 0;
    while (used < sizeof(line) - 1 && st_capability_transfer(fd, line + used, 1, 0, deadline)) {
        if (line[used++] == '\n') {
            *okay = used >= 3 && memcmp(line, "OK", 2) == 0 && (line[2] == '\n' || line[2] == ' ' || line[2] == '\r');
            int error = used >= 4 && memcmp(line, "ERR ", 4) == 0;
            st_capability_discard_owned(line, sizeof(line));
            return error ? -1 : 1;
        }
    }
    st_capability_discard_owned(line, sizeof(line)); return 0;
}
static void gpg_agent(st_capability_observation *out, uint64_t deadline) {
    /* Baseline uses only a supplied narrow legacy socket binding. Modern agent
     * discovery belongs to the session adapter, not an uncontrolled gpgconf. */
    const char *binding = getenv("GPG_AGENT_INFO");
    if (!binding || strlen(binding) >= 4096) { unavailable(out, 1); return; }
    char path[4096]; strcpy(path, binding);
    char *separator = strchr(path, ':'); if (separator) *separator = 0;
    int fd = agent_socket(path, deadline);
    st_capability_discard_owned(path, sizeof(path));
    if (fd < 0) { unavailable(out, 1); return; }
    out->preflight_class = ST_PREFLIGHT_ALLOWED;
    int okay = 0, success = 0;
    for (unsigned i = 0; i < 4 && !okay; ++i) if (assuan_line(fd, deadline, &okay) <= 0) break;
    char query[] = "GETINFO version\n";
    if (okay && st_capability_transfer(fd, query, sizeof(query) - 1, 1, deadline)) {
        okay = 0;
        for (unsigned i = 0; i < 8 && !okay; ++i) if (assuan_line(fd, deadline, &okay) <= 0) break;
        success = okay;
    }
    close(fd);
    if (success) verified(out); else out->outcome = expired(deadline) ? ST_OUTCOME_TIMED_OUT : ST_OUTCOME_FAILED;
}

static int owned_wait(pid_t child, uint64_t deadline, int *status) {
    while (!expired(deadline)) {
        pid_t result = waitpid(child, status, WNOHANG);
        if (result == child) return 1;
        if (result < 0 && errno != EINTR) return 0;
        struct timespec pause = {0, 1000000}; nanosleep(&pause, NULL);
    }
    /* The child PID is still retained here. Attempt bounded direct-child
     * retirement, but never treat it as descendant-scope absence. */
    (void)kill(child, SIGTERM);
    uint64_t retirement = st_capability_monotonic_ns() + UINT64_C(250000000);
    while (!expired(retirement)) {
        pid_t result = waitpid(child, status, WNOHANG);
        if (result == child || (result < 0 && errno != EINTR)) break;
        struct timespec pause = {0, 1000000}; nanosleep(&pause, NULL);
    }
    return 0;
}
static void touch_id(st_capability_observation *out, uint64_t deadline) {
    int tty = open("/dev/tty", O_RDWR | O_CLOEXEC | O_NOCTTY);
    if (tty < 0) { unavailable(out, 1); return; }
    out->preflight_class = ST_PREFLIGHT_ALLOWED;
    posix_spawn_file_actions_t actions; posix_spawnattr_t attributes;
    if (posix_spawn_file_actions_init(&actions) || posix_spawnattr_init(&attributes)) { close(tty); unavailable(out, 0); return; }
    int error = posix_spawn_file_actions_adddup2(&actions, tty, STDIN_FILENO);
    if (!error) error = posix_spawn_file_actions_adddup2(&actions, tty, STDOUT_FILENO);
    if (!error) error = posix_spawn_file_actions_adddup2(&actions, tty, STDERR_FILENO);
    if (!error) error = posix_spawnattr_setflags(&attributes, POSIX_SPAWN_CLOEXEC_DEFAULT);
    char *environment[] = {"PATH=/usr/bin:/bin:/usr/sbin:/sbin", "LC_ALL=C", NULL};
    pid_t child = -1;
    if (!error) error = posix_spawn(&child, "/usr/bin/sudo", &actions, &attributes, (char *const *)st_capability_sudo_argv(), environment);
    posix_spawn_file_actions_destroy(&actions); posix_spawnattr_destroy(&attributes); close(tty);
    if (error) { unavailable(out, 0); return; }
    int status = 0;
    if (!owned_wait(child, deadline, &status)) { out->outcome = ST_OUTCOME_TIMED_OUT; return; }
    if (WIFEXITED(status) && WEXITSTATUS(status) == 0) {
        out->effect_class = ST_EFFECT_COMPLETED_UNATTRIBUTED;
        out->outcome = ST_OUTCOME_UNAVAILABLE; /* No public proof of Touch ID vs password/cache. */
    } else if (WIFSIGNALED(status) && (WTERMSIG(status) == SIGINT || WTERMSIG(status) == SIGTERM))
        out->outcome = ST_OUTCOME_CANCELLED;
    else out->outcome = ST_OUTCOME_UNAVAILABLE; /* A nonzero sudo exit is not a native denial classification. */
}
void st_capability_controlled_shell_order(st_capability_observation *out, uint64_t deadline) {
    struct passwd *account = getpwuid(getuid());
    if (!account || strcmp(account->pw_shell, "/bin/zsh")) { unavailable(out, 1); return; }
    const char *root = "/Library/Application Support/SpaceTerm Native Evidence 520/capability-shell";
    const char *leaves[] = {".zshenv", ".zprofile", ".zshrc", ".zlogin"};
    for (size_t i = 0; i < 4; ++i) {
        char path[4096]; snprintf(path, sizeof(path), "%s/%s", root, leaves[i]);
        if (!protected_path(path)) { unavailable(out, 1); return; }
    }
    int pipe_fds[2];
    if (pipe(pipe_fds)) { unavailable(out, 0); return; }
    fcntl(pipe_fds[0], F_SETFD, FD_CLOEXEC); fcntl(pipe_fds[1], F_SETFD, FD_CLOEXEC);
    posix_spawn_file_actions_t actions; posix_spawnattr_t attributes;
    if (posix_spawn_file_actions_init(&actions) || posix_spawnattr_init(&attributes)) { close(pipe_fds[0]); close(pipe_fds[1]); unavailable(out, 0); return; }
    int error = posix_spawn_file_actions_addopen(&actions, STDIN_FILENO, "/dev/null", O_RDONLY, 0);
    if (!error) error = posix_spawn_file_actions_adddup2(&actions, pipe_fds[1], STDOUT_FILENO);
    if (!error) error = posix_spawn_file_actions_addopen(&actions, STDERR_FILENO, "/dev/null", O_WRONLY, 0);
    if (!error) error = posix_spawnattr_setflags(&attributes, POSIX_SPAWN_CLOEXEC_DEFAULT);
    char *argv[] = {"/bin/zsh", "-l", "-i", "-c", ":", NULL};
    char *environment[] = {"PATH=/usr/bin:/bin", "LC_ALL=C", "TERM=dumb",
        "ZDOTDIR=/Library/Application Support/SpaceTerm Native Evidence 520/capability-shell", NULL};
    pid_t child = -1;
    if (!error) error = posix_spawn(&child, "/bin/zsh", &actions, &attributes, argv, environment);
    posix_spawn_file_actions_destroy(&actions); posix_spawnattr_destroy(&attributes); close(pipe_fds[1]);
    if (error) { close(pipe_fds[0]); unavailable(out, 0); return; }
    uint8_t order[5] = {0}; size_t used = 0;
    while (used < sizeof(order) && !expired(deadline) && st_capability_wait_fd(pipe_fds[0], POLLIN, deadline)) {
        ssize_t count = read(pipe_fds[0], order + used, sizeof(order) - used);
        if (count > 0) used += (size_t)count;
        else if (count == 0) break;
        else if (errno != EINTR && errno != EAGAIN) break;
    }
    close(pipe_fds[0]);
    int status = 0;
    if (!owned_wait(child, deadline, &status)) out->outcome = ST_OUTCOME_TIMED_OUT;
    else if (used == 4 && memcmp(order, "1234", 4) == 0 && WIFEXITED(status) && WEXITSTATUS(status) == 0) verified(out);
    else out->outcome = ST_OUTCOME_FAILED;
    st_capability_discard_owned(order, sizeof(order));
    out->cleanup_class = ST_CLEANUP_UNRESOLVED;
    out->outcome = ST_OUTCOME_CLEANUP_UNRESOLVED;
}

st_native_result st_capability_observe_environment(st_probe_context context,
    st_capability_retained_environment **out) {
    if (!out || *out || getuid() == 0 || geteuid() != getuid() ||
        !signed_process(getpid(), @ST_CAPABILITY_IDENTIFIER) || !observed_context(context))
        return ST_PROBE_UNAVAILABLE;
    struct passwd *account = getpwuid(getuid());
    if (!account || !account->pw_shell || (strcmp(account->pw_shell, "/bin/zsh") && strcmp(account->pw_shell, "/bin/bash")))
        return ST_PROBE_UNAVAILABLE;
    pid_t shell = getppid();
    /* Worker re-exec adds exactly one signed probe parent. Never search past an
     * unrelated intermediary to claim its ancestor's shell environment. */
    if (signed_process(shell, @ST_CAPABILITY_IDENTIFIER)) {
        struct proc_bsdinfo parent;
        if (proc_pidinfo(shell, PROC_PIDTBSDINFO, 0, &parent, sizeof(parent)) != sizeof(parent))
            return ST_PROBE_UNAVAILABLE;
        shell = (pid_t)parent.pbi_ppid;
    }
    char shell_path[PROC_PIDPATHINFO_MAXSIZE];
    struct proc_bsdinfo before, after, current;
    int tty = open("/dev/tty", O_RDONLY | O_CLOEXEC | O_NOCTTY);
    int matches = tty >= 0 && isatty(tty) &&
        proc_pidinfo(getpid(), PROC_PIDTBSDINFO, 0, &current, sizeof(current)) == sizeof(current) &&
        (current.pbi_flags & PROC_FLAG_CONTROLT) &&
        proc_pidinfo(shell, PROC_PIDTBSDINFO, 0, &before, sizeof(before)) == sizeof(before) &&
        before.pbi_uid == getuid() && (before.pbi_flags & PROC_FLAG_CONTROLT) && before.e_tdev == current.e_tdev &&
        proc_pidpath(shell, shell_path, sizeof(shell_path)) > 0 && !strcmp(shell_path, account->pw_shell);
    if (tty >= 0) close(tty);
    st_capability_discard_owned(shell_path, sizeof(shell_path));
    if (!matches) return ST_PROBE_UNAVAILABLE;
    /* Native argv inspection remains memory-only. The buffer can also contain
     * unrelated process data; bound and wipe the entire allocation on all exits. */
    size_t length = 128 * 1024;
    uint8_t *bytes = calloc(1, length);
    if (!bytes) return ST_PROBE_IO_UNAVAILABLE;
    int query[] = {CTL_KERN, KERN_PROCARGS2, shell};
    size_t received = length;
    int recognized = 0;
    if (sysctl(query, 3, bytes, &received, NULL, 0) == 0 && received > sizeof(int)) {
        int count; memcpy(&count, bytes, sizeof(count));
        size_t offset = sizeof(count);
        while (offset < received && bytes[offset]) ++offset;
        while (offset < received && !bytes[offset]) ++offset;
        const char *arguments[32]; size_t used = 0;
        if (count > 0 && count <= 32) {
            while (used < (size_t)count && offset < received) {
                arguments[used] = (const char *)bytes + offset;
                while (offset < received && bytes[offset]) ++offset;
                if (offset == received) break;
                ++offset; ++used;
            }
            recognized = used == (size_t)count && st_capability_shell_invocation(arguments, used, 1);
        }
    }
    st_capability_discard_owned(bytes, length); free(bytes);
    if (!recognized || proc_pidinfo(shell, PROC_PIDTBSDINFO, 0, &after, sizeof(after)) != sizeof(after) ||
        before.pbi_start_tvsec != after.pbi_start_tvsec || before.pbi_start_tvusec != after.pbi_start_tvusec)
        return ST_PROBE_UNAVAILABLE;
    const char *locales[ST_CAPABILITY_LOCALE_COUNT];
    for (size_t i = 0; i < ST_CAPABILITY_LOCALE_COUNT; ++i) locales[i] = getenv(st_capability_locale_names[i]);
    return st_capability_environment_retain(context, getenv("PATH"), locales, out);
}

static int environment_peer(int fd) {
    uid_t uid; gid_t gid; pid_t pid = -1; socklen_t size = sizeof(pid);
    return getuid() != 0 && geteuid() == getuid() && getpeereid(fd, &uid, &gid) == 0 && uid == getuid() &&
        getsockopt(fd, SOL_LOCAL, LOCAL_PEERPID, &pid, &size) == 0 && size == sizeof(pid) &&
        pid > 0 && signed_process(pid, @ST_CAPABILITY_IDENTIFIER);
}
st_native_result st_capability_environment_send_current(int channel, st_probe_context context,
    uint64_t deadline) {
    uint64_t now = st_capability_monotonic_ns();
    if (!now || deadline <= now) return ST_PROBE_TIMED_OUT;
    if (deadline - now > UINT64_C(5000000000)) return ST_PROBE_LIMIT;
    if (!environment_peer(channel)) return ST_PROBE_UNAVAILABLE;
    st_capability_retained_environment *value = NULL;
    st_native_result result = st_capability_observe_environment(context, &value);
    if (result == ST_PROBE_OK) result = st_capability_environment_send_private(channel, value, deadline);
    /* Keep the authenticated peer process alive until the collector recognizes
     * both actual contexts. EOF on the snapshot half does not retire the owner. */
    uint8_t acknowledgement = 0, extra;
    if (result == ST_PROBE_OK && (!st_capability_transfer(channel, &acknowledgement, 1, 0, deadline) ||
        acknowledgement != 1 || !st_capability_wait_fd(channel, POLLIN, deadline) || read(channel, &extra, 1) != 0))
        result = ST_PROBE_IO_UNAVAILABLE;
    st_capability_environment_release(&value);
    return result;
}
st_native_result st_capability_environment_compare_channels(int gui_channel, int daemon_channel,
    uint64_t deadline, st_capability_environment_difference *out) {
    uint64_t now = st_capability_monotonic_ns();
    if (!now || deadline <= now) return ST_PROBE_TIMED_OUT;
    if (deadline - now > UINT64_C(5000000000)) return ST_PROBE_LIMIT;
    if (!out || gui_channel == daemon_channel || !signed_process(getpid(), @ST_CAPABILITY_IDENTIFIER) ||
        !environment_peer(gui_channel) || !environment_peer(daemon_channel) || expired(deadline))
        return ST_PROBE_UNAVAILABLE;
    st_capability_retained_environment *gui = NULL, *daemon = NULL;
    st_native_result result = st_capability_environment_receive_private(gui_channel, ST_CONTEXT_GUI, deadline, &gui);
    if (result == ST_PROBE_OK) result = st_capability_environment_receive_private(daemon_channel, ST_CONTEXT_DAEMON, deadline, &daemon);
    if (result == ST_PROBE_OK && !expired(deadline)) result = st_capability_environment_compare(gui, daemon, out);
    else if (expired(deadline)) result = ST_PROBE_TIMED_OUT;
    if (result == ST_PROBE_OK) {
        uint8_t acknowledgement = 1;
        if (!st_capability_transfer(gui_channel, &acknowledgement, 1, 1, deadline) || shutdown(gui_channel, SHUT_WR) ||
            !st_capability_transfer(daemon_channel, &acknowledgement, 1, 1, deadline) || shutdown(daemon_channel, SHUT_WR))
            result = ST_PROBE_IO_UNAVAILABLE;
    }
    st_capability_environment_release(&gui); st_capability_environment_release(&daemon);
    return result;
}

st_native_result st_capability_native_worker(st_capability_case capability, st_probe_context context,
    uint64_t deadline, st_capability_observation *out) {
    @autoreleasepool {
        if (!out || !targets_bound || capability < 1 || capability > 13 || context < 1 || context > 2) return ST_PROBE_INVALID;
        *out = (st_capability_observation){1, capability, context, ST_OUTCOME_UNAVAILABLE,
            ST_RESPONSIBLE_UNKNOWN, ST_PREFLIGHT_UNKNOWN, ST_EFFECT_UNOBSERVED, ST_CLEANUP_NOT_NEEDED};
        /* Context ancestry does not establish TCC responsible-application identity. */
        if (getuid() == 0 || geteuid() != getuid() || !signed_process(getpid(), @ST_CAPABILITY_IDENTIFIER) ||
            !observed_context(context)) return ST_PROBE_UNAVAILABLE;
        switch (capability) {
            case ST_CAP_PROTECTED_FOLDERS: folders(out); break;
            case ST_CAP_SELECTED_VOLUME: volume(out); break;
            case ST_CAP_MICROPHONE: microphone(out, deadline); break;
            case ST_CAP_SCREEN_RECORDING: screen(out, deadline); break;
            case ST_CAP_ACCESSIBILITY: accessibility(out); break;
            case ST_CAP_CLIPBOARD:
                /* NSPasteboard has no atomic change-count-conditioned replacement.
                 * A check followed by clearContents can destroy a newer user change.
                 * Do not modify the general pasteboard or claim pbcopy/pbpaste proof. */
                unavailable(out, 0); break;
            case ST_CAP_URL: st_capability_events(out, deadline, 1); break;
            case ST_CAP_APPLE_EVENTS: st_capability_events(out, deadline, 0); break;
            case ST_CAP_LOGIN_KEYCHAIN: login_keychain(out); break;
            case ST_CAP_SSH_AGENT: ssh_agent(out, deadline); break;
            case ST_CAP_GPG_AGENT: gpg_agent(out, deadline); break;
            case ST_CAP_TOUCH_ID_SUDO: touch_id(out, deadline); break;
            case ST_CAP_LOGIN_SHELL: {
                st_capability_retained_environment *environment = NULL;
                if (st_capability_observe_environment(context, &environment) == ST_PROBE_OK) verified(out);
                else unavailable(out, 1);
                st_capability_environment_release(&environment);
                break;
            }
        }
        return ST_PROBE_OK;
    }
}

static int parse_decimal(const char *text, uint64_t *out) {
    if (!text || !text[0] || (text[0] == '0' && text[1])) return 0;
    uint64_t value = 0;
    for (size_t i = 0; text[i]; ++i) {
        if (i >= 20 || text[i] < '0' || text[i] > '9' || value > (UINT64_MAX - (unsigned)(text[i] - '0')) / 10) return 0;
        value = value * 10 + (unsigned)(text[i] - '0');
    }
    *out = value; return 1;
}
static void lifetime_expired(int signal_number) {
    (void)signal_number;
    /* An independent guard also covers blocking framework calls and parent loss.
     * This is the worker's own group, never a PID-name-based process search. */
    kill(0, SIGTERM);
    _exit(124);
}
int st_capability_worker_entry(int argc, char **argv) {
    @autoreleasepool {
        uint64_t capability, context, deadline;
        if (argc != 5 || strcmp(argv[1], "--worker") || !parse_decimal(argv[2], &capability) ||
            !parse_decimal(argv[3], &context) || !parse_decimal(argv[4], &deadline) || capability > 13 || context > 2 ||
            !capability || !context || expired(deadline) || getpgrp() != getpid() ||
            !signed_process(getppid(), @ST_CAPABILITY_IDENTIFIER)) return 1;
        struct rlimit no_core = {0, 0};
        if (setrlimit(RLIMIT_CORE, &no_core) != 0) return 1;
        uint64_t remaining = deadline - st_capability_monotonic_ns();
        uint64_t limit = capability == ST_CAP_TOUCH_ID_SUDO ? UINT64_C(60000000000) : UINT64_C(5000000000);
        if (remaining > limit) return 1;
        struct sigaction guard = {0};
        guard.sa_handler = lifetime_expired; sigemptyset(&guard.sa_mask);
        if (sigaction(SIGALRM, &guard, NULL) != 0) return 1;
        struct itimerval timer = {0};
        timer.it_value.tv_sec = (time_t)(remaining / UINT64_C(1000000000));
        timer.it_value.tv_usec = (suseconds_t)((remaining % UINT64_C(1000000000)) / 1000 + 1);
        if (timer.it_value.tv_usec >= 1000000) { ++timer.it_value.tv_sec; timer.it_value.tv_usec = 0; }
        if (setitimer(ITIMER_REAL, &timer, NULL) != 0) return 1;
        st_capability_targets targets;
        if (!st_capability_transfer(STDIN_FILENO, &targets, sizeof(targets), 0, deadline)) return 1;
        uint8_t extra;
        if (!st_capability_wait_fd(STDIN_FILENO, POLLIN, deadline) || read(STDIN_FILENO, &extra, 1) != 0 ||
            st_capability_bind_targets(&targets) != ST_PROBE_OK) return 1;
        st_capability_observation observation;
        st_native_result result = st_capability_native_worker((uint32_t)capability, (uint32_t)context, deadline, &observation);
        uint8_t bytes[32];
        if (result != ST_PROBE_OK || st_capability_encode(&observation, bytes) != 0 ||
            !st_capability_transfer(STDOUT_FILENO, bytes, sizeof(bytes), 1, deadline)) return 1;
        return 0;
    }
}
