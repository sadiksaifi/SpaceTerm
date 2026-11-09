// A real application bundle used only by the isolated Sparkle integration test.
#import <AppKit/AppKit.h>
#import "../src/platform/macos_updater.h"

static void *session;
static NSString *mode;
static NSString *logFile;

// SpaceTerm's quit policy (ADR 0049), compiled from src/platform/quit_policy.rs. The fixture
// keeps only what the app acts on: its relaunch phase, the Settings save and the quit itself.
uint8_t spt_quit_request(uint32_t event_class, uint32_t event_id, bool has_reason, bool in_event);
uint8_t spt_quit_step(uint8_t request, uint8_t relaunch);
enum { SPTRelaunchNone = 0, SPTRelaunchApproved, SPTRelaunchQuitting, SPTRelaunchReleased };
enum { SPTStepProceed = 0, SPTStepBeginRelaunch, SPTStepWait, SPTStepQuitForRelaunch,
       SPTStepDismiss, SPTStepExit };
static uint8_t relaunch = SPTRelaunchNone;
/// The fixture confirmed its own quit, as SpaceTerm's confirm_quit does.
static BOOL confirmed;

static void record(NSString *message) {
    NSFileHandle *file = [NSFileHandle fileHandleForWritingAtPath:logFile];
    [file seekToEndOfFile];
    [file writeData:[[message stringByAppendingString:@"\n"] dataUsingEncoding:NSUTF8StringEncoding]];
    [file closeFile];
}
static void confirmQuit(void) {
    dispatch_async(dispatch_get_main_queue(), ^{ confirmed = YES; [NSApp terminate:nil]; });
}
// SpaceTerm's Settings save before a relaunch. The relaunch-failed mode fails it, so the relaunch
// fails at once and the person then quits SpaceTerm ordinarily.
static void saveSettingsForRelaunch(void) {
    dispatch_async(dispatch_get_main_queue(), ^{
        if ([mode isEqualToString:@"relaunch-failed"]) {
            record(@"save-failed");
            relaunch = SPTRelaunchNone;
            dispatch_async(dispatch_get_main_queue(), ^{ [NSApp terminate:nil]; });
            return;
        }
        relaunch = spt_updater_release(session) ? SPTRelaunchReleased : SPTRelaunchNone;
    });
}
@interface SPTFixtureDelegate : NSObject <NSApplicationDelegate>
@end
@implementation SPTFixtureDelegate
- (NSApplicationTerminateReply)applicationShouldTerminate:(NSApplication *)sender {
    if (confirmed) return NSTerminateNow;
    NSAppleEventDescriptor *event = NSAppleEventManager.sharedAppleEventManager.currentAppleEvent;
    uint8_t request = spt_quit_request(event.eventClass, event.eventID,
        [event paramDescriptorForKeyword:'why?'] != nil, event != nil);
    switch (spt_quit_step(request, relaunch)) {
    case SPTStepProceed:
        return NSTerminateNow;
    case SPTStepBeginRelaunch:
        relaunch = SPTRelaunchQuitting;
        saveSettingsForRelaunch();
        return NSTerminateCancel;
    case SPTStepQuitForRelaunch:
        record(@"relaunch-quit");
        relaunch = SPTRelaunchQuitting;
        confirmQuit();
        return NSTerminateCancel;
    case SPTStepDismiss:
        record(@"dismissed");
        return NSTerminateCancel;
    case SPTStepExit:
        // The person confirms the quit warning.
        record(@"exit");
        return NSTerminateNow;
    default:
        return NSTerminateCancel;
    }
}
@end

static bool validate(const char *version, const char *display, const char *url, bool informational) {
    return !informational && strcmp(version, "0.1.1") == 0 && strcmp(display, "0.1.1") == 0 &&
        [@(url) hasPrefix:@"http://127.0.0.1:"];
}
static void event(void *context, uint32_t kind, const char *version, uint64_t first, uint64_t second) {
    record([NSString stringWithFormat:@"event:%u:%llu", kind, first]);
    if (kind == 1) dispatch_async(dispatch_get_main_queue(), ^{ spt_updater_download(session); });
    if (kind == 5) {
        if ([mode isEqualToString:@"install"] || [mode isEqualToString:@"relaunch-failed"]) {
            // The person chose Relaunch: SpaceTerm installs, then asks to quit.
            record(@"confirmed");
            relaunch = SPTRelaunchApproved;
            dispatch_async(dispatch_get_main_queue(), ^{
                spt_updater_install(session);
                [NSApp terminate:nil];
            });
        } else if ([mode isEqualToString:@"quit"]) {
            record(@"normal-quit");
            dispatch_async(dispatch_get_main_queue(), ^{ spt_updater_finish_on_quit(session); });
        } else {
            record(@"cancelled");
            dispatch_async(dispatch_get_main_queue(), ^{ spt_updater_cancel(session); });
        }
    }
    if (kind == 8 && ![mode isEqualToString:@"install"] && ![mode isEqualToString:@"relaunch-failed"]) {
        dispatch_async(dispatch_get_main_queue(), ^{ [NSApp terminate:nil]; });
    }
}
int main(void) {
    @autoreleasepool {
        NSBundle *bundle = NSBundle.mainBundle;
        logFile = [bundle objectForInfoDictionaryKey:@"SPTTestLog"];
        mode = [bundle objectForInfoDictionaryKey:@"SPTTestMode"];
        if ([[bundle objectForInfoDictionaryKey:@"CFBundleVersion"] isEqualToString:@"0.1.1"]) {
            record(@"relaunched");
            return 0;
        }
        [NSApplication sharedApplication];
        [NSApp setActivationPolicy:NSApplicationActivationPolicyProhibited];
        SPTFixtureDelegate *delegate = [SPTFixtureDelegate new];
        NSApp.delegate = delegate;
        session = spt_updater_create(NULL, event, validate);
        if (!session) { record(@"startup-failed"); return 2; }
        dispatch_async(dispatch_get_main_queue(), ^{ spt_updater_check(session); });
        [NSApp run];
        spt_updater_destroy(session);
        return 0;
    }
}
