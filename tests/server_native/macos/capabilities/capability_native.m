#include "capability_internal.h"
#include "capability_environment.h"
#include "capability_process.h"
#include "capability_agents.h"
#include "capability_clipboard.h"
#include "capability_targets.h"
#include "capability_terminal.h"
#import <OpenDirectory/OpenDirectory.h>
#include <SystemConfiguration/SystemConfiguration.h>
#include <CommonCrypto/CommonDigest.h>
#import <AppKit/AppKit.h>
#import <AVFoundation/AVFoundation.h>
#import <ScreenCaptureKit/ScreenCaptureKit.h>
#import <Security/Security.h>
#include <ApplicationServices/ApplicationServices.h>
#include <Carbon/Carbon.h>
#include <bsm/libbsm.h>
#include <errno.h>
#include <dlfcn.h>
#include <fcntl.h>
#include <libproc.h>
#include <poll.h>
#include <pwd.h>
#include <spawn.h>
#include <signal.h>
#include <pthread.h>
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
static int shell_mode_bound;
static uint32_t shell_mode;
static int owned_tty = -1;
static pid_t foreground_child = -1;
static pid_t previous_foreground = -1;
static struct termios previous_modes;
static pid_t tree_child = -1;
static uint64_t tree_root;

static int foreground(int tty, pid_t group) {
    sigset_t blocked, original;
    sigemptyset(&blocked); sigaddset(&blocked, SIGTTOU);
    if (pthread_sigmask(SIG_BLOCK, &blocked, &original) != 0) return 0;
    int result = tcsetpgrp(tty, group) == 0;
    if (pthread_sigmask(SIG_SETMASK, &original, NULL) != 0) return 0;
    return result;
}
int st_capability_begin_foreground(pid_t child) {
    if (child <= 0 || owned_tty >= 0) return 0;
    owned_tty = open("/dev/tty", O_RDWR | O_CLOEXEC | O_NOCTTY);
    foreground_child = child;
    previous_foreground = owned_tty >= 0 ? tcgetpgrp(owned_tty) : -1;
    return previous_foreground == getpgrp() && tcgetattr(owned_tty, &previous_modes) == 0 && foreground(owned_tty, child);
}
st_native_result st_capability_finish_spawn(pid_t child, int retired, int root_reaped) {
    (void)retired;
    int tree_retired = 1;
    if (tree_child > 0) {
        tree_retired = child == tree_child && root_reaped && tree_root &&
            st_capability_tree_retire_native(tree_root) == ST_PROBE_OK;
        tree_child = -1; tree_root = 0;
    }
    if (owned_tty < 0) return tree_retired ? ST_PROBE_OK : ST_PROBE_CLEANUP_UNRESOLVED;
    int okay = child == foreground_child && tcgetpgrp(owned_tty) == child &&
        foreground(owned_tty, previous_foreground) && st_capability_terminal_restore_owner(owned_tty, &previous_modes);
    /* Never replace a newer foreground-group change. */
    close(owned_tty); owned_tty = -1; foreground_child = -1; previous_foreground = -1;
    return okay && tree_retired ? ST_PROBE_OK : ST_PROBE_CLEANUP_UNRESOLVED;
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

static NSString *id_hex(const uint8_t bytes[16]) {
    char text[33];
    static const char hex[] = "0123456789abcdef";
    for (size_t i = 0; i < 16; ++i) {
        text[i * 2] = hex[bytes[i] >> 4];
        text[i * 2 + 1] = hex[bytes[i] & 15];
    }
    text[32] = 0;
    return [NSString stringWithUTF8String:text];
}

static NSString *run_hex(void) { return id_hex(bound_targets.run_id); }
static NSString *target_hex(void) { return id_hex(bound_targets.target_nonce); }

st_native_result st_capability_bind_targets(const st_capability_targets *targets) {
    if (!targets || targets->schema != 2 || targets->controller_pid > INT32_MAX ||
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
    nonzero = 0;
    for (size_t i = 0; i < 16; ++i) nonzero |= targets->target_nonce[i];
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
typedef struct {
    st_capability_process_identity effect, actor;
    uint32_t session, account;
} responsible_binding;
static uint32_t observe_responsible(responsible_binding *binding) {
    /* libSystem's responsibility observer is evidence-only, not ancestry and
     * not a TCC grant. Older systems without this fixed OS symbol stay Unknown. */
    typedef pid_t (*responsible_fn)(pid_t);
    responsible_fn responsible = (responsible_fn)dlsym(RTLD_DEFAULT,
        "responsibility_get_pid_responsible_for_pid");
    st_capability_process_identity effect_before, effect_after, actor_before, actor_after;
    auditinfo_addr_t session_before, session_after;
    if (!responsible || !st_capability_process_observe(getpid(), &effect_before) ||
        getaudit_addr(&session_before, sizeof(session_before)) != 0) return ST_RESPONSIBLE_UNKNOWN;
    pid_t actor = responsible(getpid());
    if (actor <= 0 || !st_capability_process_observe(actor, &actor_before)) return ST_RESPONSIBLE_UNKNOWN;
    uint32_t classification = ST_RESPONSIBLE_OTHER;
    if (signed_process(actor, @ST_CAPABILITY_IDENTIFIER)) classification = ST_RESPONSIBLE_PROBE;
    else if (signed_process(actor, @ST_CAPABILITY_COMPANION_IDENTIFIER)) classification = ST_RESPONSIBLE_COMPANION;
    else if (signed_process(actor, @"io.github.sadiksaifi.spaceterm") ||
        signed_process(actor, @"io.github.sadiksaifi.spaceterm-preflight") ||
        signed_process(actor, @"io.github.sadiksaifi.spaceterm-development")) classification = ST_RESPONSIBLE_DESKTOP;
    else if (signed_process(actor, @"io.github.sadiksaifi.spaceterm.server") ||
        signed_process(actor, @"io.github.sadiksaifi.spaceterm-preflight.server") ||
        signed_process(actor, @"io.github.sadiksaifi.spaceterm-development.server") ||
        signed_process(actor, @"io.github.sadiksaifi.spaceterm-development.server.native-evidence.server")) classification = ST_RESPONSIBLE_SERVER;
    if (responsible(getpid()) != actor ||
        !st_capability_process_observe(actor, &actor_after) ||
        !st_capability_process_observe(getpid(), &effect_after) ||
        getaudit_addr(&session_after, sizeof(session_after)) != 0 ||
        effect_before.uniqueid != effect_after.uniqueid || effect_before.pidversion != effect_after.pidversion ||
        actor_before.uniqueid != actor_after.uniqueid || actor_before.pidversion != actor_after.pidversion ||
        session_before.ai_asid != session_after.ai_asid || session_before.ai_auid != session_after.ai_auid)
        return ST_RESPONSIBLE_UNKNOWN;
    if (binding) *binding = (responsible_binding){effect_after, actor_after,
        session_after.ai_asid, session_after.ai_auid};
    return classification;
}
uint32_t st_capability_responsible_process(void) { return observe_responsible(NULL); }
static int same_responsible(const responsible_binding *a, const responsible_binding *b) {
    return a->effect.uniqueid == b->effect.uniqueid && a->effect.pidversion == b->effect.pidversion &&
        a->actor.uniqueid == b->actor.uniqueid && a->actor.pidversion == b->actor.pidversion &&
        a->session == b->session && a->account == b->account;
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
    /* Only SSH needs its inherited socket selector. The local shell comparison
     * never re-execs and never puts PATH or locale values in a child environment. */
    char **result = calloc(3, sizeof(char *));
    if (!result) return NULL;
    size_t used = 0;
    if (capability == ST_CAP_SSH_AGENT) {
        const char *value = getenv("SSH_AUTH_SOCK");
        if (value && (strlen(value) > 4096 || asprintf(&result[used], "SSH_AUTH_SOCK=%s", value) < 0)) {
            free(result); return NULL;
        }
        if (value) ++used;
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
            /* The worker is blocked on its configuration pipe. Retain its
             * uniqueid before either fixed sudo command may fork. This outer
             * scope survives worker timeout or cancellation during inner cleanup. */
            st_capability_process_identity identity;
            tree_child = child;
            tree_root = st_capability_process_observe(child, &identity) ? identity.uniqueid : 0;
            ready = tree_root && st_capability_begin_foreground(child);
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
    NSString *title = @"SpaceTerm Evidence Targets";
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
            /* The retained target deliberately never stores or echoes its value.
             * Observe the set operation, then require its nonce-bound receipt. */
            return AXUIElementSetAttributeValue(element, kAXValueAttribute, (__bridge CFStringRef)value) == kAXErrorSuccess;
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
static st_native_result target_receipt(uint32_t capability, uint64_t deadline);
static void accessibility(st_capability_observation *out, uint64_t deadline) {
    if (!companion()) { unavailable(out, 1); return; }
    if (!AXIsProcessTrusted()) { out->preflight_class = ST_PREFLIGHT_DENIED; out->outcome = ST_OUTCOME_DENIED; return; }
    out->preflight_class = ST_PREFLIGHT_ALLOWED;
    AXUIElementRef application = AXUIElementCreateApplication((pid_t)bound_targets.companion_pid);
    AXUIElementSetMessagingTimeout(application, 0.1f);
    NSString *identifier = @ST_COMPANION_ACCESSIBILITY_IDENTIFIER;
    char marker[ST_COMPANION_MARKER_LENGTH + 1];
    if (st_capability_target_marker(bound_targets.target_nonce, marker) != ST_PROBE_OK) { unavailable(out, 1); return; }
    NSString *value = [NSString stringWithUTF8String:marker];
    unsigned visited = 0;
    if (synthetic_control(application, identifier, value, 0, &visited) &&
        target_receipt(ST_CAP_ACCESSIBILITY, deadline) == ST_PROBE_OK) verified(out);
    else out->outcome = ST_OUTCOME_FAILED;
    CFRelease(application);
}

static OSStatus event_receipt(uint64_t deadline) {
    pid_t pid = (pid_t)bound_targets.companion_pid;
    AEAddressDesc address = {typeNull, NULL};
    AppleEvent event = {typeNull, NULL}, reply = {typeNull, NULL};
    OSStatus status = AECreateDesc(typeKernelProcessID, &pid, sizeof(pid), &address);
    if (status == noErr) status = AEDeterminePermissionToAutomateTarget(&address,
        ST_COMPANION_EVENT_CLASS, ST_COMPANION_EVENT_ID, false);
    if (status == noErr) status = AECreateAppleEvent(ST_COMPANION_EVENT_CLASS, ST_COMPANION_EVENT_ID,
        &address, kAutoGenerateReturnID, kAnyTransactionID, &event);
    const char *run = run_hex().UTF8String, *nonce = target_hex().UTF8String;
    if (status == noErr) status = AEPutParamPtr(&event, ST_COMPANION_EVENT_RUN, typeUTF8Text, run, 32);
    if (status == noErr) status = AEPutParamPtr(&event, ST_COMPANION_EVENT_NONCE, typeUTF8Text, nonce, 32);
    if (status == noErr && !expired(deadline)) {
        uint64_t remaining = deadline - st_capability_monotonic_ns();
        SInt32 ticks = (SInt32)(remaining / UINT64_C(16666667));
        status = AESendMessage(&event, &reply, kAEWaitReply | kAENeverInteract, ticks > 0 ? ticks : 1);
    }
    if (status == noErr) {
        char response[33] = {0}; DescType type = typeNull; Size size = 0;
        status = AEGetParamPtr(&reply, keyDirectObject, typeUTF8Text, &type, response, sizeof(response), &size);
        if (status == noErr && (type != typeUTF8Text || size != 8 || memcmp(response, "accepted", 8)))
            status = errAEEventNotHandled;
        if (status == noErr) status = AEGetParamPtr(&reply, ST_COMPANION_EVENT_RUN, typeUTF8Text,
            &type, response, sizeof(response), &size);
        if (status == noErr && (type != typeUTF8Text || size != 32 || memcmp(response, run, 32))) status = errAEEventNotHandled;
        if (status == noErr) status = AEGetParamPtr(&reply, ST_COMPANION_EVENT_NONCE, typeUTF8Text,
            &type, response, sizeof(response), &size);
        if (status == noErr && (type != typeUTF8Text || size != 32 || memcmp(response, nonce, 32))) status = errAEEventNotHandled;
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
    char url_text[ST_COMPANION_URL_LENGTH + 1];
    if (st_capability_target_url(bound_targets.run_id, bound_targets.target_nonce, url_text) != ST_PROBE_OK) return ST_PROBE_INVALID;
    NSURL *url = [NSURL URLWithString:[NSString stringWithUTF8String:url_text]];
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
    OSStatus status = event_receipt(deadline);
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
/* Only the fixed signed evidence controller can supply a target receipt. Its
 * producer must consume the retained companion's accepted run/nonce record, not
 * synthesize an acknowledgement from dispatch or preflight. No private values
 * travel on this fixed metadata-only port. */
static st_native_result target_receipt(uint32_t capability, uint64_t deadline) {
    if (!companion() || (capability != ST_CAP_URL && capability != ST_CAP_ACCESSIBILITY)) return ST_PROBE_UNAVAILABLE;
    pid_t pid = (pid_t)bound_targets.controller_pid;
    st_capability_process_identity before, after;
    if (pid <= 0 || pid == getpid() || pid == (pid_t)bound_targets.companion_pid ||
        !signed_process(pid, @ST_CAPABILITY_CONTROLLER_IDENTIFIER) ||
        !st_capability_process_observe(pid, &before)) return ST_PROBE_UNAVAILABLE;
    char path[sizeof(((struct sockaddr_un *)0)->sun_path)];
    int length = snprintf(path, sizeof(path), "/private/tmp/spaceterm-native-520-%s.%u.receipt.sock", run_hex().UTF8String, capability);
    if (length < 0 || (size_t)length >= sizeof(path)) return ST_PROBE_UNAVAILABLE;
    int fd = agent_socket(path, deadline);
    if (fd < 0) return expired(deadline) ? ST_PROBE_TIMED_OUT : ST_PROBE_UNAVAILABLE;
    pid_t peer = -1; socklen_t size = sizeof(peer);
    uint8_t request[44] = {'S', 'C', 'T', 'R', 0, 0, 0, 1}, reply[45] = {0};
    request[11] = (uint8_t)capability;
    memcpy(request + 12, bound_targets.run_id, 16); memcpy(request + 28, bound_targets.target_nonce, 16);
    int matched = getsockopt(fd, SOL_LOCAL, LOCAL_PEERPID, &peer, &size) == 0 &&
        size == sizeof(peer) && peer == pid && st_capability_transfer(fd, request, sizeof(request), 1, deadline) &&
        shutdown(fd, SHUT_WR) == 0 && st_capability_transfer(fd, reply, sizeof(request), 0, deadline) &&
        memcmp(reply, request, sizeof(request)) == 0 && st_capability_wait_fd(fd, POLLIN, deadline) &&
        read(fd, reply + sizeof(request), 1) == 0 &&
        st_capability_process_observe(pid, &after) &&
        before.uniqueid == after.uniqueid && before.pidversion == after.pidversion &&
        signed_process(pid, @ST_CAPABILITY_CONTROLLER_IDENTIFIER);
    size = sizeof(peer);
    matched = matched && getsockopt(fd, SOL_LOCAL, LOCAL_PEERPID, &peer, &size) == 0 && size == sizeof(peer) && peer == pid;
    st_capability_discard_owned(reply, sizeof(reply)); close(fd);
    if (matched && !expired(deadline)) return ST_PROBE_OK;
    return expired(deadline) ? ST_PROBE_TIMED_OUT : ST_PROBE_UNAVAILABLE;
}
st_native_result st_capability_url_receipt(uint64_t deadline) { return target_receipt(ST_CAP_URL, deadline); }

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
static int native_agent_session(void *unused, st_capability_agent_session *out) {
    (void)unused;
    memset(out, 0, sizeof(*out));
    uid_t uid = getuid();
    if (!uid || geteuid() != uid) return 0;
    ODNode *node = [ODNode nodeWithSession:[ODSession defaultSession] type:kODNodeTypeAuthentication error:NULL];
    if (!node) return 0;
    NSArray *attributes = @[kODAttributeTypeGUID, kODAttributeTypeNFSHomeDirectory];
    NSString *number = [NSString stringWithFormat:@"%u", uid];
    ODQuery *query = [ODQuery queryWithNode:node forRecordTypes:kODRecordTypeUsers
        attribute:kODAttributeTypeUniqueID matchType:kODMatchEqualTo queryValues:number
        returnAttributes:attributes maximumResults:2 error:NULL];
    NSArray *records = [query resultsAllowingPartial:NO error:NULL];
    if (records.count != 1) return 0;
    ODRecord *record = records[0];
    NSArray *generated = [record valuesForAttribute:kODAttributeTypeGUID error:NULL];
    NSArray *homes = [record valuesForAttribute:kODAttributeTypeNFSHomeDirectory error:NULL];
    if (generated.count != 1 || homes.count != 1 || ![generated[0] isKindOfClass:[NSString class]] ||
        ![homes[0] isKindOfClass:[NSString class]]) return 0;
    NSUUID *uuid = [[NSUUID alloc] initWithUUIDString:generated[0]];
    if (!uuid || ![homes[0] getCString:out->home maxLength:sizeof(out->home) encoding:NSUTF8StringEncoding] ||
        out->home[0] != '/') return 0;
    [uuid getUUIDBytes:out->account];
    ODQuery *unique = [ODQuery queryWithNode:node forRecordTypes:kODRecordTypeUsers
        attribute:kODAttributeTypeGUID matchType:kODMatchEqualTo queryValues:generated[0]
        returnAttributes:@[kODAttributeTypeGUID] maximumResults:2 error:NULL];
    if ([unique resultsAllowingPartial:NO error:NULL].count != 1) return 0;
    int okay = 1;
    SCDynamicStoreRef store = SCDynamicStoreCreate(NULL, CFSTR("SpaceTerm narrow agent observation"), NULL, NULL);
    CFDictionaryRef console = store ? SCDynamicStoreCopyValue(store, CFSTR("State:/Users/ConsoleUser")) : NULL;
    CFArrayRef sessions = console ? CFDictionaryGetValue(console, CFSTR("SessionInfo")) : NULL;
    uint32_t asid = 0;
    if (sessions && CFGetTypeID(sessions) == CFArrayGetTypeID() && CFArrayGetCount(sessions) <= 64) {
        for (CFIndex i = 0; i < CFArrayGetCount(sessions); ++i) {
            CFDictionaryRef entry = CFArrayGetValueAtIndex(sessions, i);
            if (CFGetTypeID(entry) != CFDictionaryGetTypeID()) continue;
            CFNumberRef user = CFDictionaryGetValue(entry, CFSTR("kCGSSessionUserIDKey"));
            CFNumberRef audit = CFDictionaryGetValue(entry, CFSTR("kCGSSessionAuditIDKey"));
            CFBooleanRef active = CFDictionaryGetValue(entry, CFSTR("kCGSSessionOnConsoleKey"));
            uint32_t observed_uid = UINT32_MAX, observed_asid = 0;
            if (user && audit && active && CFGetTypeID(user) == CFNumberGetTypeID() &&
                CFGetTypeID(audit) == CFNumberGetTypeID() && CFGetTypeID(active) == CFBooleanGetTypeID() &&
                CFNumberGetValue(user, kCFNumberSInt32Type, &observed_uid) && observed_uid == uid &&
                CFBooleanGetValue(active) && CFNumberGetValue(audit, kCFNumberSInt32Type, &observed_asid)) {
                if (asid) { okay = 0; break; } asid = observed_asid;
            }
        }
    }
    if (console) CFRelease(console); if (store) CFRelease(store);
    uint32_t actual = 0, attributes_bits = 0;
    if (!okay || !asid || SessionGetInfo(asid, &actual, &attributes_bits) != 0 || actual != asid ||
        !(attributes_bits & sessionHasGraphicAccess) || (attributes_bits & sessionIsRoot)) return 0;
    uint64_t window = 0; pid_t pids[8193];
    int count = proc_listallpids(pids, sizeof(pids));
    if (count <= 0 || count >= 8193) return 0;
    for (int i = 0; i < count; ++i) {
        struct proc_bsdshortinfo info; char path[4096]; st_capability_process_identity identity;
        if (pids[i] <= 0 || proc_pidinfo(pids[i], PROC_PIDT_SHORTBSDINFO, 0, &info, sizeof(info)) != sizeof(info) ||
            info.pbsi_uid != uid || proc_pidpath(pids[i], path, sizeof(path)) <= 0 ||
            strcmp(path, "/System/Library/CoreServices/loginwindow.app/Contents/MacOS/loginwindow")) continue;
        if (window || !st_capability_process_observe(pids[i], &identity)) return 0;
        window = identity.uniqueid;
    }
    char boot[64] = {0}; size_t length = sizeof(boot);
    if (!window || sysctlbyname("kern.bootsessionuuid", boot, &length, NULL, 0) != 0 || length > sizeof(boot) || length < 2) return 0;
    uint8_t input[sizeof(boot) + sizeof(asid) + sizeof(window)], digest[CC_SHA256_DIGEST_LENGTH];
    memcpy(input, boot, sizeof(boot)); memcpy(input + sizeof(boot), &asid, sizeof(asid));
    memcpy(input + sizeof(boot) + sizeof(asid), &window, sizeof(window));
    CC_SHA256(input, sizeof(input), digest); memcpy(out->session, digest, 16); out->uid = uid;
    st_capability_discard_owned(input, sizeof(input)); st_capability_discard_owned(digest, sizeof(digest));
    return 1;
}
int st_capability_native_agent_session(st_capability_agent_session *out) {
    return out && native_agent_session(NULL, out);
}
static void gpg_agent(st_capability_observation *out, uint64_t deadline) {
    st_capability_agent_peer peer = {NULL, native_agent_session};
    (void)st_capability_gpg_agent(&peer, deadline, out);
}

static int clipboard_count(void *context, uint64_t *out) {
    NSPasteboard *board = (__bridge NSPasteboard *)context;
    NSInteger count = board.changeCount;
    if (count < 0) return 0;
    *out = (uint64_t)count; return 1;
}
int st_capability_clipboard_text(void *context, uint8_t *out, size_t capacity, size_t *length) {
    (void)context; (void)out; (void)capacity; (void)length;
    /* Darwin's RLIMIT_AS rejects mmap but does not bound Mach out-of-line copyout.
     * AppKit exposes no size-before-acquisition or bounded pasteboard read. A
     * 64-MiB native IPC regression crosses the proposed 16-MiB cap, so neither that
     * cap nor a post-acquisition NSData length check can authorize this request.
     * Refuse before dataForType. The case emits Unavailable, never read evidence.
     * The injected bounded clipboard peer remains read-only and content-free. */
    return 0;
}
static uint64_t clipboard_now(void *unused) { (void)unused; return st_capability_monotonic_ns(); }
static void clipboard(st_capability_observation *out, uint64_t deadline) {
    st_capability_agent_session before = {0}, after = {0};
    if (!native_agent_session(NULL, &before)) { unavailable(out, 1); return; }
    NSPasteboard *board = [NSPasteboard generalPasteboard];
    st_capability_clipboard_peer peer = {(__bridge void *)board, clipboard_count, st_capability_clipboard_text, clipboard_now};
    st_capability_clipboard_read(&peer, bound_targets.target_nonce, deadline, out);
    if (!native_agent_session(NULL, &after) || memcmp(&before, &after, sizeof(before))) {
        unavailable(out, 0); out->effect_class = ST_EFFECT_UNOBSERVED;
    }
    st_capability_discard_owned(&before, sizeof(before)); st_capability_discard_owned(&after, sizeof(after));
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
/* Both commands are prepared before fork. The child performs only async-signal-
 * safe operations before exec and cannot run until its uniqueid is retained.
 * Each sudo root gets its own session/group. The outer worker scope additionally
 * catches interruption before the inner scope's sentinel or final scan. */
static int fixed_sudo(int tty, int invalidate, uint64_t deadline, int *status, int *clean) {
    st_capability_terminal terminal;
    if (!st_capability_terminal_prepare(tty, &terminal)) return 0;
    int gate[2];
    if (pipe(gate) != 0) { st_capability_terminal_finish(&terminal); return 0; }
    fcntl(gate[0], F_SETFD, FD_CLOEXEC); fcntl(gate[1], F_SETFD, FD_CLOEXEC);
    char *reset[] = {"/usr/bin/sudo", "-k", NULL};
    char *environment[] = {"PATH=/usr/bin:/bin:/usr/sbin:/sbin", "LC_ALL=C", NULL};
    char *const *arguments = invalidate ? reset : (char *const *)st_capability_sudo_argv();
    pid_t child = fork();
    if (!child) {
        close(gate[1]);
        char byte = 0; ssize_t received;
        do { received = read(gate[0], &byte, 1); } while (received < 0 && errno == EINTR);
        close(gate[0]);
        if (received != 1 || byte != 'g' || !st_capability_terminal_child_setup(&terminal)) _exit(125);
        execve("/usr/bin/sudo", arguments, environment); _exit(126);
    }
    close(gate[0]);
    if (child < 0) { close(gate[1]); st_capability_terminal_finish(&terminal); return 0; }
    st_capability_process_identity root;
    int observed = st_capability_process_observe(child, &root);
    char byte = 'g';
    int started = observed && st_capability_transfer(gate[1], &byte, 1, 1, deadline);
    close(gate[1]);
    int completed = st_capability_terminal_wait(&terminal, child, deadline, status);
    int restored = st_capability_terminal_finish(&terminal);
    *clean = completed && observed && restored && st_capability_tree_retire_native(root.uniqueid) == ST_PROBE_OK;
    return started && completed;
}
static void touch_id(st_capability_observation *out, uint64_t deadline) {
    int tty = open("/dev/tty", O_RDWR | O_CLOEXEC | O_NOCTTY);
    if (tty < 0) { unavailable(out, 1); return; }
    out->preflight_class = ST_PREFLIGHT_ALLOWED;
    int status = 0, clean = 0;
    int completed = fixed_sudo(tty, 1, deadline, &status, &clean);
    if (completed && clean && WIFEXITED(status) && WEXITSTATUS(status) == 0)
        completed = fixed_sudo(tty, 0, deadline, &status, &clean);
    close(tty);
    if (!clean) {
        out->cleanup_class = ST_CLEANUP_UNRESOLVED;
        out->outcome = ST_OUTCOME_CLEANUP_UNRESOLVED; return;
    }
    out->cleanup_class = ST_CLEANUP_VERIFIED;
    if (!completed) out->outcome = ST_OUTCOME_TIMED_OUT;
    else if (WIFEXITED(status) && WEXITSTATUS(status) == 0) {
        out->effect_class = ST_EFFECT_COMPLETED_UNATTRIBUTED;
        out->outcome = ST_OUTCOME_UNAVAILABLE; /* Completion alone cannot prove Touch ID rather than password. */
    } else if (WIFSIGNALED(status) && (WTERMSIG(status) == SIGINT || WTERMSIG(status) == SIGTERM))
        out->outcome = ST_OUTCOME_CANCELLED;
    else out->outcome = ST_OUTCOME_UNAVAILABLE;
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

st_native_result st_capability_bind_shell_mode(int fd, uint64_t deadline) {
    if (shell_mode_bound) return ST_PROBE_INVALID;
    st_native_result result = st_capability_read_shell_mode(fd, deadline, &shell_mode);
    if (result == ST_PROBE_OK) shell_mode_bound = 1;
    return result;
}

st_native_result st_capability_observe_environment(st_probe_context context,
    st_capability_environment_difference *out) {
    if (!out || getuid() == 0 || geteuid() != getuid() ||
        !signed_process(getpid(), @ST_CAPABILITY_IDENTIFIER) || !observed_context(context))
        return ST_PROBE_UNAVAILABLE;
    pid_t shell = getppid();
    return st_capability_observe_shell_environment(shell, context, out);
}
st_native_result st_capability_observe_shell_environment(pid_t shell, st_probe_context context,
    st_capability_environment_difference *out) {
    /* Mode is normalized by the originating shell. KERN_PROCARGS2 includes that
     * process's private initial environment even after it unsets a value. Never
     * request another process's argv/environment block to recover two flags. */
    if (!out || context < 1 || context > 2 || !shell_mode_bound || shell_mode != 3) return ST_PROBE_UNAVAILABLE;
    struct passwd *account = getpwuid(getuid());
    if (!account || !account->pw_shell || (strcmp(account->pw_shell, "/bin/zsh") && strcmp(account->pw_shell, "/bin/bash")))
        return ST_PROBE_UNAVAILABLE;
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
    if (proc_pidinfo(shell, PROC_PIDTBSDINFO, 0, &after, sizeof(after)) != sizeof(after) ||
        before.pbi_start_tvsec != after.pbi_start_tvsec || before.pbi_start_tvusec != after.pbi_start_tvusec)
        return ST_PROBE_UNAVAILABLE;
    const char *locales[ST_CAPABILITY_LOCALE_COUNT];
    for (size_t i = 0; i < ST_CAPABILITY_LOCALE_COUNT; ++i) locales[i] = getenv(st_capability_locale_names[i]);
    return st_capability_environment_compare_local(context, getenv("PATH"), locales, out);
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
        responsible_binding before = {0}, after = {0};
        out->responsible_process = observe_responsible(&before);
        switch (capability) {
            case ST_CAP_PROTECTED_FOLDERS: folders(out); break;
            case ST_CAP_SELECTED_VOLUME: volume(out); break;
            case ST_CAP_MICROPHONE: microphone(out, deadline); break;
            case ST_CAP_SCREEN_RECORDING: screen(out, deadline); break;
            case ST_CAP_ACCESSIBILITY: accessibility(out, deadline); break;
            case ST_CAP_CLIPBOARD: clipboard(out, deadline); break;
            case ST_CAP_URL: st_capability_events(out, deadline, 1); break;
            case ST_CAP_APPLE_EVENTS: st_capability_events(out, deadline, 0); break;
            case ST_CAP_LOGIN_KEYCHAIN: login_keychain(out); break;
            case ST_CAP_SSH_AGENT: ssh_agent(out, deadline); break;
            case ST_CAP_GPG_AGENT: gpg_agent(out, deadline); break;
            case ST_CAP_TOUCH_ID_SUDO: touch_id(out, deadline); break;
            case ST_CAP_LOGIN_SHELL: {
                st_capability_environment_difference environment;
                if (st_capability_observe_environment(context, &environment) == ST_PROBE_OK) verified(out);
                else unavailable(out, 1);
                break;
            }
        }
        if (observe_responsible(&after) == ST_RESPONSIBLE_UNKNOWN || !same_responsible(&before, &after))
            out->responsible_process = ST_RESPONSIBLE_UNKNOWN;
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
