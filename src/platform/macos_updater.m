#import <AppKit/AppKit.h>
#import <Sparkle/Sparkle.h>
#import "macos_updater.h"

// Only bounded classifications and a validated version cross into Rust. Native errors stay here.
enum { SPTAvailable = 1, SPTCurrent, SPTDownloading, SPTVerifying, SPTReady,
       SPTInstalling, SPTFailed, SPTFinished };
enum { SPTUnavailable = 0, SPTCheckError, SPTDownloadError, SPTVerificationError,
       SPTInstallationError, SPTReadOnlyError };

@interface SPTUpdateDriver : NSObject <SPUUserDriver, SPUUpdaterDelegate>
@property(nonatomic, assign) void *context;
@property(nonatomic, assign) SPTUpdateCallback callback;
@property(nonatomic, assign) SPTUpdateValidator validator;
@property(nonatomic, weak) SPUUpdater *updater;
@property(nonatomic, copy) void (^choice)(SPUUserUpdateChoice);
@property(nonatomic, copy) void (^cancellation)(void);
@property(nonatomic, copy) void (^retryTermination)(void);
@property(nonatomic) BOOL cancelling;
@property(nonatomic) BOOL ready;
@property(nonatomic) BOOL authorized;
@property(nonatomic) uint64_t received;
@property(nonatomic) uint64_t total;
@property(nonatomic) uint32_t phase;
@end

@implementation SPTUpdateDriver
- (void)emit:(uint32_t)event version:(NSString *)version first:(uint64_t)first second:(uint64_t)second {
    if (self.callback) self.callback(self.context, event, version.UTF8String, first, second);
}
- (void)emit:(uint32_t)event { [self emit:event version:nil first:0 second:0]; }
- (void)cancel {
    self.cancelling = YES;
    if (self.choice) {
        void (^reply)(SPUUserUpdateChoice) = self.choice;
        self.choice = nil;
        // Skip in the installing stage cancels the staged installer instead of installing on quit.
        reply(self.ready ? SPUUserUpdateChoiceSkip : SPUUserUpdateChoiceDismiss);
    } else if (self.cancellation) {
        void (^cancel)(void) = self.cancellation;
        self.cancellation = nil;
        cancel();
    }
    // Extraction cannot be cancelled. showReadyToInstallAndRelaunch settles the cancellation.
}
- (void)showUpdatePermissionRequest:(SPUUpdatePermissionRequest *)request reply:(void (^)(SUUpdatePermissionResponse *))reply {
    reply([[SUUpdatePermissionResponse alloc] initWithAutomaticUpdateChecks:NO sendSystemProfile:NO]);
}
- (void)showUserInitiatedUpdateCheckWithCancellation:(void (^)(void))cancellation {
    self.cancellation = cancellation;
    if (self.cancelling) [self cancel];
}
- (void)showUpdateFoundWithAppcastItem:(SUAppcastItem *)item state:(SPUUserUpdateState *)state reply:(void (^)(SPUUserUpdateChoice))reply {
    self.cancellation = nil;
    self.choice = reply;
    self.ready = state.stage == SPUUserUpdateStageInstalling;
    if (self.cancelling) { [self cancel]; return; }
    [self emit:SPTAvailable version:item.displayVersionString first:0 second:0];
    if (self.ready) [self emit:SPTReady];
}
- (void)showUpdateReleaseNotesWithDownloadData:(SPUDownloadData *)data {}
- (void)showUpdateReleaseNotesFailedToDownloadWithError:(NSError *)error {}
- (void)showUpdateNotFoundWithError:(NSError *)error acknowledgement:(void (^)(void))acknowledgement {
    [self emit:SPTCurrent];
    acknowledgement();
}
- (void)showUpdaterError:(NSError *)error acknowledgement:(void (^)(void))acknowledgement {
    // didAbortWithError reports one classified failure for both foreground and background checks.
    acknowledgement();
}
- (void)showDownloadInitiatedWithCancellation:(void (^)(void))cancellation {
    self.phase = SPTDownloadError;
    self.cancellation = cancellation;
    self.received = 0; self.total = 0;
    [self emit:SPTDownloading];
    if (self.cancelling) [self cancel];
}
- (void)showDownloadDidReceiveExpectedContentLength:(uint64_t)length { self.total = length; }
- (void)showDownloadDidReceiveDataOfLength:(uint64_t)length {
    self.received = UINT64_MAX - self.received < length ? UINT64_MAX : self.received + length;
    [self emit:SPTDownloading version:nil first:self.received second:self.total];
}
- (void)showDownloadDidStartExtractingUpdate {
    self.phase = SPTVerificationError;
    self.cancellation = nil;
    [self emit:SPTVerifying];
}
- (void)showExtractionReceivedProgress:(double)progress {}
- (void)showReadyToInstallAndRelaunch:(void (^)(SPUUserUpdateChoice))reply {
    self.phase = SPTInstallationError;
    self.ready = YES;
    self.choice = reply;
    if (self.cancelling) { [self cancel]; return; }
    [self emit:SPTReady];
}
- (void)showInstallingUpdateWithApplicationTerminated:(BOOL)terminated retryTerminatingApplication:(void (^)(void))retry {
    self.retryTermination = terminated ? nil : retry;
    [self emit:SPTInstalling];
}
- (void)showUpdateInstalledAndRelaunched:(BOOL)relaunched acknowledgement:(void (^)(void))acknowledgement { acknowledgement(); }
- (void)dismissUpdateInstallation {
    self.choice = nil; self.cancellation = nil; self.retryTermination = nil;
}
- (BOOL)updaterShouldPromptForPermissionToCheckForUpdates:(SPUUpdater *)updater { return NO; }
- (BOOL)updater:(SPUUpdater *)updater shouldDownloadReleaseNotesForUpdate:(SUAppcastItem *)item { return NO; }
- (BOOL)updater:(SPUUpdater *)updater shouldProceedWithUpdate:(SUAppcastItem *)item updateCheck:(SPUUpdateCheck)check error:(NSError * __autoreleasing *)error {
    NSString *version = item.versionString;
    NSString *display = item.displayVersionString;
    NSString *url = item.fileURL.absoluteString;
    BOOL valid = self.validator && version.length <= 48 && display.length <= 48 && url.length <= 512 &&
        [item.installationType isEqualToString:@"application"] &&
        self.validator(version.UTF8String, display.UTF8String, url.UTF8String, item.informationOnlyUpdate);
    if (!valid && error) *error = [NSError errorWithDomain:@"SpaceTermUpdate" code:SPTVerificationError userInfo:@{NSLocalizedDescriptionKey: @"The update metadata could not be verified."}];
    return valid;
}
- (void)updater:(SPUUpdater *)updater didAbortWithError:(NSError *)error {
    if (self.cancelling || error.code == SUNoUpdateError || error.code == SUInstallationCanceledError) return;
    uint32_t kind = self.phase;
    if ([error.domain isEqualToString:@"SpaceTermUpdate"] || error.code == SUSignatureError || error.code == SUValidationError) kind = SPTVerificationError;
    if (error.code == SURunningFromDiskImageError) kind = SPTReadOnlyError;
    [self emit:SPTFailed version:nil first:kind second:0];
}
- (void)updater:(SPUUpdater *)updater didFinishUpdateCycleForUpdateCheck:(SPUUpdateCheck)check error:(NSError *)error {
    self.choice = nil; self.cancellation = nil; self.retryTermination = nil;
    self.ready = NO;
    [self emit:SPTFinished];
}
@end

@interface SPTUpdateSession : NSObject
@property(nonatomic, strong) SPTUpdateDriver *driver;
@property(nonatomic, strong) SPUUpdater *updater;
@end
@implementation SPTUpdateSession
@end

void *spt_updater_create(void *context, SPTUpdateCallback callback, SPTUpdateValidator validator) {
    if (![NSThread isMainThread]) return NULL;
    NSBundle *bundle = NSBundle.mainBundle;
    if (![bundle objectForInfoDictionaryKey:@"SUPublicEDKey"] || ![bundle objectForInfoDictionaryKey:@"SUFeedURL"]) return NULL;
    SPTUpdateSession *session = [SPTUpdateSession new];
    session.driver = [SPTUpdateDriver new];
    session.driver.context = context;
    session.driver.callback = callback;
    session.driver.validator = validator;
    session.updater = [[SPUUpdater alloc] initWithHostBundle:bundle applicationBundle:bundle userDriver:session.driver delegate:session.driver];
    session.driver.updater = session.updater;
    session.updater.automaticallyChecksForUpdates = NO;
    session.updater.automaticallyDownloadsUpdates = NO;
    session.updater.sendsSystemProfile = NO;
    NSError *error = nil;
    if (![session.updater startUpdater:&error]) { session.driver.callback = NULL; return NULL; }
    return (__bridge_retained void *)session;
}
bool spt_updater_check(void *pointer) {
    SPTUpdateSession *session = (__bridge SPTUpdateSession *)pointer;
    if (![NSThread isMainThread] || !session.updater.canCheckForUpdates) return false;
    session.driver.cancelling = NO; session.driver.ready = NO; session.driver.authorized = NO;
    session.driver.phase = SPTCheckError;
    [session.updater checkForUpdates];
    return true;
}
bool spt_updater_download(void *pointer) {
    SPTUpdateDriver *driver = ((__bridge SPTUpdateSession *)pointer).driver;
    if (![NSThread isMainThread] || !driver.choice || driver.ready || driver.cancelling) return false;
    void (^reply)(SPUUserUpdateChoice) = driver.choice; driver.choice = nil;
    reply(SPUUserUpdateChoiceInstall);
    return true;
}
void spt_updater_cancel(void *pointer) { [((__bridge SPTUpdateSession *)pointer).driver cancel]; }
bool spt_updater_install(void *pointer) {
    SPTUpdateDriver *driver = ((__bridge SPTUpdateSession *)pointer).driver;
    if (![NSThread isMainThread] || driver.cancelling) return false;
    if (driver.authorized && driver.retryTermination) { driver.retryTermination(); return true; }
    if (!driver.ready || !driver.choice) return false;
    driver.authorized = YES;
    void (^reply)(SPUUserUpdateChoice) = driver.choice; driver.choice = nil;
    reply(SPUUserUpdateChoiceInstall);
    return true;
}
void spt_updater_destroy(void *pointer) {
    SPTUpdateSession *session = (__bridge_transfer SPTUpdateSession *)pointer;
    session.driver.callback = NULL;
    [session.driver cancel];
}
