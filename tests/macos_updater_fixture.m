// A real application bundle used only by the isolated Sparkle integration test.
#import <AppKit/AppKit.h>
#import "../src/platform/macos_updater.h"

static void *session;
static NSString *mode;
static NSString *logFile;

// SpaceTerm's quit policy (ADR 0049) and its relaunch quit, compiled from
// src/platform/quit_policy.rs through tests/macos_updater_fixture_policy.rs. The fixture only
// performs each action the policy returns, as SpaceTerm's quit coordinator does.
uint8_t spt_fixture_quit_request(uint32_t event_class, uint32_t event_id, bool has_reason,
                                 bool in_event, uint64_t approval);
uint8_t spt_fixture_settings_saved(bool saved, uint64_t approval);
uint8_t spt_fixture_relaunch_released(bool updater_quits, uint64_t approval);
enum { SPTProceed = 0, SPTNothing, SPTSaveSettings, SPTRelease, SPTQuit, SPTFailUnsaved,
       SPTDismiss, SPTExit };
/// The approved relaunch as SpaceTerm's update service holds it, or 0 for none.
static uint64_t approval;
/// The fixture confirmed its own quit, as SpaceTerm's confirm_quit does.
static BOOL confirmed;

static void record(NSString *message) {
    NSFileHandle *file = [NSFileHandle fileHandleForWritingAtPath:logFile];
    [file seekToEndOfFile];
    [file writeData:[[message stringByAppendingString:@"\n"] dataUsingEncoding:NSUTF8StringEncoding]];
    [file closeFile];
}
static NSApplicationTerminateReply perform(uint8_t action) {
    switch (action) {
    case SPTProceed:
        return NSTerminateNow;
    case SPTSaveSettings:
        // The relaunch-failed mode's Settings can't be saved.
        dispatch_async(dispatch_get_main_queue(), ^{
            perform(spt_fixture_settings_saved(![mode isEqualToString:@"relaunch-failed"], approval));
        });
        return NSTerminateCancel;
    case SPTRelease:
        dispatch_async(dispatch_get_main_queue(), ^{
            perform(spt_fixture_relaunch_released(spt_updater_release(session), approval));
        });
        return NSTerminateCancel;
    case SPTQuit:
        record(@"relaunch-quit");
        dispatch_async(dispatch_get_main_queue(), ^{ confirmed = YES; [NSApp terminate:nil]; });
        return NSTerminateCancel;
    case SPTFailUnsaved:
        // The update service fails the relaunch, then the person quits SpaceTerm.
        record(@"save-failed");
        approval = 0;
        dispatch_async(dispatch_get_main_queue(), ^{ [NSApp terminate:nil]; });
        return NSTerminateCancel;
    case SPTDismiss:
        record(@"dismissed");
        return NSTerminateCancel;
    case SPTExit:
        // The person confirms the quit warning.
        record(@"exit");
        return NSTerminateNow;
    default:
        return NSTerminateCancel;
    }
}
@interface SPTFixtureDelegate : NSObject <NSApplicationDelegate>
@end
@implementation SPTFixtureDelegate
- (NSApplicationTerminateReply)applicationShouldTerminate:(NSApplication *)sender {
    if (confirmed) return NSTerminateNow;
    NSAppleEventDescriptor *event = NSAppleEventManager.sharedAppleEventManager.currentAppleEvent;
    return perform(spt_fixture_quit_request(event.eventClass, event.eventID,
        [event paramDescriptorForKeyword:'why?'] != nil, event != nil, approval));
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
            approval = 1;
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
