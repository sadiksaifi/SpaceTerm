// A real application bundle used only by the isolated Sparkle integration test.
#import <AppKit/AppKit.h>
#import "../src/platform/macos_updater.h"

static void *session;
static NSString *mode;
static NSString *logFile;

static void record(NSString *message) {
    NSFileHandle *file = [NSFileHandle fileHandleForWritingAtPath:logFile];
    [file seekToEndOfFile];
    [file writeData:[[message stringByAppendingString:@"\n"] dataUsingEncoding:NSUTF8StringEncoding]];
    [file closeFile];
}
static bool validate(const char *version, const char *display, const char *url, bool informational) {
    return !informational && strcmp(version, "0.1.1") == 0 && strcmp(display, "0.1.1") == 0 &&
        [@(url) hasPrefix:@"http://127.0.0.1:"];
}
static void event(void *context, uint32_t kind, const char *version, uint64_t first, uint64_t second) {
    record([NSString stringWithFormat:@"event:%u:%llu", kind, first]);
    if (kind == 1) dispatch_async(dispatch_get_main_queue(), ^{ spt_updater_download(session); });
    if (kind == 5) {
        if ([mode isEqualToString:@"install"]) {
            record(@"confirmed");
            dispatch_async(dispatch_get_main_queue(), ^{ spt_updater_install(session); });
        } else {
            record(@"cancelled");
            dispatch_async(dispatch_get_main_queue(), ^{ spt_updater_cancel(session); });
        }
    }
    if (kind == 8 && ![mode isEqualToString:@"install"]) {
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
        session = spt_updater_create(NULL, event, validate);
        if (!session) { record(@"startup-failed"); return 2; }
        dispatch_async(dispatch_get_main_queue(), ^{ spt_updater_check(session); });
        [NSApp run];
        spt_updater_destroy(session);
        return 0;
    }
}
