#import "companion_app.h"

#include <time.h>

/* Production ports. Interaction tests replace every one of these. */

@interface STAlertConfirmer : NSObject <STCompanionConfirmer>
@end

@implementation STAlertConfirmer

- (void)confirmWithTitle:(NSString *)title
                 message:(NSString *)message
            confirmTitle:(NSString *)confirmTitle
             destructive:(BOOL)destructive
                  window:(NSWindow *)window
              completion:(STCompanionDecision)completion
{
    NSAlert *alert = [[NSAlert alloc] init];
    alert.alertStyle = destructive ? NSAlertStyleCritical : NSAlertStyleWarning;
    alert.messageText = title;
    alert.informativeText = message;
    NSButton *confirm = [alert addButtonWithTitle:confirmTitle];
    NSButton *cancel = [alert addButtonWithTitle:@"Cancel"];
    cancel.keyEquivalent = @"\033";
    if (destructive) {
        /* Return must not confirm a destructive request by accident. */
        confirm.keyEquivalent = @"";
        confirm.hasDestructiveAction = YES;
    }
    void (^finish)(NSModalResponse) = ^(NSModalResponse response) {
        completion(response == NSAlertFirstButtonReturn);
    };
    if (window != nil) {
        if (!window.visible) [window makeKeyAndOrderFront:nil];
        [alert beginSheetModalForWindow:(NSWindow *)window completionHandler:finish];
    } else {
        finish([alert runModal]);
    }
}
@end

@interface STAccessibilityAnnouncer : NSObject <STCompanionAnnouncer>
@end

@implementation STAccessibilityAnnouncer
- (void)announce:(NSString *)text
{
    id element = NSApp.mainWindow != nil ? (id)NSApp.mainWindow : (id)NSApp;
    NSAccessibilityPostNotificationWithUserInfo(
        element, NSAccessibilityAnnouncementRequestedNotification, @{
            NSAccessibilityAnnouncementKey: text,
            NSAccessibilityPriorityKey: @(NSAccessibilityPriorityMedium),
        });
}
@end

@interface STWorkspaceSettingsOpener : NSObject <STCompanionSettingsOpener>
@end

@implementation STWorkspaceSettingsOpener
- (void)openSettingsURL:(NSURL *)url
{
    /* Open only a URL the allowlist itself would produce. */
    for (uint32_t code = 1; code <= ST_COMPANION_CAPABILITY_COUNT; ++code) {
        NSURL *allowed = STCompanionSettingsURL(st_companion_capability_find(code));
        if (allowed != nil && [allowed isEqual:url]) {
            [NSWorkspace.sharedWorkspace openURL:url];
            return;
        }
    }
}
@end

@interface STBackgroundExecutor : NSObject <STCompanionExecutor>
@end

@implementation STBackgroundExecutor {
    dispatch_queue_t _queue;
}

- (instancetype)init
{
    self = [super init];
    if (self == nil) return nil;
    _queue = dispatch_queue_create("spaceterm.evidence-companion.authorization",
                                   DISPATCH_QUEUE_SERIAL);
    return self;
}

- (void)perform:(dispatch_block_t)work then:(dispatch_block_t)completion
{
    dispatch_async(_queue, ^{
        work();
        dispatch_async(dispatch_get_main_queue(), completion);
    });
}
@end

@implementation STCompanionPorts (Production)
+ (instancetype)productionPorts
{
    STCompanionPorts *ports = [[STCompanionPorts alloc] init];
    ports.pasteboard = [[STGeneralPasteboard alloc] init];
    ports.confirmer = [[STAlertConfirmer alloc] init];
    ports.announcer = [[STAccessibilityAnnouncer alloc] init];
    ports.settingsOpener = [[STWorkspaceSettingsOpener alloc] init];
    ports.authorizationExecutor = [[STBackgroundExecutor alloc] init];
    ports.presenter = [[STWindowPresenter alloc] init];
    ports.clock = ^uint64_t(void) {
        return clock_gettime_nsec_np(CLOCK_UPTIME_RAW);
    };
    ports.quitReply = ^(BOOL quit) {
        [NSApp replyToApplicationShouldTerminate:quit];
    };
    /* No management adapter is linked yet. NULL selects the unavailable
     * port, which reports NativeUnavailable without any fallback. */
    ports.authorization = NULL;
    return ports;
}
@end
