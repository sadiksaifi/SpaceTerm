#import "companion_app.h"

#include <string.h>

/* Entry point of the temporary evidence companion. The evidence controller
 * launches it through LaunchServices with --run-id and --controller-socket.
 * Anything else exits with EX_USAGE before any window appears. */

enum { ST_COMPANION_EXIT_USAGE = 64 };

@interface STCompanionApplicationDelegate : NSObject <NSApplicationDelegate>
- (instancetype)initWithRun:(const st_companion_id *)run socketPath:(NSString *)path;
@end

@implementation STCompanionApplicationDelegate {
    st_companion_id _run;
    NSString *_socketPath;
    STCompanion *_companion;
}

- (instancetype)initWithRun:(const st_companion_id *)run socketPath:(NSString *)path
{
    self = [super init];
    if (self == nil) return nil;
    _run = *run;
    _socketPath = [path copy];
    return self;
}

- (void)applicationWillFinishLaunching:(NSNotification *)notification
{
    (void)notification;
    _companion = [[STCompanion alloc] initWithRun:&_run ports:[STCompanionPorts productionPorts]];
    NSApp.mainMenu = [_companion mainMenu];
    NSAppleEventManager *events = NSAppleEventManager.sharedAppleEventManager;
    [events setEventHandler:_companion
                andSelector:@selector(handleURLEvent:withReplyEvent:)
              forEventClass:kInternetEventClass
                 andEventID:kAEGetURL];
    [events setEventHandler:_companion
                andSelector:@selector(handleReceiptEvent:withReplyEvent:)
              forEventClass:ST_COMPANION_EVENT_CLASS
                 andEventID:ST_COMPANION_EVENT_ID];
}

- (void)applicationDidFinishLaunching:(NSNotification *)notification
{
    (void)notification;
    [_companion.interface.mainWindow center];
    [_companion.interface.mainWindow makeKeyAndOrderFront:nil];
    [NSApp activate];
    STSocketChannel *channel = [STSocketChannel connectToPath:_socketPath delegate:_companion];
    if (channel == nil) {
        [_companion connectionFailedWithState:ST_COMPANION_RUN_REJECTED];
        return;
    }
    [_companion attachChannel:channel];
}

- (NSApplicationTerminateReply)applicationShouldTerminate:(NSApplication *)sender
{
    return [_companion applicationShouldTerminate:sender];
}

- (BOOL)applicationShouldTerminateAfterLastWindowClosed:(NSApplication *)sender
{
    (void)sender;
    return NO;
}

- (BOOL)applicationShouldHandleReopen:(NSApplication *)sender hasVisibleWindows:(BOOL)visible
{
    (void)sender;
    if (!visible) [_companion showCompanion:nil];
    return YES;
}

- (BOOL)applicationSupportsSecureRestorableState:(NSApplication *)app
{
    (void)app;
    return YES;
}
@end

static int usage(void)
{
    fputs("usage: spaceterm-evidence-companion --run-id <id> --controller-socket <path>\n",
          stderr);
    return ST_COMPANION_EXIT_USAGE;
}

int main(int argc, const char *argv[])
{
    @autoreleasepool {
        const char *run_text = NULL;
        const char *socket_path = NULL;
        for (int i = 1; i < argc; ++i) {
            /* LaunchServices may add a process serial number argument. */
            if (strncmp(argv[i], "-psn_", 5) == 0) continue;
            if (i + 1 < argc && strcmp(argv[i], "--run-id") == 0 && run_text == NULL) {
                run_text = argv[++i];
            } else if (i + 1 < argc && strcmp(argv[i], "--controller-socket") == 0 &&
                       socket_path == NULL) {
                socket_path = argv[++i];
            } else {
                return usage();
            }
        }
        st_companion_id run;
        if (run_text == NULL || socket_path == NULL ||
            !st_companion_id_parse(run_text, strlen(run_text), &run) || socket_path[0] != '/') {
            return usage();
        }
        NSString *path = [NSFileManager.defaultManager
            stringWithFileSystemRepresentation:socket_path
                                        length:strlen(socket_path)];
        if (path.length == 0) return usage();

        NSApplication *application = NSApplication.sharedApplication;
        application.activationPolicy = NSApplicationActivationPolicyRegular;
        STCompanionApplicationDelegate *delegate =
            [[STCompanionApplicationDelegate alloc] initWithRun:&run socketPath:path];
        application.delegate = delegate;
        [application run];
    }
    return 0;
}
