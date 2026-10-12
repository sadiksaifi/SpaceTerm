#import "companion_app.h"

#include <objc/runtime.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* Interaction tests for the evidence companion's real windows and controller.
 *
 * Every port is a fake: no pasteboard, System Settings, Authorization
 * Services, Apple Event delivery, evidence controller, or termination is
 * touched. Windows are built but never shown. Keyboard traversal runs in a
 * window ordered at alpha 0 far off screen. With --preview <dir>, the binary
 * instead writes light and dark renders of both windows for visual review.
 */

static const char *const scope = "companion-interface";
static bool case_failed;

/* Variadic so conditions may contain Objective-C message arguments. */
#define CHECK(...)                                                           \
    do {                                                                     \
        if (!(__VA_ARGS__)) {                                                \
            fprintf(stderr, "check failed at line %d\n", __LINE__);          \
            case_failed = true;                                              \
            return;                                                          \
        }                                                                    \
    } while (0)

/* --- Fakes ----------------------------------------------------------------- */

@interface FakeChannel : NSObject <STCompanionChannel>
@property (nonatomic, strong) NSMutableArray<NSString *> *records;
@property (nonatomic) BOOL closed;
@end

@implementation FakeChannel
- (instancetype)init
{
    self = [super init];
    _records = [NSMutableArray array];
    return self;
}

- (void)sendRecord:(const char *)record length:(size_t)length
{
    NSString *text = [[NSString alloc] initWithBytes:record
                                              length:length
                                            encoding:NSASCIIStringEncoding];
    [_records addObject:text != nil ? text : @"<non-ascii>"];
}

- (void)close
{
    _closed = YES;
}

- (BOOL)sent:(NSString *)record
{
    return [_records containsObject:record];
}

- (BOOL)sentPrefix:(NSString *)prefix
{
    for (NSString *record in _records) {
        if ([record hasPrefix:prefix]) return YES;
    }
    return NO;
}
@end

@interface FakePasteboard : NSObject <STCompanionPasteboard>
@property (nonatomic) NSInteger change;
@property (nonatomic) BOOL plain;
@property (nonatomic, copy) NSString *text;
@property (nonatomic) NSUInteger metadataReads;
@property (nonatomic) NSUInteger contentReads;
@end

@implementation FakePasteboard
- (NSInteger)changeCount
{
    ++_metadataReads;
    return _change;
}

- (BOOL)holdsOnlyPlainText
{
    ++_metadataReads;
    return _plain;
}

- (BOOL)copyPlainText:(uint8_t *)buffer capacity:(size_t)capacity length:(size_t *)length
{
    ++_contentReads;
    const char *bytes = _text.UTF8String;
    size_t size = bytes == NULL ? 0 : strlen(bytes);
    if (size > capacity) return NO;
    memcpy(buffer, bytes, size);
    *length = size;
    return YES;
}
@end

@interface FakeConfirmation : NSObject
@property (nonatomic, copy) NSString *title;
@property (nonatomic, copy) NSString *message;
@property (nonatomic, copy) NSString *confirmTitle;
@property (nonatomic) BOOL destructive;
@property (nonatomic, copy) STCompanionDecision completion;
@end

@implementation FakeConfirmation
@end

@interface FakeConfirmer : NSObject <STCompanionConfirmer>
@property (nonatomic, strong) NSMutableArray<FakeConfirmation *> *requests;
@end

@implementation FakeConfirmer
- (instancetype)init
{
    self = [super init];
    _requests = [NSMutableArray array];
    return self;
}

- (void)confirmWithTitle:(NSString *)title
                 message:(NSString *)message
            confirmTitle:(NSString *)confirmTitle
             destructive:(BOOL)destructive
                  window:(NSWindow *)window
              completion:(STCompanionDecision)completion
{
    (void)window;
    FakeConfirmation *request = [[FakeConfirmation alloc] init];
    request.title = title;
    request.message = message;
    request.confirmTitle = confirmTitle;
    request.destructive = destructive;
    request.completion = completion;
    [_requests addObject:request];
}

- (void)answerLast:(BOOL)confirmed
{
    STCompanionDecision completion = _requests.lastObject.completion;
    _requests.lastObject.completion = nil;
    if (completion != nil) completion(confirmed);
}
@end

@interface FakeAnnouncer : NSObject <STCompanionAnnouncer>
@property (nonatomic, strong) NSMutableArray<NSString *> *announcements;
@end

@implementation FakeAnnouncer
- (instancetype)init
{
    self = [super init];
    _announcements = [NSMutableArray array];
    return self;
}

- (void)announce:(NSString *)text
{
    [_announcements addObject:text];
}
@end

@interface FakeOpener : NSObject <STCompanionSettingsOpener>
@property (nonatomic, strong) NSMutableArray<NSURL *> *urls;
@end

@implementation FakeOpener
- (instancetype)init
{
    self = [super init];
    _urls = [NSMutableArray array];
    return self;
}

- (void)openSettingsURL:(NSURL *)url
{
    [_urls addObject:url];
}
@end

@interface ImmediateExecutor : NSObject <STCompanionExecutor>
@end

@implementation ImmediateExecutor
- (void)perform:(dispatch_block_t)work then:(dispatch_block_t)completion
{
    work();
    completion();
}
@end

@interface FakePresenter : NSObject <STCompanionWindowPresenter>
@property (nonatomic) NSUInteger presented;
@property (nonatomic) NSUInteger activated;
@end

@implementation FakePresenter
- (void)presentWindow:(NSWindow *)window activate:(BOOL)activate
{
    (void)window;
    ++_presented;
    if (activate) ++_activated;
}
@end

/* Records how a presenter orders it in, and never reaches the window server. */
@interface RecordingWindow : NSWindow
@property (nonatomic) NSUInteger orderFrontCount;
@property (nonatomic) NSUInteger regardlessCount;
@property (nonatomic) NSUInteger keyCount;
@end

@implementation RecordingWindow
- (void)orderFront:(id)sender
{
    (void)sender;
    ++_orderFrontCount;
}

- (void)orderFrontRegardless
{
    ++_regardlessCount;
}

- (void)makeKeyAndOrderFront:(id)sender
{
    (void)sender;
    ++_keyCount;
}
@end

typedef struct {
    uint32_t assess_failure;
    uint32_t execute_failure;
    uint32_t sessions;
    uint32_t writes;
    uint32_t completion;
    uint32_t assesses;
    uint32_t executes;
} fake_port_state;

static uint32_t fake_assess(void *context, uint32_t operation, st_companion_assessment *out)
{
    fake_port_state *state = context;
    ++state->assesses;
    if (state->assess_failure != 0) return state->assess_failure;
    memset(out, 0, sizeof *out);
    out->operation = operation;
    out->terminal_sessions = state->sessions;
    out->started_writes = state->writes;
    out->binding[0] = (uint8_t)state->assesses;
    return 0;
}

static uint32_t fake_execute(void *context, const st_companion_assessment *confirmed,
                             st_companion_execution *out)
{
    (void)confirmed;
    fake_port_state *state = context;
    ++state->executes;
    if (state->execute_failure != 0) return state->execute_failure;
    memset(out, 0, sizeof *out);
    out->completion = state->completion;
    return 0;
}

/* --- Fixture --------------------------------------------------------------- */

static uint64_t fake_now = 1000000000ull;

static st_companion_id synthetic_id(uint8_t seed)
{
    st_companion_id id;
    for (size_t i = 0; i < ST_COMPANION_ID_BYTES; ++i) id.bytes[i] = (uint8_t)(seed + i * 17u);
    return id;
}

static NSString *hex_id(uint8_t seed)
{
    st_companion_id id = synthetic_id(seed);
    char hex[ST_COMPANION_ID_HEX + 1];
    st_companion_id_format(&id, hex);
    return [NSString stringWithUTF8String:hex];
}

static const uint8_t run_seed = 0x21;

@interface Fixture : NSObject
@property (nonatomic, strong) STCompanion *companion;
@property (nonatomic, strong) FakeChannel *channel;
@property (nonatomic, strong) FakePasteboard *pasteboard;
@property (nonatomic, strong) FakeConfirmer *confirmer;
@property (nonatomic, strong) FakeAnnouncer *announcer;
@property (nonatomic, strong) FakeOpener *opener;
@property (nonatomic, strong) FakePresenter *presenter;
@property (nonatomic, strong) NSMutableArray<NSNumber *> *quitReplies;
@property (nonatomic) fake_port_state *port_state;
@property (nonatomic) st_companion_authorization_port *port;
@end

@implementation Fixture
- (void)dealloc
{
    free(_port_state);
    free(_port);
}

- (void)feed:(NSString *)record
{
    const char *bytes = record.UTF8String;
    [_companion channelDidReceiveRecord:bytes length:strlen(bytes)];
}

- (STCompanionInterface *)ui
{
    return _companion.interface;
}
@end

/* connect: attach a channel and complete the welcome. active: also report an
 * active run. adapter: link the fake authorization port. */
static Fixture *make_fixture(BOOL connect, BOOL active, BOOL adapter)
{
    Fixture *fixture = [[Fixture alloc] init];
    fixture.channel = [[FakeChannel alloc] init];
    fixture.pasteboard = [[FakePasteboard alloc] init];
    fixture.pasteboard.change = 7;
    fixture.confirmer = [[FakeConfirmer alloc] init];
    fixture.announcer = [[FakeAnnouncer alloc] init];
    fixture.opener = [[FakeOpener alloc] init];
    fixture.presenter = [[FakePresenter alloc] init];
    fixture.quitReplies = [NSMutableArray array];
    fixture.port_state = calloc(1, sizeof(fake_port_state));
    fixture.port = calloc(1, sizeof(st_companion_authorization_port));
    fixture.port_state->completion = ST_COMPANION_COMPLETION_RUNNING;
    fixture.port->context = fixture.port_state;
    fixture.port->assess = fake_assess;
    fixture.port->execute = fake_execute;

    STCompanionPorts *ports = [[STCompanionPorts alloc] init];
    ports.pasteboard = fixture.pasteboard;
    ports.confirmer = fixture.confirmer;
    ports.announcer = fixture.announcer;
    ports.settingsOpener = fixture.opener;
    ports.authorizationExecutor = [[ImmediateExecutor alloc] init];
    ports.presenter = fixture.presenter;
    ports.clock = ^uint64_t(void) {
        return fake_now;
    };
    NSMutableArray<NSNumber *> *replies = fixture.quitReplies;
    ports.quitReply = ^(BOOL quit) {
        [replies addObject:@(quit)];
    };
    ports.authorization = adapter ? fixture.port : NULL;

    st_companion_id run = synthetic_id(run_seed);
    fixture.companion = [[STCompanion alloc] initWithRun:&run ports:ports];
    if (connect) {
        [fixture.companion attachChannel:fixture.channel];
        [fixture feed:[@"welcome schema=1 run=" stringByAppendingString:hex_id(run_seed)]];
        if (active) [fixture feed:@"run state=active"];
    }
    return fixture;
}

static BOOL has(NSString *text, NSString *needle)
{
    return text != nil && [text rangeOfString:needle options:NSCaseInsensitiveSearch].location !=
                              NSNotFound;
}

static NSString *cell_text(STCompanion *companion, uint32_t code, NSString *column)
{
    NSTableView *table = companion.interface.capabilityTable;
    NSTableColumn *tableColumn = [table tableColumnWithIdentifier:column];
    NSTableCellView *cell = (NSTableCellView *)[companion tableView:table
                                                  viewForTableColumn:tableColumn
                                                                 row:(NSInteger)code - 1];
    return cell.textField.stringValue;
}

static NSArray<NSMenuItem *> *all_items(NSMenu *menu)
{
    NSMutableArray<NSMenuItem *> *items = [NSMutableArray array];
    for (NSMenuItem *item in menu.itemArray) {
        [items addObject:item];
        if (item.submenu != nil) [items addObjectsFromArray:all_items(item.submenu)];
    }
    return items;
}

static NSMenuItem *item_with_action(NSMenu *menu, SEL action)
{
    for (NSMenuItem *item in all_items(menu)) {
        if (item.action == action) return item;
    }
    return nil;
}

static NSAppleEventDescriptor *receipt_event(NSString *run, NSString *nonce)
{
    NSAppleEventDescriptor *event =
        [NSAppleEventDescriptor appleEventWithEventClass:ST_COMPANION_EVENT_CLASS
                                                 eventID:ST_COMPANION_EVENT_ID
                                        targetDescriptor:nil
                                                returnID:kAutoGenerateReturnID
                                           transactionID:kAnyTransactionID];
    if (run != nil) {
        [event setParamDescriptor:[NSAppleEventDescriptor descriptorWithString:run]
                       forKeyword:ST_COMPANION_EVENT_RUN];
    }
    if (nonce != nil) {
        [event setParamDescriptor:[NSAppleEventDescriptor descriptorWithString:nonce]
                       forKeyword:ST_COMPANION_EVENT_NONCE];
    }
    return event;
}

static NSAppleEventDescriptor *reply_event(void)
{
    return [NSAppleEventDescriptor appleEventWithEventClass:kCoreEventClass
                                                    eventID:kAEAnswer
                                           targetDescriptor:nil
                                                   returnID:kAutoGenerateReturnID
                                              transactionID:kAnyTransactionID];
}

static NSAppleEventDescriptor *url_event(NSString *url)
{
    NSAppleEventDescriptor *event =
        [NSAppleEventDescriptor appleEventWithEventClass:kInternetEventClass
                                                 eventID:kAEGetURL
                                        targetDescriptor:nil
                                                returnID:kAutoGenerateReturnID
                                           transactionID:kAnyTransactionID];
    [event setParamDescriptor:[NSAppleEventDescriptor descriptorWithString:url]
                   forKeyword:keyDirectObject];
    return event;
}

static NSString *receipt_url(NSString *run, NSString *nonce)
{
    return [NSString stringWithFormat:@"%s://receipt?run=%@&nonce=%@", ST_COMPANION_URL_SCHEME,
                                      run, nonce];
}

static NSView *frame_view(NSWindow *window)
{
    NSView *content = window.contentView;
    return content.superview != nil ? (NSView *)content.superview : content;
}

/* Renders a window's frame view, including its background, without showing
 * the window. */
static NSBitmapImageRep *render(NSWindow *window, NSAppearanceName appearance)
{
    window.appearance = [NSAppearance appearanceNamed:appearance];
    NSView *frame = frame_view(window);
    [window.contentView layoutSubtreeIfNeeded];
    [frame layoutSubtreeIfNeeded];
    NSBitmapImageRep *rep = [frame bitmapImageRepForCachingDisplayInRect:frame.bounds];
    [frame cacheDisplayInRect:frame.bounds toBitmapImageRep:rep];
    return rep;
}

/* The pixels a view draws now, to show that its drawing did not change. */
static NSData *view_pixels(NSView *view)
{
    [view.window.contentView layoutSubtreeIfNeeded];
    if (NSIsEmptyRect(view.bounds)) return [NSData data];
    NSBitmapImageRep *rep = [view bitmapImageRepForCachingDisplayInRect:view.bounds];
    [view cacheDisplayInRect:view.bounds toBitmapImageRep:rep];
    return [NSData dataWithBytes:rep.bitmapData
                          length:(NSUInteger)(rep.bytesPerRow * rep.pixelsHigh)];
}

static NSColor *srgb_at(NSBitmapImageRep *rep, NSView *view, NSPoint pointInView)
{
    NSView *frame = frame_view(view.window);
    NSPoint point = [view convertPoint:pointInView toView:frame];
    CGFloat scale = (CGFloat)rep.pixelsWide / NSWidth(frame.bounds);
    NSInteger x = (NSInteger)(point.x * scale);
    NSInteger y = frame.isFlipped ? (NSInteger)(point.y * scale)
                                  : (NSInteger)((NSHeight(frame.bounds) - point.y) * scale);
    /* colorAtX:y: tags samples as generic RGB whatever the rep's color space
     * is, so read the samples and tag them with the rep's own space. */
    NSUInteger samples[5] = {0};
    NSInteger count = rep.samplesPerPixel;
    if (count < 3 || count > 5 || rep.bitsPerSample <= 0 || rep.bitsPerSample > 16 ||
        rep.colorSpace.colorSpaceModel != NSColorSpaceModelRGB ||
        (rep.bitmapFormat & NSBitmapFormatFloatingPointSamples) != 0 ||
        (rep.bitmapFormat & NSBitmapFormatAlphaFirst) != 0) {
        /* Mid gray fails every white, black, red, and contrast check. */
        return [NSColor colorWithSRGBRed:0.5 green:0.5 blue:0.5 alpha:1];
    }
    [rep getPixel:samples atX:x y:y];
    CGFloat maximum = (CGFloat)((1u << rep.bitsPerSample) - 1);
    CGFloat components[5] = {0};
    for (NSInteger i = 0; i < count; ++i) components[i] = (CGFloat)samples[i] / maximum;
    if (count == 3) components[3] = 1;
    NSColor *color = [NSColor colorWithColorSpace:rep.colorSpace components:components count:4];
    return [color colorUsingColorSpace:NSColorSpace.sRGBColorSpace];
}

/* The capture pattern uses only sRGB white, black, and red. */
static BOOL pattern_color(NSColor *color)
{
    BOOL high[3] = {color.redComponent > 0.95, color.greenComponent > 0.95,
                    color.blueComponent > 0.95};
    BOOL low[3] = {color.redComponent < 0.05, color.greenComponent < 0.05,
                   color.blueComponent < 0.05};
    return (high[0] && high[1] && high[2]) || (low[0] && low[1] && low[2]) ||
           (high[0] && low[1] && low[2]);
}

static CGFloat luminance(NSColor *color)
{
    return 0.2126 * color.redComponent + 0.7152 * color.greenComponent +
           0.0722 * color.blueComponent;
}

/* --- Required cases -------------------------------------------------------- */

static void permission_explanation_names_requesting_process(void)
{
    Fixture *f = make_fixture(YES, YES, NO);
    STCompanionInterface *ui = f.ui;
    CHECK(has(ui.scopeLabel.stringValue, @"SpaceTerm app"));
    CHECK(has(ui.scopeLabel.stringValue, @"SpaceTerm Server"));
    CHECK(has(ui.scopeLabel.stringValue, @"do not apply"));
    for (uint32_t code = 1; code <= ST_COMPANION_CAPABILITY_COUNT; ++code) {
        const st_companion_capability *capability = st_companion_capability_find(code);
        [f.companion selectCapability:code];
        CHECK([f.companion selectedCapability] == code);
        CHECK([ui.detailTitle.stringValue isEqualToString:@(capability->name)]);
        NSArray<STContextPanel *> *panels = @[ui.guiPanel, ui.daemonPanel];
        for (NSUInteger context = 0; context < panels.count; ++context) {
            const st_companion_context_copy *copy = &capability->contexts[context];
            STContextPanel *panel = panels[context];
            CHECK([panel.explanation.stringValue isEqualToString:@(copy->explanation)]);
            if (copy->process == ST_COMPANION_PROCESS_NONE) {
                CHECK(has(panel.whoAsks.stringValue, @"No System Permission"));
                continue;
            }
            NSString *requester = @(copy->requester);
            CHECK(has(panel.whoAsks.stringValue, @"Requesting process"));
            CHECK(has(panel.whoAsks.stringValue, requester));
            CHECK(has(panel.explanation.stringValue, requester));
        }
        if (capability->system_permission && code != ST_COMPANION_CAPABILITY_LOGIN_KEYCHAIN &&
            code != ST_COMPANION_CAPABILITY_TOUCH_ID_SUDO) {
            CHECK(has(ui.guiPanel.whoAsks.stringValue, @"SpaceTerm app"));
            CHECK(has(ui.daemonPanel.whoAsks.stringValue, @"SpaceTerm Server"));
            CHECK(!has(ui.daemonPanel.whoAsks.stringValue, @"SpaceTerm app"));
        }
    }
}

static void authorization_is_separate_from_destructive_confirmation(void)
{
    Fixture *f = make_fixture(YES, YES, YES);
    STCompanionInterface *ui = f.ui;
    f.port_state->sessions = 2;
    f.port_state->writes = 1;
    f.port_state->completion = ST_COMPANION_COMPLETION_STOPPED;

    [f.companion selectOperation:ST_COMPANION_OPERATION_STOP];
    [f.companion requestAuthorization:nil];
    /* Assessed once; the adapter has not been asked to authorize anything. */
    CHECK(f.port_state->assesses == 1);
    CHECK(f.port_state->executes == 0);
    CHECK(f.confirmer.requests.count == 1);
    FakeConfirmation *confirmation = f.confirmer.requests.lastObject;
    CHECK(confirmation.destructive);
    CHECK([confirmation.title isEqualToString:@"Stop Server?"]);
    CHECK([confirmation.confirmTitle isEqualToString:@"Stop Server"]);
    CHECK(!has(confirmation.confirmTitle, @"Authoriz"));
    CHECK(has(confirmation.message, @"2 Terminal Sessions will end."));
    CHECK(has(confirmation.message, @"separate dialog"));
    CHECK(has(confirmation.message, @"Confirming here does not authorize it."));
    CHECK(has(ui.authorizationTitle.stringValue, @"Waiting for your confirmation"));
    CHECK(has(ui.authorizationDetail.stringValue, @"macOS has not been asked yet"));
    CHECK(!ui.requestAuthorizationButton.enabled);
    CHECK(![f.channel sentPrefix:@"authorization "]);

    [f.confirmer answerLast:YES];
    CHECK(f.port_state->executes == 1);
    CHECK(f.companion.authorizationFlow->outcome == ST_COMPANION_AUTH_OUTCOME_COMPLETED_STOPPED);
    CHECK([f.channel sent:@"authorization operation=stop outcome=completed-stopped\n"]);
    CHECK(has(ui.authorizationTitle.stringValue, @"Stop Server: Stopped"));
    CHECK(ui.requestAuthorizationButton.enabled);

    /* Declining never reaches the adapter's authorization step. */
    [f.companion selectOperation:ST_COMPANION_OPERATION_RESTART];
    [f.companion requestAuthorization:nil];
    CHECK(f.confirmer.requests.count == 2);
    CHECK([f.confirmer.requests.lastObject.title isEqualToString:@"Restart Server?"]);
    [f.confirmer answerLast:NO];
    CHECK(f.port_state->executes == 1);
    CHECK(f.companion.authorizationFlow->outcome == ST_COMPANION_AUTH_OUTCOME_DECLINED);
    CHECK(has(ui.authorizationDetail.stringValue, @"macOS was not asked"));

    /* Start ends nothing, so it has no companion confirmation; macOS still
     * asks through the adapter. */
    f.port_state->completion = ST_COMPANION_COMPLETION_RUNNING;
    [f.companion selectOperation:ST_COMPANION_OPERATION_START];
    [f.companion requestAuthorization:nil];
    CHECK(f.confirmer.requests.count == 2);
    CHECK(f.port_state->executes == 2);
    CHECK([f.channel sent:@"authorization operation=start outcome=completed-running\n"]);

    /* An assessment that expires while the person reads the confirmation is
     * assessed again and confirmed again before anything executes. */
    f.port_state->completion = ST_COMPANION_COMPLETION_STOPPED;
    [f.companion selectOperation:ST_COMPANION_OPERATION_STOP];
    [f.companion requestAuthorization:nil];
    CHECK(f.confirmer.requests.count == 3);
    uint32_t assessed = f.port_state->assesses;
    fake_now += 31ull * 1000000000ull;
    [f.confirmer answerLast:YES];
    CHECK(f.port_state->assesses == assessed + 1);
    CHECK(f.port_state->executes == 2);
    CHECK(f.confirmer.requests.count == 4);
    [f.confirmer answerLast:YES];
    CHECK(f.port_state->executes == 3);
}

static void denied_cancelled_and_unavailable_are_distinct_visible_states(void)
{
    /* Authorization outcomes. */
    struct {
        uint32_t failure;
        BOOL adapter;
        NSString *title;
    } outcomes[] = {
        {16, YES, @"Authorization denied"},
        {17, YES, @"Authorization cancelled"},
        {18, YES, @"Authorization unavailable"},
        {0, NO, @"Adapter unavailable"},
        {2, YES, @"macOS service unavailable"},
        {25, YES, @"Background item off"},
        {29, YES, @"Timed out"},
    };
    NSMutableSet<NSString *> *titles = [NSMutableSet set];
    NSMutableSet<NSString *> *details = [NSMutableSet set];
    for (size_t i = 0; i < sizeof outcomes / sizeof outcomes[0]; ++i) {
        Fixture *f = make_fixture(YES, YES, outcomes[i].adapter);
        f.port_state->execute_failure = outcomes[i].failure;
        [f.companion selectOperation:ST_COMPANION_OPERATION_START];
        [f.companion requestAuthorization:nil];
        NSString *title = f.ui.authorizationTitle.stringValue;
        CHECK(has(title, outcomes[i].title));
        CHECK(f.ui.authorizationSymbol.image != nil);
        [titles addObject:title];
        [details addObject:f.ui.authorizationDetail.stringValue];
        CHECK(has(f.announcer.announcements.lastObject, outcomes[i].title));
    }
    CHECK(titles.count == sizeof outcomes / sizeof outcomes[0]);
    CHECK(details.count == sizeof outcomes / sizeof outcomes[0]);

    /* Probe results, in both contexts. */
    Fixture *f = make_fixture(YES, YES, NO);
    NSArray<NSString *> *results = @[
        @"denied", @"cancelled", @"unavailable", @"timed-out", @"cleanup-unresolved",
        @"indeterminate", @"failed"
    ];
    NSMutableSet<NSString *> *shown = [NSMutableSet set];
    for (NSUInteger i = 0; i < results.count; ++i) {
        uint32_t code = (uint32_t)i + 1;
        [f feed:[NSString stringWithFormat:@"result capability=%u context=daemon-parented "
                                           @"result=%@ responsible=server preflight=denied "
                                           @"effect=blocked cleanup=complete",
                                           code, results[i]]];
        [f.companion selectCapability:code];
        NSString *title = f.ui.daemonPanel.resultTitle.stringValue;
        CHECK(title.length != 0);
        CHECK(f.ui.daemonPanel.resultDetail.stringValue.length != 0);
        CHECK([cell_text(f.companion, code, @"daemon") isEqualToString:title]);
        CHECK([f.ui.guiPanel.resultTitle.stringValue isEqualToString:@"Not run"]);
        [shown addObject:title];
    }
    CHECK(shown.count == results.count);
    CHECK([shown containsObject:@"Denied"]);
    CHECK([shown containsObject:@"Cancelled"]);
    CHECK([shown containsObject:@"Unavailable"]);
    CHECK([shown containsObject:@"Cleanup unresolved"]);

    /* Run-level cleanup uncertainty is text too. */
    [f feed:@"run state=cleanup-unresolved"];
    CHECK([f.ui.runTitle.stringValue isEqualToString:@"Cleanup unresolved"]);
    CHECK(has(f.ui.runDetail.stringValue, @"could not confirm cleanup"));
}

static void keyboard_navigation_reaches_every_probe_and_cleanup_control(void)
{
    Fixture *f = make_fixture(YES, YES, YES);
    STCompanionInterface *ui = f.ui;
    [f.companion selectCapability:ST_COMPANION_CAPABILITY_SCREEN_RECORDING];

    NSArray<NSView *> *order = [ui mainKeyViewOrder];
    NSArray<NSView *> *expected = @[
        ui.capabilityTable, ui.openSettingsButton, ui.requestProbeButton, ui.operationPopUp,
        ui.requestAuthorizationButton, ui.showTargetsButton, ui.stopRunButton
    ];
    CHECK([order isEqualToArray:expected]);
    CHECK(!ui.mainWindow.autorecalculatesKeyViewLoop);
    CHECK(ui.mainWindow.initialFirstResponder == ui.capabilityTable);
    /* The explicit loop visits every control once and returns to the table. */
    NSView *view = ui.capabilityTable;
    for (NSUInteger i = 0; i < order.count; ++i) {
        CHECK(view == order[i]);
        CHECK(view.window == ui.mainWindow);
        CHECK(view.acceptsFirstResponder);
        view = view.nextKeyView;
    }
    CHECK(view == ui.capabilityTable);
    for (NSUInteger i = 1; i < order.count; ++i) {
        NSControl *control = (NSControl *)order[i];
        CHECK(control.enabled);
        CHECK(!control.hidden);
        CHECK(control.action != NULL);
    }

    /* Every probe, authorization, and cleanup action is in the menu bar,
     * which is reachable from the keyboard. Stop has no shortcut on purpose. */
    NSMenu *menu = [f.companion mainMenu];
    struct {
        SEL action;
        NSString *key;
    } items[] = {
        {@selector(requestProbe:), @"r"},
        {@selector(openPrivacySettings:), @""},
        {@selector(requestAuthorization:), @""},
        {@selector(stopEvidenceRun:), @""},
        {@selector(showCompanion:), @"1"},
        {@selector(showTestTargets:), @"2"},
    };
    for (size_t i = 0; i < sizeof items / sizeof items[0]; ++i) {
        NSMenuItem *item = item_with_action(menu, items[i].action);
        CHECK(item != nil);
        CHECK([item.keyEquivalent isEqualToString:items[i].key]);
        CHECK(item.target == f.companion);
        CHECK([f.companion validateMenuItem:item]);
    }
    CHECK([item_with_action(menu, @selector(paste:)).keyEquivalent isEqualToString:@"v"]);
    CHECK([item_with_action(menu, @selector(terminate:)).keyEquivalent isEqualToString:@"q"]);
    CHECK([item_with_action(menu, @selector(performClose:)).keyEquivalent isEqualToString:@"w"]);
    CHECK(![f.companion validateMenuItem:[[NSMenuItem alloc] initWithTitle:@"Other"
                                                                    action:@selector(copy:)
                                                             keyEquivalent:@""]]);

    /* Real traversal in windows ordered at alpha 0 far off screen. Buttons
     * join the loop only with Full Keyboard Access, a system setting these
     * tests do not change, so the views that do not depend on it are driven
     * here and the button chain is checked above. */
    NSWindow *main = ui.mainWindow;
    NSWindow *targets = ui.targetsWindow;
    for (NSWindow *window in @[main, targets]) {
        window.alphaValue = 0;
        window.ignoresMouseEvents = YES;
        [window setFrameOrigin:NSMakePoint(-30000, -30000)];
        [window orderBack:nil];
    }
    CHECK([main makeFirstResponder:ui.capabilityTable]);
    CHECK(main.firstResponder == ui.capabilityTable);
    NSEvent *down = [NSEvent keyEventWithType:NSEventTypeKeyDown
                                     location:NSZeroPoint
                                modifierFlags:0
                                    timestamp:0
                                 windowNumber:main.windowNumber
                                      context:nil
                                   characters:@"\uF701"
                  charactersIgnoringModifiers:@"\uF701"
                                    isARepeat:NO
                                      keyCode:125];
    [main sendEvent:down];
    CHECK([f.companion selectedCapability] == ST_COMPANION_CAPABILITY_ACCESSIBILITY);
    CHECK(has(ui.detailTitle.stringValue, @"Accessibility"));

    CHECK([[ui targetsKeyViewOrder] containsObject:ui.clipboardTarget]);
    CHECK(targets.initialFirstResponder == ui.clipboardTarget);
    CHECK([targets makeFirstResponder:ui.clipboardTarget]);
    NSEvent *tab = [NSEvent keyEventWithType:NSEventTypeKeyDown
                                    location:NSZeroPoint
                               modifierFlags:0
                                   timestamp:0
                                windowNumber:targets.windowNumber
                                     context:nil
                                  characters:@"\t"
                 charactersIgnoringModifiers:@"\t"
                                   isARepeat:NO
                                     keyCode:48];
    [targets sendEvent:tab];
    CHECK(targets.firstResponder == ui.clipboardTarget);
    /* Edit > Paste goes to the focused clipboard target. */
    CHECK([NSApp targetForAction:@selector(paste:) to:nil from:nil] == nil ||
          targets.firstResponder == ui.clipboardTarget);
    CHECK([ui.clipboardTarget respondsToSelector:@selector(paste:)]);
    CHECK([(id<NSMenuItemValidation>)ui.clipboardTarget
        validateMenuItem:item_with_action(menu, @selector(paste:))]);
    for (NSWindow *window in @[main, targets]) [window orderOut:nil];
}

static void synthetic_targets_do_not_access_other_application_content(void)
{
    Fixture *f = make_fixture(YES, YES, NO);
    STCompanionInterface *ui = f.ui;
    FakePasteboard *pasteboard = f.pasteboard;
    NSString *foreign = @"not-a-marker other app text";
    pasteboard.text = foreign;
    pasteboard.plain = YES;

    /* Nothing armed: a paste touches no pasteboard data at all. */
    [ui.clipboardTarget paste:nil];
    CHECK(pasteboard.metadataReads == 0);
    CHECK(pasteboard.contentReads == 0);
    CHECK([f.channel sent:@"receipt target=clipboard status=unexpected\n"]);
    CHECK(has(ui.clipboardStatus.stringValue, @"reads nothing"));

    /* Armed, but the clipboard changed after the probe copied: not read. */
    [f feed:[NSString stringWithFormat:@"expect target=clipboard nonce=%@ deadline-ms=60000",
                                       hex_id(0x41)]];
    CHECK(pasteboard.metadataReads == 1);
    CHECK(f.presenter.presented == 1);
    CHECK(f.presenter.activated == 0);
    pasteboard.change = 8;
    [ui.clipboardTarget paste:nil];
    CHECK(pasteboard.contentReads == 0);
    CHECK([f.channel sent:[NSString stringWithFormat:@"receipt target=clipboard "
                                                     @"status=superseded nonce=%@\n",
                                                     hex_id(0x41)]]);

    /* Armed, but the clipboard holds more than plain text: not read. */
    [f feed:[NSString stringWithFormat:@"expect target=clipboard nonce=%@ deadline-ms=60000",
                                       hex_id(0x42)]];
    pasteboard.plain = NO;
    [ui.clipboardTarget paste:nil];
    CHECK(pasteboard.contentReads == 0);
    CHECK([f.channel sentPrefix:@"receipt target=clipboard status=mismatch"]);

    /* Armed and unchanged: exactly one bounded read of the marker. */
    NSString *marker = [@ST_COMPANION_MARKER_PREFIX stringByAppendingString:hex_id(0x43)];
    pasteboard.plain = YES;
    pasteboard.text = marker;
    [f feed:[NSString stringWithFormat:@"expect target=clipboard nonce=%@ deadline-ms=60000",
                                       hex_id(0x43)]];
    [ui.clipboardTarget paste:nil];
    CHECK(pasteboard.contentReads == 1);
    CHECK([f.channel sent:[NSString stringWithFormat:@"receipt target=clipboard "
                                                     @"status=accepted nonce=%@\n",
                                                     hex_id(0x43)]]);
    /* A second paste of the same marker reads nothing. */
    [ui.clipboardTarget paste:nil];
    CHECK(pasteboard.contentReads == 1);

    /* The Accessibility target forwards the value and never keeps or shows it. */
    NSData *unset = view_pixels(ui.accessibilityTarget);
    CHECK(unset.length != 0);
    [ui.accessibilityTarget setAccessibilityValue:foreign];
    CHECK([ui.accessibilityTarget.accessibilityValue isEqual:@""]);
    CHECK([view_pixels(ui.accessibilityTarget) isEqualToData:unset]);
    CHECK([f.channel sent:@"receipt target=accessibility status=malformed\n"]);
    [ui.accessibilityTarget setAccessibilityValue:@[@1]];
    [ui.accessibilityTarget setAccessibilityValue:[foreign stringByPaddingToLength:4096
                                                                         withString:@"x"
                                                                    startingAtIndex:0]];
    CHECK(f.companion.ledger->rejected[ST_COMPANION_TARGET_ACCESSIBILITY] == 3);

    /* Received content never reaches the interface, announcements, or records. */
    NSMutableArray<NSString *> *visible = [NSMutableArray array];
    for (NSTextField *field in @[ui.clipboardStatus, ui.accessibilityStatus, ui.urlStatus,
                                 ui.appleEventStatus, ui.captureStatus]) {
        [visible addObject:field.stringValue];
    }
    [visible addObjectsFromArray:f.announcer.announcements];
    [visible addObjectsFromArray:f.channel.records];
    for (NSString *text in visible) {
        CHECK(!has(text, @"other app text"));
        CHECK(!has(text, marker));
    }
    for (NSString *record in f.channel.records) {
        const char *line = record.UTF8String;
        for (const char *c = line; *c; ++c) {
            CHECK((*c >= 'a' && *c <= 'z') || (*c >= '0' && *c <= '9') || *c == ' ' ||
                  *c == '=' || *c == '-' || *c == '\n');
        }
    }

    /* Longer status text grows the window instead of clipping a target. */
    NSWindow *targets = ui.targetsWindow;
    NSView *root = targets.contentView;
    [root layoutSubtreeIfNeeded];
    for (NSView *view in root.subviews) {
        NSRect frame = [view convertRect:view.bounds toView:root];
        CHECK(NSContainsRect(NSInsetRect(root.bounds, -0.5, -0.5), frame));
    }

    /* The capture target draws only its own frame and code. */
    NSBitmapImageRep *before = render(targets, NSAppearanceNameAqua);
    NSView *pattern = (NSView *)ui.capturePattern;
    const CGFloat cell = ST_COMPANION_CAPTURE_CELL_POINTS;
    NSColor *frame = srgb_at(before, pattern, NSMakePoint(cell / 2, cell / 2));
    CHECK(frame.redComponent > 0.95 && frame.greenComponent < 0.05 && frame.blueComponent < 0.05);
    for (int row = 1; row < ST_COMPANION_CAPTURE_ROWS - 1; ++row) {
        for (int column = 1; column < ST_COMPANION_CAPTURE_COLUMNS - 1; ++column) {
            NSColor *color = srgb_at(before, pattern,
                                     NSMakePoint(column * cell + cell / 2, row * cell + cell / 2));
            CHECK(color.redComponent > 0.95 && color.greenComponent > 0.95 &&
                  color.blueComponent > 0.95);
        }
    }
    st_companion_id code = synthetic_id(0x44);
    [f feed:[NSString stringWithFormat:@"expect target=capture nonce=%@ deadline-ms=60000",
                                       hex_id(0x44)]];
    /* The controller captures only after the pattern for this nonce is on screen. */
    CHECK([f.channel sent:[NSString stringWithFormat:@"armed target=capture nonce=%@\n",
                                                     hex_id(0x44)]]);
    NSBitmapImageRep *after = render(targets, NSAppearanceNameAqua);
    for (int bit = 0; bit < 128; ++bit) {
        int row = 1 + bit / 16;
        int column = 1 + bit % 16;
        BOOL set = (code.bytes[bit / 8] >> (7 - bit % 8)) & 1;
        NSColor *color = srgb_at(after, pattern,
                                 NSMakePoint(column * cell + cell / 2, row * cell + cell / 2));
        CHECK(set ? luminance(color) < 0.05 : luminance(color) > 0.95);
    }
    CHECK(has(ui.captureStatus.stringValue, @"captures only this window"));
}

static void url_and_apple_event_receipts_bind_exact_run(void)
{
    Fixture *f = make_fixture(YES, YES, NO);
    NSString *run = hex_id(run_seed);
    NSString *foreignRun = hex_id(0x77);
    NSString *urlNonce = hex_id(0x51);
    NSString *eventNonce = hex_id(0x52);
    [f feed:[NSString stringWithFormat:@"expect target=url nonce=%@ deadline-ms=5000", urlNonce]];
    [f feed:[NSString stringWithFormat:@"expect target=apple-event nonce=%@ deadline-ms=5000",
                                       eventNonce]];
    CHECK([f.channel sent:[NSString stringWithFormat:@"armed target=url nonce=%@\n", urlNonce]]);
    CHECK([f.channel sent:[NSString stringWithFormat:@"armed target=apple-event nonce=%@\n",
                                                     eventNonce]]);

    /* URL target. */
    [f.companion handleURLEvent:url_event(receipt_url(foreignRun, urlNonce))
                 withReplyEvent:[NSAppleEventDescriptor nullDescriptor]];
    CHECK([f.channel sent:@"receipt target=url status=foreign-run\n"]);
    [f.companion handleURLEvent:url_event([receipt_url(run, urlNonce) stringByAppendingString:@"#x"])
                 withReplyEvent:[NSAppleEventDescriptor nullDescriptor]];
    CHECK([f.channel sent:@"receipt target=url status=malformed\n"]);
    [f.companion handleURLEvent:url_event(receipt_url(run, eventNonce))
                 withReplyEvent:[NSAppleEventDescriptor nullDescriptor]];
    CHECK([f.channel sent:@"receipt target=url status=unexpected\n"]);
    [f.companion handleURLEvent:url_event(receipt_url(run, urlNonce))
                 withReplyEvent:[NSAppleEventDescriptor nullDescriptor]];
    CHECK([f.channel sent:[NSString stringWithFormat:@"receipt target=url status=accepted "
                                                     @"nonce=%@\n",
                                                     urlNonce]]);
    [f.companion handleURLEvent:url_event(receipt_url(run, urlNonce))
                 withReplyEvent:[NSAppleEventDescriptor nullDescriptor]];
    CHECK([f.channel sent:[NSString stringWithFormat:@"receipt target=url status=replayed "
                                                     @"nonce=%@\n",
                                                     urlNonce]]);
    CHECK(f.companion.ledger->accepted[ST_COMPANION_TARGET_URL] == 1);
    CHECK(has(f.ui.urlStatus.stringValue, @"Accepted: 1."));

    /* Apple Event target: a foreign run is rejected with an error reply. */
    NSAppleEventDescriptor *reply = reply_event();
    [f.companion handleReceiptEvent:receipt_event(foreignRun, eventNonce) withReplyEvent:reply];
    CHECK([[reply paramDescriptorForKeyword:keyDirectObject].stringValue
        isEqualToString:@"foreign-run"]);
    CHECK([reply paramDescriptorForKeyword:keyErrorNumber].int32Value == errAEEventFailed);
    CHECK([reply paramDescriptorForKeyword:ST_COMPANION_EVENT_NONCE] == nil);

    reply = reply_event();
    [f.companion handleReceiptEvent:receipt_event(run, nil) withReplyEvent:reply];
    CHECK([[reply paramDescriptorForKeyword:keyDirectObject].stringValue
        isEqualToString:@"malformed"]);

    reply = reply_event();
    [f.companion handleReceiptEvent:receipt_event(run, eventNonce) withReplyEvent:reply];
    CHECK([[reply paramDescriptorForKeyword:keyDirectObject].stringValue
        isEqualToString:@"accepted"]);
    CHECK([[reply paramDescriptorForKeyword:ST_COMPANION_EVENT_RUN].stringValue
        isEqualToString:run]);
    CHECK([[reply paramDescriptorForKeyword:ST_COMPANION_EVENT_NONCE].stringValue
        isEqualToString:eventNonce]);
    CHECK([reply paramDescriptorForKeyword:keyErrorNumber] == nil);
    CHECK([f.channel sent:[NSString stringWithFormat:@"receipt target=apple-event "
                                                     @"status=accepted nonce=%@\n",
                                                     eventNonce]]);

    reply = reply_event();
    [f.companion handleReceiptEvent:receipt_event(run, eventNonce) withReplyEvent:reply];
    CHECK([[reply paramDescriptorForKeyword:keyDirectObject].stringValue
        isEqualToString:@"replayed"]);

    /* An expired expectation is reported as expired, not accepted. */
    NSString *late = hex_id(0x53);
    [f feed:[NSString stringWithFormat:@"expect target=apple-event nonce=%@ deadline-ms=1000",
                                       late]];
    fake_now += 2000000000ull;
    reply = reply_event();
    [f.companion handleReceiptEvent:receipt_event(run, late) withReplyEvent:reply];
    CHECK([[reply paramDescriptorForKeyword:keyDirectObject].stringValue
        isEqualToString:@"expired"]);
    CHECK(f.companion.ledger->accepted[ST_COMPANION_TARGET_APPLE_EVENT] == 1);

    /* Without a bound run nothing is accepted. */
    Fixture *unbound = make_fixture(NO, NO, NO);
    reply = reply_event();
    [unbound.companion handleReceiptEvent:receipt_event(run, eventNonce) withReplyEvent:reply];
    CHECK([[reply paramDescriptorForKeyword:keyDirectObject].stringValue
        isEqualToString:@"no-run"]);
    CHECK(unbound.channel.records.count == 0);
}

static void companion_quit_does_not_claim_daemon_cleanup(void)
{
    Fixture *f = make_fixture(YES, YES, NO);
    NSUInteger before = f.channel.records.count;
    CHECK([f.companion applicationShouldTerminate:NSApp] == NSTerminateLater);
    CHECK(f.confirmer.requests.count == 1);
    FakeConfirmation *confirmation = f.confirmer.requests.lastObject;
    CHECK([confirmation.title isEqualToString:@"Quit SpaceTerm Evidence Companion?"]);
    CHECK([confirmation.confirmTitle isEqualToString:@"Quit"]);
    CHECK(!confirmation.destructive);
    CHECK(has(confirmation.message, @"does not stop the evidence run or clean it up"));
    CHECK(has(confirmation.message, @"controller still owns cleanup"));
    CHECK(!has(confirmation.message, @"cleanup completed"));
    CHECK(!has(confirmation.message, @"was cleaned up"));
    CHECK(f.channel.records.count == before);

    [f.confirmer answerLast:YES];
    CHECK(f.quitReplies.count == 1 && f.quitReplies[0].boolValue);
    CHECK([f.channel.records.lastObject isEqualToString:@"closing reason=quit\n"]);
    CHECK(![f.channel sentPrefix:@"request"]);
    CHECK(f.companion.runState == ST_COMPANION_RUN_ACTIVE);
    CHECK([f.ui.runTitle.stringValue isEqualToString:@"Evidence run active"]);
    CHECK(!f.channel.closed);

    /* Declining keeps the companion running and says nothing. */
    Fixture *declined = make_fixture(YES, YES, NO);
    NSUInteger count = declined.channel.records.count;
    CHECK([declined.companion applicationShouldTerminate:NSApp] == NSTerminateLater);
    [declined.confirmer answerLast:NO];
    CHECK(declined.quitReplies.count == 1 && !declined.quitReplies[0].boolValue);
    CHECK(declined.channel.records.count == count);

    /* With no run, quitting needs no confirmation and still claims nothing. */
    Fixture *idle = make_fixture(NO, NO, NO);
    CHECK([idle.companion applicationShouldTerminate:NSApp] == NSTerminateNow);
    CHECK(idle.confirmer.requests.count == 0);

    /* A lost controller is reported as unknown, never as stopped or clean. */
    Fixture *lost = make_fixture(YES, YES, NO);
    [lost.companion channelDidCloseWithState:ST_COMPANION_RUN_CONTROLLER_LOST];
    CHECK(lost.companion.runState == ST_COMPANION_RUN_CONTROLLER_LOST);
    CHECK(has(lost.ui.runDetail.stringValue, @"cannot tell whether the run stopped"));
    CHECK(!has(lost.ui.runTitle.stringValue, @"Stopped"));
    CHECK(!lost.ui.stopRunButton.enabled);
    CHECK(!lost.ui.requestProbeButton.enabled);
}

static void no_automatic_grant_or_global_reset_operation_exists(void)
{
    Fixture *f = make_fixture(YES, YES, YES);
    /* A whole run of reports causes no side effect without a person. */
    [f feed:@"progress capability=4 context=daemon-parented phase=waiting-for-person"];
    [f feed:@"result capability=4 context=daemon-parented result=denied responsible=server "
            @"preflight=denied effect=blocked cleanup=complete"];
    [f feed:@"run state=stopping"];
    [f feed:@"run state=stopped"];
    CHECK(f.opener.urls.count == 0);
    CHECK(f.port_state->assesses == 0 && f.port_state->executes == 0);
    CHECK(f.pasteboard.metadataReads == 0 && f.pasteboard.contentReads == 0);
    CHECK(f.confirmer.requests.count == 0);
    CHECK(f.presenter.activated == 0);

    /* Every action has an allowed effect; none grants, resets, or cleans up. */
    for (uint32_t action = 1; action <= ST_COMPANION_ACTION_COUNT; ++action) {
        st_companion_effect effect = st_companion_action_find(action)->effect;
        CHECK(effect >= ST_COMPANION_EFFECT_PRESENTATION &&
              effect <= ST_COMPANION_EFFECT_QUIT_COMPANION);
    }
    NSSet<NSString *> *allowed = [NSSet setWithArray:@[
        @"orderFrontStandardAboutPanel:", @"hide:", @"hideOtherApplications:",
        @"unhideAllApplications:", @"terminate:", @"paste:", @"requestProbe:",
        @"openPrivacySettings:", @"requestAuthorization:", @"stopEvidenceRun:",
        @"performClose:", @"performMiniaturize:", @"performZoom:", @"showCompanion:",
        @"showTestTargets:", @"arrangeInFront:"
    ]];
    for (NSMenuItem *item in all_items([f.companion mainMenu])) {
        if (item.isSeparatorItem || item.submenu != nil) continue;
        CHECK([allowed containsObject:NSStringFromSelector(item.action)]);
    }
    for (NSButton *button in @[f.ui.openSettingsButton, f.ui.requestProbeButton,
                               f.ui.requestAuthorizationButton, f.ui.showTargetsButton,
                               f.ui.stopRunButton]) {
        CHECK([allowed containsObject:NSStringFromSelector(button.action)]);
    }

    /* No method on the companion or its views names a grant or reset. */
    NSArray<NSString *> *forbidden =
        @[@"grant", @"reset", @"tcc", @"pam", @"sudo", @"launchctl", @"privilege", @"forceclean"];
    for (Class cls in @[STCompanion.class, STCompanionInterface.class,
                        STAccessibilityTargetField.class, STClipboardTargetView.class,
                        STCapturePatternView.class, STContextPanel.class]) {
        unsigned int count = 0;
        Method *methods = class_copyMethodList(cls, &count);
        for (unsigned int i = 0; i < count; ++i) {
            NSString *name = NSStringFromSelector(method_getName(methods[i]));
            for (NSString *word in forbidden) {
                if (has(name, word)) {
                    free(methods);
                    CHECK(!has(name, word));
                }
            }
        }
        free(methods);
    }

    /* Open Privacy Settings only opens the allowlisted pane for a person. */
    [f.companion selectCapability:ST_COMPANION_CAPABILITY_SCREEN_RECORDING];
    [f.companion openPrivacySettings:nil];
    CHECK(f.opener.urls.count == 1);
    CHECK([f.opener.urls[0].absoluteString
        isEqualToString:@"x-apple.systempreferences:com.apple.settings.PrivacySecurity."
                        @"extension?Privacy_ScreenCapture"]);
    [f.companion selectCapability:ST_COMPANION_CAPABILITY_SHELL_ENVIRONMENT];
    CHECK(f.ui.openSettingsButton.hidden);
    [f.companion openPrivacySettings:nil];
    CHECK(f.opener.urls.count == 1);
    CHECK(has(f.ui.purposeLabel.stringValue, @"never grants permissions"));
}

/* --- Additional interaction cases ------------------------------------------ */

static void gui_and_daemon_results_render_separately(void)
{
    Fixture *f = make_fixture(YES, YES, NO);
    [f feed:@"result capability=4 context=gui-parented result=passed responsible=desktop "
            @"preflight=granted effect=observed cleanup=not-needed"];
    [f feed:@"result capability=4 context=daemon-parented result=denied responsible=desktop "
            @"preflight=denied effect=blocked cleanup=complete"];
    [f.companion selectCapability:ST_COMPANION_CAPABILITY_SCREEN_RECORDING];
    STCompanionInterface *ui = f.ui;
    CHECK([ui.guiPanel.resultTitle.stringValue isEqualToString:@"Passed"]);
    CHECK([ui.daemonPanel.resultTitle.stringValue isEqualToString:@"Denied"]);
    CHECK([cell_text(f.companion, 4, @"gui") isEqualToString:@"Passed"]);
    CHECK([cell_text(f.companion, 4, @"daemon") isEqualToString:@"Denied"]);
    CHECK(has(ui.guiPanel.observed.stringValue, @"Reported responsible process: SpaceTerm app"));
    CHECK(!has(ui.guiPanel.observed.stringValue, @"differs"));
    /* A daemon result attributed to the desktop app is called out. */
    CHECK(has(ui.daemonPanel.observed.stringValue, @"differs from the process"));
    CHECK(has(ui.guiPanel.accessibilityLabel, @"GUI-parented"));
    CHECK(has(ui.daemonPanel.accessibilityLabel, @"daemon-parented"));
    CHECK(has(f.announcer.announcements.lastObject, @"In a server session: Denied"));

    /* Progress shows per context and is announced when a person must act. */
    [f feed:@"progress capability=5 context=gui-parented phase=waiting-for-person"];
    [f.companion selectCapability:ST_COMPANION_CAPABILITY_ACCESSIBILITY];
    CHECK([ui.guiPanel.resultTitle.stringValue isEqualToString:@"Waiting for you"]);
    CHECK([ui.daemonPanel.resultTitle.stringValue isEqualToString:@"Not run"]);
    CHECK(has(f.announcer.announcements.lastObject, @"waiting for you"));
}

static void protocol_errors_close_without_claims(void)
{
    Fixture *early = make_fixture(NO, NO, NO);
    [early.companion attachChannel:early.channel];
    CHECK([early.channel.records.firstObject hasPrefix:@"hello schema=1 run="]);
    [early feed:@"run state=active"];
    CHECK(early.companion.runState == ST_COMPANION_RUN_PROTOCOL_ERROR);
    CHECK(early.channel.closed);
    CHECK(has(early.ui.runDetail.stringValue, @"state is unknown"));

    Fixture *foreign = make_fixture(NO, NO, NO);
    [foreign.companion attachChannel:foreign.channel];
    [foreign feed:[@"welcome schema=1 run=" stringByAppendingString:hex_id(0x99)]];
    CHECK(foreign.companion.runState == ST_COMPANION_RUN_PROTOCOL_ERROR);

    Fixture *f = make_fixture(YES, YES, NO);
    CHECK([f.channel sentPrefix:@"targets capture-window="]);
    [f feed:@"ack seq=9 disposition=accepted"];
    CHECK(f.companion.runState == ST_COMPANION_RUN_PROTOCOL_ERROR);

    Fixture *reopened = make_fixture(YES, YES, NO);
    [reopened feed:@"run state=stopped"];
    [reopened feed:@"run state=active"];
    CHECK(reopened.companion.runState == ST_COMPANION_RUN_PROTOCOL_ERROR);

    /* A probe still in flight when the run ends has no known outcome. */
    Fixture *lost = make_fixture(YES, YES, NO);
    [lost.companion selectCapability:ST_COMPANION_CAPABILITY_MICROPHONE];
    [lost feed:@"result capability=3 context=daemon-parented result=passed responsible=server "
               @"preflight=granted effect=observed cleanup=not-needed"];
    [lost.companion requestProbe:nil];
    [lost feed:@"ack seq=1 disposition=accepted"];
    [lost feed:@"progress capability=3 context=gui-parented phase=running"];
    [lost.companion channelDidCloseWithState:ST_COMPANION_RUN_CONTROLLER_LOST];
    CHECK([lost.ui.guiPanel.resultTitle.stringValue isEqualToString:@"Indeterminate"]);
    CHECK([lost.ui.daemonPanel.resultTitle.stringValue isEqualToString:@"Indeterminate"]);
    CHECK([cell_text(lost.companion, ST_COMPANION_CAPABILITY_MICROPHONE, @"gui")
        isEqualToString:@"Indeterminate"]);
    CHECK(lost.ui.guiPanel.observed.hidden);
    CHECK(!has(lost.ui.daemonPanel.resultTitle.stringValue, @"Passed"));

    /* A probe the controller declined keeps its earlier result. */
    Fixture *declined = make_fixture(YES, YES, NO);
    [declined.companion selectCapability:ST_COMPANION_CAPABILITY_MICROPHONE];
    [declined feed:@"result capability=3 context=daemon-parented result=denied responsible=server "
                   @"preflight=denied effect=blocked cleanup=not-needed"];
    [declined.companion requestProbe:nil];
    [declined feed:@"ack seq=1 disposition=unsupported"];
    [declined feed:@"run state=stopped"];
    CHECK([declined.ui.daemonPanel.resultTitle.stringValue isEqualToString:@"Denied"]);
    CHECK([declined.ui.guiPanel.resultTitle.stringValue isEqualToString:@"Not run"]);

    /* After the controller reports a final state, a closed channel keeps it. */
    Fixture *done = make_fixture(YES, YES, NO);
    [done feed:@"run state=cleanup-unresolved"];
    [done.companion channelDidCloseWithState:ST_COMPANION_RUN_CONTROLLER_LOST];
    CHECK(done.companion.runState == ST_COMPANION_RUN_CLEANUP_UNRESOLVED);
}

static void probe_request_waits_for_acknowledgement(void)
{
    Fixture *f = make_fixture(YES, YES, NO);
    [f.companion selectCapability:ST_COMPANION_CAPABILITY_MICROPHONE];
    [f.companion requestProbe:nil];
    CHECK([f.channel sent:@"request seq=1 kind=probe capability=3\n"]);
    CHECK(!f.ui.requestProbeButton.enabled);
    CHECK([f.ui.guiPanel.resultTitle.stringValue isEqualToString:@"Requested"]);
    CHECK([f.ui.daemonPanel.resultTitle.stringValue isEqualToString:@"Requested"]);
    NSUInteger count = f.channel.records.count;
    [f.companion requestProbe:nil];
    CHECK(f.channel.records.count == count);

    [f feed:@"ack seq=1 disposition=busy"];
    CHECK(f.ui.requestProbeButton.enabled);
    CHECK(has(f.ui.probeStatusLabel.stringValue, @"running another probe"));
    CHECK([f.ui.guiPanel.resultTitle.stringValue isEqualToString:@"Not run"]);

    [f.companion requestProbe:nil];
    CHECK([f.channel sent:@"request seq=2 kind=probe capability=3\n"]);
    [f feed:@"ack seq=2 disposition=accepted"];
    CHECK(has(f.ui.probeStatusLabel.stringValue, @"accepted"));
    [f feed:@"progress capability=3 context=gui-parented phase=running"];
    CHECK([f.ui.guiPanel.resultTitle.stringValue isEqualToString:@"Running"]);

    Fixture *inactive = make_fixture(YES, NO, NO);
    CHECK(!inactive.ui.requestProbeButton.enabled);
    [inactive.companion requestProbe:nil];
    CHECK(![inactive.channel sentPrefix:@"request"]);
}

static void stop_run_has_its_own_confirmation(void)
{
    Fixture *f = make_fixture(YES, YES, YES);
    [f.companion stopEvidenceRun:nil];
    CHECK(f.confirmer.requests.count == 1);
    FakeConfirmation *confirmation = f.confirmer.requests.lastObject;
    CHECK(confirmation.destructive);
    CHECK([confirmation.title isEqualToString:@"Stop the evidence run?"]);
    CHECK(has(confirmation.message, @"does not clean up anything itself"));
    CHECK(![f.channel sentPrefix:@"request"]);
    [f.confirmer answerLast:NO];
    CHECK(![f.channel sentPrefix:@"request"]);
    CHECK(f.companion.runState == ST_COMPANION_RUN_ACTIVE);

    [f.companion stopEvidenceRun:nil];
    [f.confirmer answerLast:YES];
    CHECK([f.channel sent:@"request seq=1 kind=stop-run\n"]);
    CHECK(f.companion.runState == ST_COMPANION_RUN_STOP_REQUESTED);
    CHECK(has(f.ui.runDetail.stringValue, @"controller performs cleanup"));
    CHECK(!f.ui.stopRunButton.enabled);
    CHECK(f.port_state->assesses == 0);
    [f feed:@"ack seq=1 disposition=accepted"];
    [f feed:@"run state=stopping"];
    CHECK([f.ui.runTitle.stringValue isEqualToString:@"Stopping"]);
    [f feed:@"run state=stopped"];
    CHECK([f.ui.runTitle.stringValue isEqualToString:@"Stopped"]);
    CHECK(has(f.ui.runDetail.stringValue, @"reported"));

    Fixture *refused = make_fixture(YES, YES, NO);
    [refused.companion stopEvidenceRun:nil];
    [refused.confirmer answerLast:YES];
    [refused feed:@"ack seq=1 disposition=busy"];
    CHECK(refused.companion.runState == ST_COMPANION_RUN_ACTIVE);
    CHECK(has(refused.ui.runDetail.stringValue, @"did not accept the stop request"));
}

static void voiceover_labels_cover_controls_and_targets(void)
{
    Fixture *f = make_fixture(YES, YES, NO);
    STCompanionInterface *ui = f.ui;
    for (NSButton *button in @[ui.openSettingsButton, ui.requestProbeButton,
                               ui.requestAuthorizationButton, ui.showTargetsButton,
                               ui.stopRunButton]) {
        CHECK(button.title.length != 0);
        CHECK([button.accessibilityTitle isEqualToString:button.title] ||
              button.accessibilityLabel.length != 0);
    }
    CHECK(ui.openSettingsButton.accessibilityHelp.length != 0);
    CHECK(ui.requestProbeButton.accessibilityHelp.length != 0);
    CHECK(ui.requestAuthorizationButton.accessibilityHelp.length != 0);
    CHECK(ui.stopRunButton.accessibilityHelp.length != 0);
    CHECK(ui.operationPopUp.accessibilityLabel.length != 0);
    CHECK(ui.capabilityTable.accessibilityLabel.length != 0);
    for (NSTableColumn *column in ui.capabilityTable.tableColumns) {
        CHECK(column.title.length != 0);
    }
    CHECK(has(ui.guiPanel.accessibilityLabel, @"In a SpaceTerm window"));
    CHECK(has(ui.daemonPanel.accessibilityLabel, @"In a server session"));
    CHECK([ui.guiPanel.accessibilityRole isEqualToString:NSAccessibilityGroupRole]);
    /* Status images are decorative; adjacent text carries the meaning. */
    for (NSImageView *image in @[ui.runSymbol, ui.authorizationSymbol,
                                 ui.guiPanel.resultSymbol, ui.daemonPanel.resultSymbol]) {
        CHECK(!image.isAccessibilityElement);
    }
    STAccessibilityTargetField *field = ui.accessibilityTarget;
    CHECK([field.accessibilityIdentifier isEqualToString:@ST_COMPANION_ACCESSIBILITY_IDENTIFIER]);
    CHECK([field.accessibilityRole isEqualToString:NSAccessibilityTextFieldRole]);
    CHECK([field isAccessibilitySelectorAllowed:@selector(setAccessibilityValue:)]);
    CHECK(field.accessibilityLabel.length != 0);
    CHECK([ui.clipboardTarget.accessibilityLabel isEqualToString:@"Clipboard paste target"]);
    CHECK(ui.clipboardTarget.accessibilityHelp.length != 0);
    NSView *pattern = (NSView *)ui.capturePattern;
    CHECK([pattern.accessibilityRole isEqualToString:NSAccessibilityImageRole]);
    CHECK(pattern.accessibilityLabel.length != 0);
    CHECK([ui.mainWindow.title isEqualToString:@"SpaceTerm Evidence Companion"]);
    CHECK([ui.targetsWindow.title isEqualToString:@"SpaceTerm Evidence Targets"]);

    [f.companion requestProbe:nil];
    CHECK(has(f.announcer.announcements.lastObject, @"Waiting for the evidence controller"));
}

static void light_and_dark_appearances_render(void)
{
    Fixture *f = make_fixture(YES, YES, NO);
    [f feed:[NSString stringWithFormat:@"expect target=capture nonce=%@ deadline-ms=60000",
                                       hex_id(0x71)]];
    NSWindow *main = f.ui.mainWindow;
    NSBitmapImageRep *light = render(main, NSAppearanceNameAqua);
    NSBitmapImageRep *dark = render(main, NSAppearanceNameDarkAqua);
    NSView *content = main.contentView;
    NSPoint corner = NSMakePoint(6, NSHeight(content.bounds) / 2);
    CHECK(luminance(srgb_at(light, content, corner)) > 0.7);
    CHECK(luminance(srgb_at(dark, content, corner)) < 0.3);

    __block NSColor *lightText = nil;
    __block NSColor *darkText = nil;
    [[NSAppearance appearanceNamed:NSAppearanceNameAqua] performAsCurrentDrawingAppearance:^{
        lightText = [f.ui.runTitle.textColor colorUsingColorSpace:NSColorSpace.sRGBColorSpace];
    }];
    [[NSAppearance appearanceNamed:NSAppearanceNameDarkAqua] performAsCurrentDrawingAppearance:^{
        darkText = [f.ui.runTitle.textColor colorUsingColorSpace:NSColorSpace.sRGBColorSpace];
    }];
    CHECK(luminance(lightText) < 0.3);
    CHECK(luminance(darkText) > 0.7);

    /* The capture pattern draws identical pixels in both appearances. */
    NSWindow *targets = f.ui.targetsWindow;
    NSBitmapImageRep *lightTargets = render(targets, NSAppearanceNameAqua);
    NSBitmapImageRep *darkTargets = render(targets, NSAppearanceNameDarkAqua);
    NSView *pattern = (NSView *)f.ui.capturePattern;
    const CGFloat cell = ST_COMPANION_CAPTURE_CELL_POINTS;
    for (int row = 0; row < ST_COMPANION_CAPTURE_ROWS; ++row) {
        for (int column = 0; column < ST_COMPANION_CAPTURE_COLUMNS; ++column) {
            NSPoint point = NSMakePoint(column * cell + cell / 2, row * cell + cell / 2);
            NSColor *a = srgb_at(lightTargets, pattern, point);
            NSColor *b = srgb_at(darkTargets, pattern, point);
            CHECK(fabs(a.redComponent - b.redComponent) < 0.01);
            CHECK(fabs(a.greenComponent - b.greenComponent) < 0.01);
            CHECK(fabs(a.blueComponent - b.blueComponent) < 0.01);
            CHECK(pattern_color(a));
        }
    }
    main.appearance = nil;
    targets.appearance = nil;
}

static void status_symbols_resolve(void)
{
    for (int result = 0; result < ST_COMPANION_RESULT_COUNT; ++result) {
        CHECK(STCompanionSymbol(st_companion_result_copy((st_companion_result)result).symbol));
    }
    for (int state = ST_COMPANION_RUN_DISCONNECTED; state <= ST_COMPANION_RUN_REJECTED; ++state) {
        CHECK(STCompanionSymbol(st_companion_run_copy((st_companion_run_state)state).symbol));
    }
    for (int outcome = 0; outcome < ST_COMPANION_AUTH_OUTCOME_COUNT; ++outcome) {
        CHECK(STCompanionSymbol(
            st_companion_auth_outcome_copy((st_companion_auth_outcome)outcome).symbol));
    }
}

static BOOL visible_in(NSView *view, NSView *container, NSRect area)
{
    NSRect frame = [view convertRect:view.bounds toView:container];
    return !view.hiddenOrHasHiddenAncestor &&
           NSContainsRect(NSInsetRect(area, -0.5, -0.5), frame);
}

static void layout_keeps_every_control_visible(void)
{
    Fixture *f = make_fixture(YES, YES, NO);
    STCompanionInterface *ui = f.ui;
    /* The longest result text: a mismatched process and unresolved cleanup. */
    [f feed:@"result capability=4 context=gui-parented result=cleanup-unresolved "
            @"responsible=other preflight=not-determined effect=not-attempted "
            @"cleanup=unresolved"];
    [f feed:@"result capability=4 context=daemon-parented result=cleanup-unresolved "
            @"responsible=probe preflight=restricted effect=not-attempted cleanup=unresolved"];
    [f.companion selectCapability:ST_COMPANION_CAPABILITY_SCREEN_RECORDING];
    [f.companion selectOperation:ST_COMPANION_OPERATION_RESTART];
    [f.companion requestAuthorization:nil];
    CHECK(has(ui.authorizationTitle.stringValue, @"Adapter unavailable"));

    NSWindow *main = ui.mainWindow;
    NSView *content = main.contentView;
    [content layoutSubtreeIfNeeded];
    CHECK([ui mainContentHeight] <= NSHeight(content.bounds) + 0.5);
    NSScrollView *scroll = ui.purposeLabel.enclosingScrollView;
    CHECK(scroll != nil);
    NSClipView *clip = scroll.contentView;
    for (NSView *view in [ui mainKeyViewOrder]) {
        if (view == ui.openSettingsButton && view.hidden) continue;
        BOOL scrolled = [view isDescendantOf:clip];
        CHECK(scrolled ? visible_in(view, clip, clip.documentVisibleRect)
                       : visible_in(view, content, content.bounds));
    }
    for (NSView *view in @[ui.runDetail, ui.authorizationDetail, ui.probeStatusLabel,
                           ui.guiPanel, ui.daemonPanel]) {
        BOOL scrolled = [view isDescendantOf:clip];
        CHECK(scrolled ? visible_in(view, clip, clip.documentVisibleRect)
                       : visible_in(view, content, content.bounds));
    }
    /* Status text uses the room beside the controls instead of a narrow column. */
    CHECK(NSWidth(ui.authorizationDetail.frame) >= 400);
    CHECK(NSWidth(ui.runDetail.frame) >= 400);

    /* Push buttons keep their natural width. */
    for (NSButton *button in @[ui.showTargetsButton, ui.stopRunButton, ui.requestProbeButton,
                               ui.requestAuthorizationButton]) {
        CHECK(NSWidth(button.frame) <= button.fittingSize.width + 1);
    }
    /* Capability names start at the cell edge, without a symbol gap. */
    NSTableView *table = ui.capabilityTable;
    NSTableCellView *name = (NSTableCellView *)[f.companion
                tableView:table
        viewForTableColumn:[table tableColumnWithIdentifier:@"capability"]
                       row:0];
    CHECK(name.imageView == nil);
    NSTableCellView *result = (NSTableCellView *)[f.companion
                tableView:table
        viewForTableColumn:[table tableColumnWithIdentifier:@"gui"]
                       row:0];
    CHECK(result.imageView != nil && result.imageView.image != nil);

    /* A person's own window size wins over later growth. */
    [NSNotificationCenter.defaultCenter postNotificationName:NSWindowDidEndLiveResizeNotification
                                                      object:main];
    [main setContentSize:NSMakeSize(NSWidth(content.bounds), 600)];
    [f.companion selectCapability:ST_COMPANION_CAPABILITY_ACCESSIBILITY];
    [ui fitMainWindow];
    CHECK(fabs(NSHeight(main.contentView.bounds) - 600) < 0.5);
}

static void linked_adapter_native_unavailable_is_not_a_missing_adapter(void)
{
    /* A linked adapter's NativeUnavailable names macOS, not the adapter. */
    for (int step = 0; step < 2; ++step) {
        Fixture *f = make_fixture(YES, YES, YES);
        if (step == 0) {
            f.port_state->assess_failure = 2;
        } else {
            f.port_state->execute_failure = 2;
        }
        [f.companion selectOperation:ST_COMPANION_OPERATION_START];
        [f.companion requestAuthorization:nil];
        CHECK(f.companion.authorizationFlow->phase == ST_COMPANION_AUTH_FINISHED);
        CHECK(f.port_state->assesses == 1);
        CHECK(!has(f.ui.authorizationTitle.stringValue, @"Adapter unavailable"));
        CHECK(!has(f.ui.authorizationDetail.stringValue, @"adapter is not available"));
        CHECK(has(f.ui.authorizationTitle.stringValue, @"Start Server: macOS"));
        CHECK([f.channel sentPrefix:@"authorization operation=start outcome="]);
        CHECK(![f.channel sent:@"authorization operation=start outcome=adapter-unavailable\n"]);
        CHECK(!has(f.announcer.announcements.lastObject, @"Adapter unavailable"));
    }

    /* Only the composition without an adapter reports a missing adapter. */
    Fixture *unlinked = make_fixture(YES, YES, NO);
    [unlinked.companion selectOperation:ST_COMPANION_OPERATION_START];
    [unlinked.companion requestAuthorization:nil];
    CHECK(has(unlinked.ui.authorizationTitle.stringValue, @"Start Server: Adapter unavailable"));
    CHECK([unlinked.channel sent:@"authorization operation=start outcome=adapter-unavailable\n"]);
}

static void rejected_expectations_keep_the_channel_open(void)
{
    Fixture *f = make_fixture(YES, YES, NO);
    NSString *first = hex_id(0x61);
    NSString *second = hex_id(0x62);
    [f feed:[NSString stringWithFormat:@"expect target=clipboard nonce=%@ deadline-ms=60000",
                                       first]];
    /* The probe copies again before anyone pasted the first marker. */
    f.pasteboard.change = 8;
    [f feed:[NSString stringWithFormat:@"expect target=clipboard nonce=%@ deadline-ms=60000",
                                       second]];
    CHECK(!f.channel.closed);
    CHECK(f.companion.runState == ST_COMPANION_RUN_ACTIVE);
    CHECK([f.channel sent:[NSString stringWithFormat:@"armed target=clipboard nonce=%@\n",
                                                     second]]);

    /* A paste reads the newer marker. */
    f.pasteboard.plain = YES;
    f.pasteboard.text = [@ST_COMPANION_MARKER_PREFIX stringByAppendingString:second];
    [f.ui.clipboardTarget paste:nil];
    CHECK([f.channel sent:[NSString stringWithFormat:@"receipt target=clipboard "
                                                     @"status=accepted nonce=%@\n",
                                                     second]]);

    /* A reused nonce gets a typed refusal instead of a closed channel. */
    [f feed:[NSString stringWithFormat:@"expect target=clipboard nonce=%@ deadline-ms=60000",
                                       first]];
    CHECK([f.channel sent:[NSString stringWithFormat:@"refused target=clipboard nonce=%@ "
                                                     @"reason=duplicate\n",
                                                     first]]);
    CHECK(!f.channel.closed);

    /* So does an expectation that finds every slot armed. */
    for (uint8_t i = 0; i <= ST_COMPANION_MAX_EXPECTATIONS; ++i) {
        [f feed:[NSString stringWithFormat:@"expect target=url nonce=%@ deadline-ms=60000",
                                           hex_id((uint8_t)(0x80 + i))]];
    }
    NSString *overflow = hex_id((uint8_t)(0x80 + ST_COMPANION_MAX_EXPECTATIONS));
    CHECK([f.channel sent:[NSString stringWithFormat:@"refused target=url nonce=%@ reason=full\n",
                                                     overflow]]);
    CHECK(!f.channel.closed);

    /* And one that arrives after the run began stopping. */
    [f feed:@"run state=stopping"];
    [f feed:[NSString stringWithFormat:@"expect target=apple-event nonce=%@ deadline-ms=5000",
                                       hex_id(0x90)]];
    CHECK([f.channel sent:[NSString stringWithFormat:@"refused target=apple-event nonce=%@ "
                                                     @"reason=inactive\n",
                                                     hex_id(0x90)]]);
    CHECK(!f.channel.closed);
    CHECK(f.companion.runState == ST_COMPANION_RUN_STOPPING);

    /* The channel still carries the controller's reports. */
    [f feed:@"result capability=4 context=daemon-parented result=denied responsible=server "
            @"preflight=denied effect=blocked cleanup=complete"];
    CHECK(!f.channel.closed);
    [f.companion selectCapability:ST_COMPANION_CAPABILITY_SCREEN_RECORDING];
    CHECK([f.ui.daemonPanel.resultTitle.stringValue isEqualToString:@"Denied"]);
}

static void targets_window_opens_on_screen_beside_the_companion(void)
{
    Fixture *f = make_fixture(YES, YES, NO);
    STCompanionInterface *ui = f.ui;
    NSWindow *main = ui.mainWindow;
    NSWindow *targets = ui.targetsWindow;
    NSScreen *screen = NSScreen.mainScreen;
    if (screen == nil) return;
    NSRect visible = screen.visibleFrame;
    /* As at launch: the companion window is centered on the main screen. */
    [main center];
    [f feed:[NSString stringWithFormat:@"expect target=capture nonce=%@ deadline-ms=60000",
                                       hex_id(0x46)]];
    CHECK(f.presenter.presented == 1);
    NSRect frame = targets.frame;
    CHECK(NSMinX(frame) >= NSMinX(visible) - 0.5 && NSMaxX(frame) <= NSMaxX(visible) + 0.5);
    CHECK(NSMinY(frame) >= NSMinY(visible) - 0.5 && NSMaxY(frame) <= NSMaxY(visible) + 0.5);
    /* Its top edge lines up with the companion window's when the height
     * allows; otherwise it rests on the bottom of the visible frame. */
    CGFloat top = MIN(NSMaxY(main.frame), NSMaxY(visible));
    if (top - NSHeight(frame) >= NSMinY(visible)) {
        CHECK(fabs(NSMaxY(frame) - top) < 1);
    } else {
        CHECK(fabs(NSMinY(frame) - NSMinY(visible)) < 1);
    }
    /* It sits beside the companion window when the screen has room. */
    if (NSMaxX(main.frame) + 12 + NSWidth(frame) <= NSMaxX(visible)) {
        CHECK(NSMinX(frame) >= NSMaxX(main.frame));
    }

    /* Placement keeps the window inside small and offset visible frames. */
    NSRect small[] = {
        NSMakeRect(0, 0, 900, 600),
        NSMakeRect(-1440, 120, 1440, 860),
        NSMakeRect(NSMinX(main.frame) - 300, NSMinY(main.frame) - 200,
                   NSWidth(main.frame) + 900, NSHeight(main.frame) + 400),
    };
    for (size_t i = 0; i < sizeof small / sizeof small[0]; ++i) {
        [ui placeTargetsWindowWithin:small[i]];
        NSRect placed = targets.frame;
        CHECK(NSMinX(placed) >= NSMinX(small[i]) - 0.5);
        CHECK(NSMaxX(placed) <= NSMaxX(small[i]) + 0.5 || NSWidth(placed) > NSWidth(small[i]));
        CHECK(NSMaxY(placed) <= NSMaxY(small[i]) + 0.5);
        CHECK(NSMinY(placed) >= NSMinY(small[i]) - 0.5 || NSHeight(placed) > NSHeight(small[i]));
    }
    /* With room on the right, it opens right of the companion, tops aligned. */
    [ui placeTargetsWindowWithin:small[2]];
    CHECK(fabs(NSMinX(targets.frame) - (NSMaxX(main.frame) + 12)) < 1);
    CHECK(fabs(NSMaxY(targets.frame) - NSMaxY(main.frame)) < 1);
    [targets setFrameOrigin:frame.origin];

    /* Once a person moves it, later presentations leave it where it is. */
    NSPoint moved = NSMakePoint(NSMinX(visible) + 40, NSMinY(visible) + 40);
    [targets setFrameOrigin:moved];
    [f.companion showTestTargets:nil];
    [f feed:[NSString stringWithFormat:@"expect target=capture nonce=%@ deadline-ms=60000",
                                       hex_id(0x47)]];
    CHECK(NSEqualPoints(targets.frame.origin, moved));
}

static void armed_targets_come_forward_without_activation(void)
{
    /* The production presenter orders the targets window in front of the
     * active app's windows without activating the companion. */
    RecordingWindow *window =
        [[RecordingWindow alloc] initWithContentRect:NSMakeRect(0, 0, 40, 40)
                                           styleMask:NSWindowStyleMaskTitled
                                             backing:NSBackingStoreBuffered
                                               defer:YES];
    window.releasedWhenClosed = NO;
    STWindowPresenter *presenter = [[STWindowPresenter alloc] init];
    [presenter presentWindow:window activate:NO];
    CHECK(window.regardlessCount == 1);
    CHECK(window.orderFrontCount == 0);
    CHECK(window.keyCount == 0);

    /* One click on the inactive targets window reaches the clipboard target. */
    Fixture *f = make_fixture(YES, YES, NO);
    CHECK([f.ui.clipboardTarget acceptsFirstMouse:nil]);
    [f feed:[NSString stringWithFormat:@"expect target=clipboard nonce=%@ deadline-ms=60000",
                                       hex_id(0x45)]];
    CHECK(f.presenter.presented == 1 && f.presenter.activated == 0);
    /* The announcement names the keyboard route to the target. */
    NSString *spoken = f.announcer.announcements.lastObject;
    CHECK(has(spoken, @"Window > Test Targets"));
    CHECK(has(spoken, @"Edit > Paste"));
    /* Window > Test Targets focuses the clipboard target while a marker is expected. */
    [f.companion showTestTargets:nil];
    CHECK(f.presenter.activated == 1);
    CHECK(f.ui.targetsWindow.firstResponder == f.ui.clipboardTarget);
}

/* Collects every accessibility element below root, as an assistive app
 * walking the tree would. */
static void collect_accessibility(id element, NSMutableArray *found, NSUInteger depth)
{
    if (element == nil || depth > 40 || found.count > 4000) return;
    if (![element respondsToSelector:@selector(accessibilityChildren)]) return;
    for (id child in [element accessibilityChildren]) {
        [found addObject:child];
        collect_accessibility(child, found, depth + 1);
    }
}

/* The element an assistive app finds for view: the view or its cell. */
static id exposed_element(NSArray *tree, NSView *view)
{
    if ([tree containsObject:view]) return view;
    if ([view isKindOfClass:[NSControl class]]) {
        NSCell *cell = ((NSControl *)view).cell;
        for (id element in tree) {
            if (element == cell) return element;
        }
    }
    return nil;
}

static void accessibility_tree_exposes_targets_and_controls(void)
{
    Fixture *f = make_fixture(YES, YES, YES);
    STCompanionInterface *ui = f.ui;
    [f.companion selectCapability:ST_COMPANION_CAPABILITY_SCREEN_RECORDING];

    NSMutableArray *targets = [NSMutableArray array];
    collect_accessibility(ui.targetsWindow, targets, 0);
    id field = nil;
    for (id element in targets) {
        if ([element respondsToSelector:@selector(accessibilityIdentifier)] &&
            [[element accessibilityIdentifier]
                isEqualToString:@ST_COMPANION_ACCESSIBILITY_IDENTIFIER]) {
            CHECK(field == nil);
            field = element;
        }
    }
    CHECK(field == ui.accessibilityTarget);
    CHECK([[field accessibilityRole] isEqualToString:NSAccessibilityTextFieldRole]);
    CHECK([field isAccessibilitySelectorAllowed:@selector(setAccessibilityValue:)]);
    CHECK([targets containsObject:ui.clipboardTarget]);
    CHECK([targets containsObject:ui.capturePattern]);

    /* A control reaches assistive apps through its cell, so the cell must be in
     * the tree and carry the control's label. */
    NSMutableArray *main = [NSMutableArray array];
    collect_accessibility(ui.mainWindow, main, 0);
    for (NSView *view in [ui mainKeyViewOrder]) {
        CHECK(exposed_element(main, view) != nil);
    }
    for (NSView *view in @[ui.guiPanel, ui.daemonPanel, ui.runTitle, ui.authorizationTitle]) {
        CHECK(exposed_element(main, view) != nil);
    }
    id popup = exposed_element(main, ui.operationPopUp);
    CHECK([[popup accessibilityLabel] isEqualToString:@"Server operation"]);
}

/* --- Preview --------------------------------------------------------------- */

static int write_previews(const char *directory)
{
    Fixture *f = make_fixture(YES, YES, NO);
    [f feed:@"result capability=4 context=gui-parented result=passed responsible=desktop "
            @"preflight=granted effect=observed cleanup=not-needed"];
    [f feed:@"result capability=4 context=daemon-parented result=denied responsible=server "
            @"preflight=denied effect=blocked cleanup=complete"];
    [f feed:@"result capability=1 context=gui-parented result=passed responsible=desktop "
            @"preflight=granted effect=observed cleanup=complete"];
    [f feed:@"result capability=1 context=daemon-parented result=cancelled responsible=server "
            @"preflight=not-determined effect=not-attempted cleanup=complete"];
    [f feed:@"result capability=3 context=daemon-parented result=unavailable responsible=server "
            @"preflight=unavailable effect=absent cleanup=not-needed"];
    [f feed:@"progress capability=5 context=daemon-parented phase=waiting-for-person"];
    [f feed:[NSString stringWithFormat:@"expect target=capture nonce=%@ deadline-ms=60000",
                                       hex_id(0x71)]];
    [f.companion selectCapability:ST_COMPANION_CAPABILITY_SCREEN_RECORDING];
    [f.companion selectOperation:ST_COMPANION_OPERATION_STOP];
    [f.companion requestAuthorization:nil];
    NSString *base = [NSString stringWithUTF8String:directory];
    struct {
        NSWindow *window;
        NSString *name;
    } windows[] = {{f.ui.mainWindow, @"companion"}, {f.ui.targetsWindow, @"targets"}};
    for (size_t i = 0; i < 2; ++i) {
        for (NSAppearanceName appearance in @[NSAppearanceNameAqua, NSAppearanceNameDarkAqua]) {
            NSBitmapImageRep *rep = render(windows[i].window, appearance);
            NSData *png = [rep representationUsingType:NSBitmapImageFileTypePNG properties:@{}];
            NSString *suffix = [appearance isEqualToString:NSAppearanceNameAqua] ? @"light" : @"dark";
            NSString *path = [base stringByAppendingPathComponent:
                                       [NSString stringWithFormat:@"%@-%@.png", windows[i].name,
                                                                  suffix]];
            if (![png writeToFile:path options:NSDataWritingWithoutOverwriting error:NULL]) {
                return EXIT_FAILURE;
            }
        }
    }
    return EXIT_SUCCESS;
}

/* --- Runner ---------------------------------------------------------------- */

typedef struct {
    const char *name;
    void (*run)(void);
} test_case;

static const test_case cases[] = {
    {"permission_explanation_names_requesting_process",
     permission_explanation_names_requesting_process},
    {"authorization_is_separate_from_destructive_confirmation",
     authorization_is_separate_from_destructive_confirmation},
    {"denied_cancelled_and_unavailable_are_distinct_visible_states",
     denied_cancelled_and_unavailable_are_distinct_visible_states},
    {"keyboard_navigation_reaches_every_probe_and_cleanup_control",
     keyboard_navigation_reaches_every_probe_and_cleanup_control},
    {"synthetic_targets_do_not_access_other_application_content",
     synthetic_targets_do_not_access_other_application_content},
    {"url_and_apple_event_receipts_bind_exact_run", url_and_apple_event_receipts_bind_exact_run},
    {"companion_quit_does_not_claim_daemon_cleanup", companion_quit_does_not_claim_daemon_cleanup},
    {"no_automatic_grant_or_global_reset_operation_exists",
     no_automatic_grant_or_global_reset_operation_exists},
    {"gui_and_daemon_results_render_separately", gui_and_daemon_results_render_separately},
    {"protocol_errors_close_without_claims", protocol_errors_close_without_claims},
    {"probe_request_waits_for_acknowledgement", probe_request_waits_for_acknowledgement},
    {"stop_run_has_its_own_confirmation", stop_run_has_its_own_confirmation},
    {"voiceover_labels_cover_controls_and_targets", voiceover_labels_cover_controls_and_targets},
    {"light_and_dark_appearances_render", light_and_dark_appearances_render},
    {"status_symbols_resolve", status_symbols_resolve},
    {"layout_keeps_every_control_visible", layout_keeps_every_control_visible},
    {"linked_adapter_native_unavailable_is_not_a_missing_adapter",
     linked_adapter_native_unavailable_is_not_a_missing_adapter},
    {"rejected_expectations_keep_the_channel_open", rejected_expectations_keep_the_channel_open},
    {"targets_window_opens_on_screen_beside_the_companion",
     targets_window_opens_on_screen_beside_the_companion},
    {"armed_targets_come_forward_without_activation",
     armed_targets_come_forward_without_activation},
    {"accessibility_tree_exposes_targets_and_controls",
     accessibility_tree_exposes_targets_and_controls},
};

int main(int argc, const char *argv[])
{
    @autoreleasepool {
        [NSApplication sharedApplication];
        if (argc == 3 && strcmp(argv[1], "--preview") == 0) return write_previews(argv[2]);
        if (argc != 1) return 64;
        size_t failed = 0;
        size_t count = sizeof cases / sizeof cases[0];
        for (size_t i = 0; i < count; ++i) {
            case_failed = false;
            @autoreleasepool {
                cases[i].run();
            }
            /* All strings are fixed literals. */
            printf("{\"schema\":1,\"scope\":\"%s\",\"case\":\"%s\",\"passed\":%s}\n", scope,
                   cases[i].name, case_failed ? "false" : "true");
            if (case_failed) ++failed;
        }
        printf("{\"schema\":1,\"scope\":\"%s\",\"summary\":true,\"cases\":%zu,\"failed\":%zu}\n",
               scope, count, failed);
        return failed == 0 ? EXIT_SUCCESS : EXIT_FAILURE;
    }
}
