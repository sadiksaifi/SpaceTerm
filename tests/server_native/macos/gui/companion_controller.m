/* memset_s wipes buffers that briefly held a received marker. */
#define __STDC_WANT_LIB_EXT1__ 1

#import "companion_app.h"

#include <string.h>
#include <unistd.h>

/* Controller for the temporary evidence companion. Every decision about
 * receipts, records, and authorization is made by the portable C layer; this
 * file wires those decisions to the windows and the ports. It performs no
 * action without a person's request except recording receipts and showing
 * what the evidence controller reports. */

@implementation STCompanionPorts
@end

enum {
    ST_COMPANION_MAX_ACCESSIBILITY_VALUE = 128,
    ST_COMPANION_MAX_EVENT_TEXT = 64,
    ST_COMPANION_MAX_URL_TEXT = 256
};

static NSString *const STNameCellIdentifier = @"capability-name-cell";
static NSString *const STResultCellIdentifier = @"capability-result-cell";

static NSString *context_title(st_companion_context context)
{
    return context == ST_COMPANION_CONTEXT_GUI_PARENTED ? @"In a SpaceTerm window"
                                                        : @"In a server session";
}

static NSString *phase_title(st_companion_phase phase)
{
    switch (phase) {
    case ST_COMPANION_PHASE_REQUESTED: return @"Requested";
    case ST_COMPANION_PHASE_QUEUED: return @"Queued";
    case ST_COMPANION_PHASE_RUNNING: return @"Running";
    case ST_COMPANION_PHASE_WAITING_FOR_PERSON: return @"Waiting for you";
    case ST_COMPANION_PHASE_NONE: break;
    }
    return @"";
}

static NSString *phase_detail(st_companion_phase phase)
{
    switch (phase) {
    case ST_COMPANION_PHASE_REQUESTED:
        return @"The companion asked the evidence controller to run this probe.";
    case ST_COMPANION_PHASE_QUEUED:
        return @"The evidence controller queued this probe.";
    case ST_COMPANION_PHASE_RUNNING:
        return @"The probe is running.";
    case ST_COMPANION_PHASE_WAITING_FOR_PERSON:
        return @"The probe is waiting for you. Answer the macOS prompt or use the test "
               @"targets window.";
    case ST_COMPANION_PHASE_NONE: break;
    }
    return @"";
}

static NSString *preflight_text(st_companion_preflight preflight)
{
    switch (preflight) {
    case ST_COMPANION_PREFLIGHT_GRANTED: return @"allowed";
    case ST_COMPANION_PREFLIGHT_DENIED: return @"denied";
    case ST_COMPANION_PREFLIGHT_NOT_DETERMINED: return @"not decided yet";
    case ST_COMPANION_PREFLIGHT_RESTRICTED: return @"restricted";
    case ST_COMPANION_PREFLIGHT_NOT_APPLICABLE: return @"not applicable";
    case ST_COMPANION_PREFLIGHT_UNAVAILABLE: return @"unavailable";
    case ST_COMPANION_PREFLIGHT_UNKNOWN: break;
    }
    return @"unknown";
}

static NSString *observed_text(st_companion_observed observed)
{
    switch (observed) {
    case ST_COMPANION_OBSERVED_EFFECT: return @"observed";
    case ST_COMPANION_OBSERVED_BLOCKED: return @"blocked";
    case ST_COMPANION_OBSERVED_ABSENT: return @"absent";
    case ST_COMPANION_OBSERVED_NOT_ATTEMPTED: return @"not attempted";
    case ST_COMPANION_OBSERVED_UNKNOWN: break;
    }
    return @"unknown";
}

static NSString *cleanup_text(st_companion_cleanup cleanup)
{
    switch (cleanup) {
    case ST_COMPANION_CLEANUP_COMPLETE: return @"complete";
    case ST_COMPANION_CLEANUP_NOT_NEEDED: return @"not needed";
    case ST_COMPANION_CLEANUP_UNRESOLVED: return @"unresolved";
    case ST_COMPANION_CLEANUP_UNKNOWN: break;
    }
    return @"unknown";
}

static NSString *target_title(st_companion_target target)
{
    switch (target) {
    case ST_COMPANION_TARGET_URL: return @"URL";
    case ST_COMPANION_TARGET_APPLE_EVENT: return @"Apple Event";
    case ST_COMPANION_TARGET_ACCESSIBILITY: return @"Accessibility";
    case ST_COMPANION_TARGET_CLIPBOARD: return @"Clipboard";
    case ST_COMPANION_TARGET_CAPTURE: return @"Screen Recording";
    case ST_COMPANION_TARGET_NONE: break;
    }
    return @"";
}

static BOOL run_is_final(st_companion_run_state state)
{
    return state == ST_COMPANION_RUN_STOPPED || state == ST_COMPANION_RUN_CLEANUP_UNRESOLVED ||
           state == ST_COMPANION_RUN_FAILED || state == ST_COMPANION_RUN_CONTROLLER_LOST ||
           state == ST_COMPANION_RUN_PROTOCOL_ERROR || state == ST_COMPANION_RUN_REJECTED;
}

/* Copies a string's UTF-8 bytes when they fit, without NUL termination. */
static BOOL copy_utf8(id value, char *out, size_t capacity, size_t *length)
{
    *length = 0;
    if (![value isKindOfClass:NSString.class]) return NO;
    NSString *text = value;
    NSUInteger used = 0;
    NSRange remaining = NSMakeRange(0, 0);
    if (text.length > capacity) return NO;
    BOOL converted = [text getBytes:out
                          maxLength:capacity
                         usedLength:&used
                           encoding:NSUTF8StringEncoding
                            options:0
                              range:NSMakeRange(0, text.length)
                     remainingRange:&remaining];
    if (!converted || remaining.length != 0) return NO;
    *length = used;
    return YES;
}

@implementation STCompanion {
    STCompanionPorts *_ports;
    STCompanionInterface *_interface;
    id<STCompanionChannel> _channel;
    st_companion_id _run;
    st_companion_run_state _runState;
    st_companion_ledger _ledger;
    st_companion_observation _observations[ST_COMPANION_CAPABILITY_COUNT]
                                          [ST_COMPANION_CONTEXT_COUNT];
    st_companion_auth_flow _authFlow;
    BOOL _authBusy;
    NSString *_authBusyDetail;
    BOOL _confirming;
    BOOL _welcomed;
    uint32_t _nextSequence;
    uint32_t _probeSequence;
    uint32_t _probeCapability;
    uint32_t _stopSequence;
    NSString *_probeNote;
    NSString *_runNote;
    st_companion_operation _operation;
    BOOL _quitting;
    BOOL _batchingRefresh;
    BOOL _limitReported[ST_COMPANION_TARGET_COUNT];
}

- (instancetype)initWithRun:(const st_companion_id *)run ports:(STCompanionPorts *)ports
{
    self = [super init];
    if (self == nil) return nil;
    _ports = ports;
    _run = *run;
    _runState = ST_COMPANION_RUN_DISCONNECTED;
    st_companion_ledger_init(&_ledger);
    st_companion_auth_init(&_authFlow);
    memset(_observations, 0, sizeof _observations);
    _nextSequence = 1;
    _operation = ST_COMPANION_OPERATION_START;
    _interface = [[STCompanionInterface alloc] initWithTarget:self tableSource:self];
    _interface.operationPopUp.target = self;
    _interface.operationPopUp.action = @selector(operationChanged:);

    __weak STCompanion *weakSelf = self;
    _interface.accessibilityTarget.valueHandler = ^(id value) {
        [weakSelf receiveAccessibilityValue:value];
    };
    _interface.clipboardTarget.pasteHandler = ^{
        [weakSelf pasteMarker];
    };

    [_interface.capabilityTable reloadData];
    [self selectCapability:ST_COMPANION_CAPABILITY_PROTECTED_FOLDERS];
    [self refreshAll];
    [_interface sizeWindowsToFit];
    return self;
}

- (STCompanionInterface *)interface { return _interface; }
- (st_companion_run_state)runState { return _runState; }
- (const st_companion_ledger *)ledger { return &_ledger; }
- (const st_companion_auth_flow *)authorizationFlow { return &_authFlow; }

- (uint64_t)now
{
    return _ports.clock();
}

- (const st_companion_authorization_port *)authorizationPort
{
    return _ports.authorization != NULL ? _ports.authorization
                                        : st_companion_authorization_unavailable();
}

/* --- Channel --------------------------------------------------------------- */

- (void)attachChannel:(id<STCompanionChannel>)channel
{
    if (_channel != nil || _runState != ST_COMPANION_RUN_DISCONNECTED) {
        [channel close];
        return;
    }
    _channel = channel;
    _runState = ST_COMPANION_RUN_CONNECTING;
    char record[ST_COMPANION_MAX_RECORD];
    size_t length = st_companion_encode_hello(record, sizeof record, &_run, (uint32_t)getpid());
    [self send:record length:length];
    [self refreshRun];
}

- (void)connectionFailedWithState:(st_companion_run_state)state
{
    if (_channel != nil) return;
    _runState = state;
    [self refreshAll];
}

- (void)send:(const char *)record length:(size_t)length
{
    if (_channel == nil || length == 0) return;
    [_channel sendRecord:record length:length];
}

- (void)failProtocol
{
    id<STCompanionChannel> channel = _channel;
    _channel = nil;
    _probeSequence = 0;
    _stopSequence = 0;
    _runState = ST_COMPANION_RUN_PROTOCOL_ERROR;
    [channel close];
    [self settleInFlightPhases];
    [self refreshAll];
    [self announce:STCompanionString(st_companion_run_copy(_runState).title)];
}

- (void)channelDidCloseWithState:(st_companion_run_state)state
{
    if (_channel == nil) return;
    _channel = nil;
    _probeSequence = 0;
    _stopSequence = 0;
    /* The controller's last report stands. Otherwise nothing is known about
     * whether the run stopped or was cleaned up. */
    if (_runState != ST_COMPANION_RUN_STOPPED &&
        _runState != ST_COMPANION_RUN_CLEANUP_UNRESOLVED && _runState != ST_COMPANION_RUN_FAILED) {
        _runState = state == ST_COMPANION_RUN_PROTOCOL_ERROR ? ST_COMPANION_RUN_PROTOCOL_ERROR
                                                             : ST_COMPANION_RUN_CONTROLLER_LOST;
        [self announce:STCompanionString(st_companion_run_copy(_runState).title)];
    }
    [self settleInFlightPhases];
    [self refreshAll];
}

- (void)channelDidReceiveRecord:(const char *)line length:(size_t)length
{
    if (_channel == nil) return;
    st_companion_inbound inbound;
    if (!st_companion_protocol_parse(line, length, &inbound)) {
        [self failProtocol];
        return;
    }
    if (!_welcomed && inbound.kind != ST_COMPANION_INBOUND_WELCOME) {
        [self failProtocol];
        return;
    }
    BOOL accepted = NO;
    switch (inbound.kind) {
    case ST_COMPANION_INBOUND_WELCOME: accepted = [self receiveWelcome:&inbound]; break;
    case ST_COMPANION_INBOUND_RUN: accepted = [self receiveRunState:inbound.value.run.state]; break;
    case ST_COMPANION_INBOUND_EXPECT: accepted = [self receiveExpectation:&inbound]; break;
    case ST_COMPANION_INBOUND_PROGRESS: accepted = [self receiveProgress:&inbound]; break;
    case ST_COMPANION_INBOUND_RESULT: accepted = [self receiveResult:&inbound]; break;
    case ST_COMPANION_INBOUND_ACK: accepted = [self receiveAck:&inbound]; break;
    }
    if (!accepted) [self failProtocol];
}

- (BOOL)receiveWelcome:(const st_companion_inbound *)inbound
{
    if (_welcomed || !st_companion_id_equal(&inbound->value.welcome.run, &_run)) return NO;
    if (!st_companion_ledger_bind(&_ledger, &_run)) return NO;
    _welcomed = YES;
    char record[ST_COMPANION_MAX_RECORD];
    NSInteger window = _interface.targetsWindow.windowNumber;
    if (window <= 0 || window > UINT32_MAX) return NO;
    size_t length = st_companion_encode_targets(record, sizeof record, (uint32_t)window);
    if (length == 0) return NO;
    [self send:record length:length];
    [self refreshAll];
    return YES;
}

- (BOOL)receiveRunState:(st_companion_run_state)state
{
    if (state == _runState) return YES;
    if (run_is_final(_runState)) return NO;
    if (state == ST_COMPANION_RUN_ACTIVE && (_runState == ST_COMPANION_RUN_STOPPING)) return NO;
    if (state == ST_COMPANION_RUN_ACTIVE && _runState == ST_COMPANION_RUN_STOP_REQUESTED) {
        /* The controller may confirm "active" before it handles the stop. */
        return YES;
    }
    _runState = state;
    if (run_is_final(state)) {
        _probeSequence = 0;
        _stopSequence = 0;
        [self settleInFlightPhases];
    }
    _runNote = nil;
    [self refreshAll];
    [self announce:STCompanionString(st_companion_run_copy(state).title)];
    return YES;
}

/* A well-formed expectation the ledger cannot take is refused on the channel,
 * which stays open. Only a malformed or out-of-order record is a violation. */
- (BOOL)refuseExpectation:(const st_companion_inbound *)inbound
                   reason:(st_companion_refusal)reason
{
    char record[ST_COMPANION_MAX_RECORD];
    size_t length = st_companion_encode_refused(record, sizeof record, inbound->value.expect.target,
                                                &inbound->value.expect.nonce, reason);
    if (length == 0) return NO;
    [self send:record length:length];
    return YES;
}

- (BOOL)receiveExpectation:(const st_companion_inbound *)inbound
{
    if (_runState != ST_COMPANION_RUN_ACTIVE && _runState != ST_COMPANION_RUN_STOP_REQUESTED) {
        return [self refuseExpectation:inbound reason:ST_COMPANION_REFUSAL_INACTIVE];
    }
    st_companion_target target = inbound->value.expect.target;
    int64_t change = 0;
    if (target == ST_COMPANION_TARGET_CLIPBOARD) {
        /* Change count is metadata; the content is not read here. */
        change = (int64_t)[_ports.pasteboard changeCount];
    }
    st_companion_arm_status status = st_companion_ledger_arm(
        &_ledger, target, &inbound->value.expect.nonce, inbound->value.expect.deadline_ms,
        [self now], change);
    if (status != ST_COMPANION_ARM_OK) {
        st_companion_refusal reason = st_companion_refusal_for_arm(status);
        if (reason == 0) return NO;
        return [self refuseExpectation:inbound reason:reason];
    }
    if (target == ST_COMPANION_TARGET_CAPTURE) {
        [_interface.capturePattern showNonce:&inbound->value.expect.nonce];
    }
    [self refreshTargets];
    [_interface placeTargetsWindow];
    /* Ordered in without activation: the person keeps the app they were using. */
    [_ports.presenter presentWindow:_interface.targetsWindow activate:NO];
    /* Draw now so a capture taken after "armed" sees this run's nonce. */
    [_interface.targetsWindow displayIfNeeded];
    char record[ST_COMPANION_MAX_RECORD];
    size_t length =
        st_companion_encode_armed(record, sizeof record, target, &inbound->value.expect.nonce);
    if (length == 0) return NO;
    [self send:record length:length];
    if (target == ST_COMPANION_TARGET_CLIPBOARD) {
        [self announce:@"The capability probe copied a test marker. Choose Window > Test "
                       @"Targets in the companion, or click the clipboard target, then choose "
                       @"Edit > Paste."];
    }
    return YES;
}

- (BOOL)receiveProgress:(const st_companion_inbound *)inbound
{
    uint32_t code = inbound->value.progress.capability;
    if (st_companion_capability_find(code) == NULL) return NO;
    st_companion_observation *observation =
        &_observations[code - 1][inbound->value.progress.context];
    observation->phase = inbound->value.progress.phase;
    [self refreshCapability:code];
    if (observation->phase == ST_COMPANION_PHASE_WAITING_FOR_PERSON) {
        [self announce:[NSString stringWithFormat:@"%@, %@: %@.",
                                                  STCompanionString(
                                                      st_companion_capability_find(code)->name),
                                                  context_title(inbound->value.progress.context),
                                                  phase_detail(observation->phase)]];
    }
    return YES;
}

- (BOOL)receiveResult:(const st_companion_inbound *)inbound
{
    uint32_t code = inbound->value.result.capability;
    const st_companion_capability *capability = st_companion_capability_find(code);
    if (capability == NULL) return NO;
    st_companion_context context = inbound->value.result.context;
    st_companion_observation observation = inbound->value.result.observation;
    observation.phase = ST_COMPANION_PHASE_NONE;
    observation.reported = true;
    _observations[code - 1][context] = observation;
    [self refreshCapability:code];
    st_companion_status_copy copy = st_companion_result_copy(observation.result);
    [self announce:[NSString stringWithFormat:@"%@, %@: %@.", STCompanionString(capability->name),
                                              context_title(context),
                                              STCompanionString(copy.title)]];
    return YES;
}

- (BOOL)receiveAck:(const st_companion_inbound *)inbound
{
    uint32_t sequence = inbound->value.ack.sequence;
    st_companion_ack disposition = inbound->value.ack.disposition;
    if (sequence != 0 && sequence == _probeSequence) {
        uint32_t code = _probeCapability;
        _probeSequence = 0;
        switch (disposition) {
        case ST_COMPANION_ACK_ACCEPTED:
            _probeNote = @"The evidence controller accepted the request.";
            break;
        case ST_COMPANION_ACK_BUSY:
            _probeNote = @"The evidence controller is running another probe. Request this probe "
                         @"again when it finishes.";
            break;
        case ST_COMPANION_ACK_STOPPING:
            _probeNote = @"The evidence run is stopping, so the controller did not run this "
                         @"probe.";
            break;
        case ST_COMPANION_ACK_UNSUPPORTED:
            _probeNote = @"The evidence controller does not run this probe in this evidence run.";
            break;
        }
        if (disposition != ST_COMPANION_ACK_ACCEPTED) [self clearRequestedPhases];
        [self refreshCapability:code];
        [self refreshControls];
        [self announce:_probeNote];
        return YES;
    }
    if (sequence != 0 && sequence == _stopSequence) {
        _stopSequence = 0;
        if (disposition == ST_COMPANION_ACK_BUSY || disposition == ST_COMPANION_ACK_UNSUPPORTED) {
            if (_runState == ST_COMPANION_RUN_STOP_REQUESTED) _runState = ST_COMPANION_RUN_ACTIVE;
            _runNote = @"The evidence controller did not accept the stop request. The evidence "
                       @"run is still active.";
            [self announce:_runNote];
        }
        [self refreshAll];
        return YES;
    }
    return NO;
}

/* --- Table ----------------------------------------------------------------- */

/* Name cells hold text only. Result cells add a decorative status symbol;
 * the text carries the meaning. */
static NSTableCellView *make_cell(BOOL isName)
{
    NSTableCellView *cell = [[NSTableCellView alloc] initWithFrame:NSMakeRect(0, 0, 150, 24)];
    cell.identifier = isName ? STNameCellIdentifier : STResultCellIdentifier;
    NSTextField *text = [NSTextField labelWithString:@""];
    text.translatesAutoresizingMaskIntoConstraints = NO;
    text.lineBreakMode = NSLineBreakByTruncatingTail;
    [text setContentCompressionResistancePriority:NSLayoutPriorityDefaultLow
                                   forOrientation:NSLayoutConstraintOrientationHorizontal];
    [cell addSubview:text];
    cell.textField = text;
    NSMutableArray<NSLayoutConstraint *> *constraints = [NSMutableArray arrayWithArray:@[
        [text.trailingAnchor constraintLessThanOrEqualToAnchor:cell.trailingAnchor constant:-2],
        [text.centerYAnchor constraintEqualToAnchor:cell.centerYAnchor],
    ]];
    if (isName) {
        [constraints addObject:[text.leadingAnchor constraintEqualToAnchor:cell.leadingAnchor
                                                                  constant:2]];
    } else {
        NSImageView *image = [[NSImageView alloc] initWithFrame:NSZeroRect];
        image.translatesAutoresizingMaskIntoConstraints = NO;
        image.accessibilityElement = NO;
        [cell addSubview:image];
        cell.imageView = image;
        [constraints addObjectsFromArray:@[
            [image.leadingAnchor constraintEqualToAnchor:cell.leadingAnchor constant:2],
            [image.centerYAnchor constraintEqualToAnchor:cell.centerYAnchor],
            [image.widthAnchor constraintEqualToConstant:16],
            [text.leadingAnchor constraintEqualToAnchor:image.trailingAnchor constant:6],
        ]];
    }
    [NSLayoutConstraint activateConstraints:constraints];
    return cell;
}

- (NSInteger)numberOfRowsInTableView:(NSTableView *)tableView
{
    (void)tableView;
    return ST_COMPANION_CAPABILITY_COUNT;
}

- (nullable NSView *)tableView:(NSTableView *)tableView
            viewForTableColumn:(nullable NSTableColumn *)column
                           row:(NSInteger)row
{
    if (row < 0 || row >= ST_COMPANION_CAPABILITY_COUNT || column == nil) return nil;
    uint32_t code = (uint32_t)row + 1;
    const st_companion_capability *capability = st_companion_capability_find(code);
    NSString *identifier = column.identifier;
    BOOL isName = [identifier isEqualToString:@"capability"];
    NSTableCellView *cell = [tableView makeViewWithIdentifier:isName ? STNameCellIdentifier
                                                                     : STResultCellIdentifier
                                                        owner:self];
    if (cell == nil) cell = make_cell(isName);
    if (isName) {
        cell.textField.stringValue = STCompanionString(capability->name);
        cell.textField.font = [NSFont preferredFontForTextStyle:NSFontTextStyleBody options:@{}];
        return cell;
    }
    st_companion_context context = [identifier isEqualToString:@"gui"]
                                       ? ST_COMPANION_CONTEXT_GUI_PARENTED
                                       : ST_COMPANION_CONTEXT_DAEMON_PARENTED;
    const st_companion_observation *observation = &_observations[row][context];
    NSString *title;
    st_companion_status_copy copy;
    if (observation->phase != ST_COMPANION_PHASE_NONE) {
        copy = st_companion_result_copy(ST_COMPANION_RESULT_PENDING);
        title = phase_title(observation->phase);
    } else {
        copy = st_companion_result_copy(observation->result);
        title = STCompanionString(copy.title);
    }
    cell.imageView.image = STCompanionSymbol(copy.symbol);
    cell.imageView.contentTintColor = STCompanionToneColor(copy.tone);
    cell.textField.stringValue = title;
    cell.textField.font = [NSFont preferredFontForTextStyle:NSFontTextStyleBody options:@{}];
    return cell;
}

- (void)tableViewSelectionDidChange:(NSNotification *)notification
{
    (void)notification;
    [self refreshDetail];
    [self refreshControls];
}

- (void)selectCapability:(uint32_t)code
{
    if (st_companion_capability_find(code) == NULL) return;
    NSTableView *table = _interface.capabilityTable;
    [table selectRowIndexes:[NSIndexSet indexSetWithIndex:code - 1] byExtendingSelection:NO];
    [table scrollRowToVisible:(NSInteger)code - 1];
    [self refreshDetail];
    [self refreshControls];
}

- (uint32_t)selectedCapability
{
    NSInteger row = _interface.capabilityTable.selectedRow;
    if (row < 0 || row >= ST_COMPANION_CAPABILITY_COUNT) return 0;
    return (uint32_t)row + 1;
}

- (void)selectOperation:(st_companion_operation)operation
{
    if (operation < ST_COMPANION_OPERATION_START || operation > ST_COMPANION_OPERATION_RESTART) {
        return;
    }
    _operation = operation;
    [_interface.operationPopUp selectItemWithTag:operation];
}

- (void)operationChanged:(id)sender
{
    (void)sender;
    NSInteger tag = _interface.operationPopUp.selectedTag;
    if (tag >= ST_COMPANION_OPERATION_START && tag <= ST_COMPANION_OPERATION_RESTART) {
        _operation = (st_companion_operation)tag;
    }
}

/* --- Actions --------------------------------------------------------------- */

- (BOOL)canRequestProbe
{
    return _channel != nil && _welcomed && st_companion_run_accepts_requests(_runState) &&
           _probeSequence == 0 && _stopSequence == 0 && [self selectedCapability] != 0 &&
           _nextSequence <= ST_COMPANION_MAX_SEQUENCE;
}

- (BOOL)canStopRun
{
    return _channel != nil && _welcomed && st_companion_run_accepts_requests(_runState) &&
           _stopSequence == 0 && !_confirming && _nextSequence <= ST_COMPANION_MAX_SEQUENCE;
}

- (BOOL)canRequestAuthorization
{
    return !_authBusy && !_confirming;
}

- (BOOL)canOpenSettings
{
    return STCompanionSettingsURL(st_companion_capability_find([self selectedCapability])) != nil;
}

- (IBAction)requestProbe:(nullable id)sender
{
    (void)sender;
    if (![self canRequestProbe]) return;
    uint32_t code = [self selectedCapability];
    char record[ST_COMPANION_MAX_RECORD];
    size_t length = st_companion_encode_probe_request(record, sizeof record, _nextSequence, code);
    if (length == 0) return;
    _probeSequence = _nextSequence++;
    _probeCapability = code;
    for (size_t context = 0; context < ST_COMPANION_CONTEXT_COUNT; ++context) {
        _observations[code - 1][context].phase = ST_COMPANION_PHASE_REQUESTED;
    }
    _probeNote = @"Requested. Waiting for the evidence controller.";
    [self send:record length:length];
    [self refreshCapability:code];
    [self refreshControls];
    [self announce:_probeNote];
}

- (IBAction)openPrivacySettings:(nullable id)sender
{
    (void)sender;
    NSURL *url = STCompanionSettingsURL(st_companion_capability_find([self selectedCapability]));
    if (url == nil) return;
    [_ports.settingsOpener openSettingsURL:url];
}

- (IBAction)stopEvidenceRun:(nullable id)sender
{
    (void)sender;
    if (![self canStopRun]) return;
    _confirming = YES;
    [self refreshControls];
    __weak STCompanion *weakSelf = self;
    [_ports.confirmer
        confirmWithTitle:@"Stop the evidence run?"
                 message:@"The evidence controller stops its probes and removes only the "
                         @"resources its cleanup receipt records. It reports whether cleanup "
                         @"completed. The companion does not clean up anything itself."
            confirmTitle:@"Stop Evidence Run"
             destructive:YES
                  window:_interface.mainWindow
              completion:^(BOOL confirmed) {
                  [weakSelf finishStopConfirmation:confirmed];
              }];
}

- (void)finishStopConfirmation:(BOOL)confirmed
{
    _confirming = NO;
    if (confirmed && _channel != nil && st_companion_run_accepts_requests(_runState) &&
        _stopSequence == 0) {
        char record[ST_COMPANION_MAX_RECORD];
        size_t length = st_companion_encode_stop_request(record, sizeof record, _nextSequence);
        if (length != 0) {
            _stopSequence = _nextSequence++;
            _runState = ST_COMPANION_RUN_STOP_REQUESTED;
            _runNote = nil;
            [self send:record length:length];
            [self announce:STCompanionString(st_companion_run_copy(_runState).title)];
        }
    }
    [self refreshAll];
}

- (IBAction)showCompanion:(nullable id)sender
{
    (void)sender;
    [_ports.presenter presentWindow:_interface.mainWindow activate:YES];
}

- (IBAction)showTestTargets:(nullable id)sender
{
    (void)sender;
    [_interface placeTargetsWindow];
    [_ports.presenter presentWindow:_interface.targetsWindow activate:YES];
    /* Edit > Paste lands in the clipboard target without another click. */
    [_interface.targetsWindow makeFirstResponder:_interface.clipboardTarget];
}

/* --- Authorization --------------------------------------------------------- */

- (IBAction)requestAuthorization:(nullable id)sender
{
    (void)sender;
    if (![self canRequestAuthorization]) return;
    [self operationChanged:nil];
    st_companion_operation operation = _operation;
    const st_companion_authorization_port *port = [self authorizationPort];
    uint64_t now = [self now];
    [self authorizationBusy:@"Checking the server's current state."];
    __block st_companion_auth_flow flow = _authFlow;
    __weak STCompanion *weakSelf = self;
    [_ports.authorizationExecutor
        perform:^{
            st_companion_auth_begin(&flow, port, operation, now);
        }
        then:^{
            [weakSelf authorizationStepFinished:flow];
        }];
}

- (void)authorizationBusy:(NSString *)detail
{
    _authBusy = YES;
    _authBusyDetail = detail;
    [self refreshAuthorization];
    [self refreshControls];
}

- (void)authorizationStepFinished:(st_companion_auth_flow)flow
{
    _authFlow = flow;
    _authBusy = NO;
    _authBusyDetail = nil;
    switch (_authFlow.phase) {
    case ST_COMPANION_AUTH_CONFIRMING: [self confirmAuthorization]; break;
    case ST_COMPANION_AUTH_AUTHORIZING: [self executeAuthorization]; break;
    case ST_COMPANION_AUTH_FINISHED: [self finishAuthorization]; break;
    case ST_COMPANION_AUTH_IDLE: break;
    }
    [self refreshAuthorization];
    [self refreshControls];
}

/* The companion's own confirmation. It states what will end and that macOS
 * asks separately; it never stands in for the macOS authorization prompt. */
- (void)confirmAuthorization
{
    char message[ST_COMPANION_MAX_CONFIRMATION];
    if (st_companion_auth_confirmation_message(&_authFlow, message, sizeof message) == 0) return;
    _confirming = YES;
    __weak STCompanion *weakSelf = self;
    [_ports.confirmer confirmWithTitle:STCompanionString(
                                           st_companion_auth_confirmation_title(&_authFlow))
                               message:STCompanionString(message)
                          confirmTitle:STCompanionString(
                                           st_companion_auth_confirmation_button(&_authFlow))
                           destructive:YES
                                window:_interface.mainWindow
                            completion:^(BOOL confirmed) {
                                [weakSelf answerAuthorizationConfirmation:confirmed];
                            }];
}

- (void)answerAuthorizationConfirmation:(BOOL)confirmed
{
    _confirming = NO;
    const st_companion_authorization_port *port = [self authorizationPort];
    uint64_t now = [self now];
    [self authorizationBusy:confirmed ? @"Checking that the assessment is still current."
                                      : @"Recording your answer."];
    __block st_companion_auth_flow flow = _authFlow;
    __weak STCompanion *weakSelf = self;
    [_ports.authorizationExecutor
        perform:^{
            st_companion_auth_confirm(&flow, port, confirmed, now);
        }
        then:^{
            [weakSelf authorizationStepFinished:flow];
        }];
}

- (void)executeAuthorization
{
    const st_companion_authorization_port *port = [self authorizationPort];
    uint64_t now = [self now];
    [self authorizationBusy:@"Waiting for macOS authorization. Respond in the macOS dialog."];
    [self announce:@"Waiting for macOS authorization."];
    __block st_companion_auth_flow flow = _authFlow;
    __weak STCompanion *weakSelf = self;
    [_ports.authorizationExecutor
        perform:^{
            st_companion_auth_execute(&flow, port, now);
        }
        then:^{
            [weakSelf authorizationStepFinished:flow];
        }];
}

- (void)finishAuthorization
{
    char record[ST_COMPANION_MAX_RECORD];
    size_t length = st_companion_encode_authorization(record, sizeof record, _authFlow.operation,
                                                      _authFlow.outcome);
    if (_welcomed) [self send:record length:length];
    st_companion_status_copy copy = st_companion_auth_outcome_copy(_authFlow.outcome);
    [self announce:[NSString stringWithFormat:@"%@: %@.",
                                              STCompanionString(st_companion_operation_title(
                                                  _authFlow.operation)),
                                              STCompanionString(copy.title)]];
}

/* --- Synthetic target receipts --------------------------------------------- */

- (void)report:(st_companion_target)target
        status:(st_companion_receipt_status)status
         nonce:(const st_companion_id *)nonce
{
    if (status == ST_COMPANION_RECEIPT_LIMIT) {
        if (_limitReported[target]) {
            [self refreshTargets];
            return;
        }
        _limitReported[target] = YES;
    }
    if (_welcomed) {
        char record[ST_COMPANION_MAX_RECORD];
        size_t length = st_companion_encode_receipt(record, sizeof record, target, status,
                                                    st_companion_receipt_has_nonce(status) ? nonce
                                                                                           : NULL);
        [self send:record length:length];
    }
    [self refreshTargets];
    if (status == ST_COMPANION_RECEIPT_ACCEPTED) {
        [self announce:[NSString stringWithFormat:@"%@ receipt accepted.", target_title(target)]];
    }
}

- (void)handleURLEvent:(NSAppleEventDescriptor *)event
        withReplyEvent:(NSAppleEventDescriptor *)reply
{
    (void)reply;
    char text[ST_COMPANION_MAX_URL_TEXT];
    size_t length = 0;
    id value = [event paramDescriptorForKeyword:keyDirectObject].stringValue;
    BOOL fits = copy_utf8(value, text, sizeof text, &length);
    st_companion_id nonce = {{0}};
    st_companion_receipt_status status = st_companion_ledger_receive_url(
        &_ledger, fits ? text : NULL, fits ? length : 0, [self now], &nonce);
    memset_s(text, sizeof text, 0, sizeof text);
    [self report:ST_COMPANION_TARGET_URL status:status nonce:&nonce];
}

- (void)handleReceiptEvent:(NSAppleEventDescriptor *)event
            withReplyEvent:(NSAppleEventDescriptor *)reply
{
    char run[ST_COMPANION_MAX_EVENT_TEXT];
    char nonceText[ST_COMPANION_MAX_EVENT_TEXT];
    size_t runLength = 0;
    size_t nonceLength = 0;
    BOOL runFits = copy_utf8([event paramDescriptorForKeyword:ST_COMPANION_EVENT_RUN].stringValue,
                             run, sizeof run, &runLength);
    BOOL nonceFits =
        copy_utf8([event paramDescriptorForKeyword:ST_COMPANION_EVENT_NONCE].stringValue,
                  nonceText, sizeof nonceText, &nonceLength);
    st_companion_id nonce = {{0}};
    st_companion_receipt_status status = st_companion_ledger_receive_event(
        &_ledger, runFits ? run : NULL, runFits ? runLength : 0, nonceFits ? nonceText : NULL,
        nonceFits ? nonceLength : 0, [self now], &nonce);

    if (reply.descriptorType != typeNull) {
        [reply setParamDescriptor:[NSAppleEventDescriptor
                                      descriptorWithString:STCompanionString(
                                                               st_companion_receipt_token(status))]
                       forKeyword:keyDirectObject];
        if (status == ST_COMPANION_RECEIPT_ACCEPTED) {
            char hex[ST_COMPANION_ID_HEX + 1];
            st_companion_id_format(&_ledger.run, hex);
            [reply setParamDescriptor:[NSAppleEventDescriptor
                                          descriptorWithString:STCompanionString(hex)]
                           forKeyword:ST_COMPANION_EVENT_RUN];
            st_companion_id_format(&nonce, hex);
            [reply setParamDescriptor:[NSAppleEventDescriptor
                                          descriptorWithString:STCompanionString(hex)]
                           forKeyword:ST_COMPANION_EVENT_NONCE];
        } else {
            [reply setParamDescriptor:[NSAppleEventDescriptor descriptorWithInt32:errAEEventFailed]
                           forKeyword:keyErrorNumber];
        }
    }
    [self report:ST_COMPANION_TARGET_APPLE_EVENT status:status nonce:&nonce];
}

- (void)receiveAccessibilityValue:(id)value
{
    char text[ST_COMPANION_MAX_ACCESSIBILITY_VALUE];
    size_t length = 0;
    BOOL fits = copy_utf8(value, text, sizeof text, &length);
    st_companion_id nonce = {{0}};
    st_companion_receipt_status status = st_companion_ledger_receive_accessibility(
        &_ledger, fits ? text : NULL, fits ? length : 0, [self now], &nonce);
    memset_s(text, sizeof text, 0, sizeof text);
    [self report:ST_COMPANION_TARGET_ACCESSIBILITY status:status nonce:&nonce];
}

/* Runs only for Edit > Paste in the clipboard target. Without an armed,
 * unexpired marker the pasteboard is not touched at all; with one, only
 * metadata is checked before a single bounded plain-text read. */
- (void)pasteMarker
{
    uint64_t now = [self now];
    st_companion_id nonce = {{0}};
    st_companion_receipt_status status;
    if (!st_companion_ledger_clipboard_armed(&_ledger, now)) {
        status = st_companion_ledger_clipboard_gate(&_ledger, -1, false, now, &nonce);
        [self report:ST_COMPANION_TARGET_CLIPBOARD status:status nonce:&nonce];
        return;
    }
    id<STCompanionPasteboard> pasteboard = _ports.pasteboard;
    int64_t change = (int64_t)[pasteboard changeCount];
    bool plain = [pasteboard holdsOnlyPlainText];
    status = st_companion_ledger_clipboard_gate(&_ledger, change, plain, now, &nonce);
    if (status == ST_COMPANION_RECEIPT_ACCEPTED) {
        uint8_t bytes[ST_COMPANION_MARKER_LENGTH];
        size_t length = 0;
        BOOL copied = [pasteboard copyPlainText:bytes capacity:sizeof bytes length:&length];
        status = st_companion_ledger_receive_clipboard(&_ledger, copied ? bytes : NULL,
                                                       copied ? length : 0, now, &nonce);
        memset_s(bytes, sizeof bytes, 0, sizeof bytes);
    }
    [self report:ST_COMPANION_TARGET_CLIPBOARD status:status nonce:&nonce];
}

/* --- Quit ------------------------------------------------------------------ */

- (void)sendClosing
{
    char record[ST_COMPANION_MAX_RECORD];
    size_t length = st_companion_encode_closing(record, sizeof record);
    [self send:record length:length];
}

- (NSApplicationTerminateReply)applicationShouldTerminate:(NSApplication *)sender
{
    (void)sender;
    if (_quitting) return NSTerminateNow;
    if (_confirming) return NSTerminateCancel;
    st_companion_quit_plan plan = st_companion_plan_quit(_runState);
    NSString *message = STCompanionString(plan.message);
    if (_authBusy) {
        message = [message stringByAppendingString:@" A server authorization request is still "
                                                   @"in progress. The companion will not show "
                                                   @"its result."];
    }
    if (!plan.confirm && !_authBusy) {
        _quitting = YES;
        [self sendClosing];
        return NSTerminateNow;
    }
    _confirming = YES;
    [self refreshControls];
    __weak STCompanion *weakSelf = self;
    [_ports.confirmer confirmWithTitle:STCompanionString(plan.title)
                               message:message
                          confirmTitle:@"Quit"
                           destructive:NO
                                window:_interface.mainWindow
                            completion:^(BOOL confirmed) {
                                [weakSelf answerQuit:confirmed];
                            }];
    return NSTerminateLater;
}

- (void)answerQuit:(BOOL)confirmed
{
    _confirming = NO;
    if (confirmed) {
        _quitting = YES;
        /* Says only that the companion exits. The run state stays as the
         * controller last reported it. */
        [self sendClosing];
    }
    [self refreshControls];
    _ports.quitReply(confirmed);
}

/* --- Menus ----------------------------------------------------------------- */

static NSMenuItem *menu_item(NSString *title, SEL action, NSString *key, id target, NSInteger tag)
{
    NSMenuItem *item = [[NSMenuItem alloc] initWithTitle:title action:action keyEquivalent:key];
    item.target = target;
    item.tag = tag;
    return item;
}

static NSString *action_title(st_companion_action action)
{
    return STCompanionString(st_companion_action_find(action)->title);
}

- (NSMenu *)mainMenu
{
    NSString *name = @"SpaceTerm Evidence Companion";
    NSMenu *bar = [[NSMenu alloc] initWithTitle:@"Main Menu"];

    NSMenu *app = [[NSMenu alloc] initWithTitle:name];
    [app addItem:menu_item([@"About " stringByAppendingString:name],
                           @selector(orderFrontStandardAboutPanel:), @"", nil, 0)];
    [app addItem:NSMenuItem.separatorItem];
    [app addItem:menu_item([@"Hide " stringByAppendingString:name], @selector(hide:), @"h", nil,
                           0)];
    NSMenuItem *others =
        menu_item(@"Hide Others", @selector(hideOtherApplications:), @"h", nil, 0);
    others.keyEquivalentModifierMask = NSEventModifierFlagCommand | NSEventModifierFlagOption;
    [app addItem:others];
    [app addItem:menu_item(@"Show All", @selector(unhideAllApplications:), @"", nil, 0)];
    [app addItem:NSMenuItem.separatorItem];
    [app addItem:menu_item(action_title(ST_COMPANION_ACTION_QUIT), @selector(terminate:), @"q",
                           nil, ST_COMPANION_ACTION_QUIT)];

    NSMenu *file = [[NSMenu alloc] initWithTitle:@"File"];
    [file addItem:menu_item(@"Close", @selector(performClose:), @"w", nil, 0)];

    NSMenu *edit = [[NSMenu alloc] initWithTitle:@"Edit"];
    [edit addItem:menu_item(action_title(ST_COMPANION_ACTION_PASTE_MARKER), @selector(paste:),
                            @"v", nil, ST_COMPANION_ACTION_PASTE_MARKER)];

    NSMenu *evidence = [[NSMenu alloc] initWithTitle:@"Evidence"];
    [evidence addItem:menu_item(action_title(ST_COMPANION_ACTION_REQUEST_PROBE),
                                @selector(requestProbe:), @"r", self,
                                ST_COMPANION_ACTION_REQUEST_PROBE)];
    [evidence addItem:menu_item(action_title(ST_COMPANION_ACTION_OPEN_PRIVACY_SETTINGS),
                                @selector(openPrivacySettings:), @"", self,
                                ST_COMPANION_ACTION_OPEN_PRIVACY_SETTINGS)];
    [evidence addItem:NSMenuItem.separatorItem];
    [evidence addItem:menu_item(action_title(ST_COMPANION_ACTION_REQUEST_AUTHORIZATION),
                                @selector(requestAuthorization:), @"", self,
                                ST_COMPANION_ACTION_REQUEST_AUTHORIZATION)];
    [evidence addItem:NSMenuItem.separatorItem];
    [evidence addItem:menu_item(action_title(ST_COMPANION_ACTION_STOP_RUN),
                                @selector(stopEvidenceRun:), @"", self,
                                ST_COMPANION_ACTION_STOP_RUN)];

    NSMenu *window = [[NSMenu alloc] initWithTitle:@"Window"];
    [window addItem:menu_item(@"Minimize", @selector(performMiniaturize:), @"m", nil, 0)];
    [window addItem:menu_item(@"Zoom", @selector(performZoom:), @"", nil, 0)];
    [window addItem:NSMenuItem.separatorItem];
    [window addItem:menu_item(action_title(ST_COMPANION_ACTION_SHOW_COMPANION),
                              @selector(showCompanion:), @"1", self,
                              ST_COMPANION_ACTION_SHOW_COMPANION)];
    [window addItem:menu_item(@"Test Targets", @selector(showTestTargets:), @"2", self,
                              ST_COMPANION_ACTION_SHOW_TARGETS)];
    [window addItem:NSMenuItem.separatorItem];
    [window addItem:menu_item(@"Bring All to Front", @selector(arrangeInFront:), @"", nil, 0)];

    for (NSMenu *submenu in @[app, file, edit, evidence, window]) {
        NSMenuItem *holder = [[NSMenuItem alloc] initWithTitle:submenu.title
                                                        action:NULL
                                                 keyEquivalent:@""];
        holder.submenu = submenu;
        [bar addItem:holder];
    }
    NSApp.windowsMenu = window;
    return bar;
}

- (BOOL)validateMenuItem:(NSMenuItem *)item
{
    switch (item.tag) {
    case ST_COMPANION_ACTION_REQUEST_PROBE: return [self canRequestProbe];
    case ST_COMPANION_ACTION_OPEN_PRIVACY_SETTINGS: return [self canOpenSettings];
    case ST_COMPANION_ACTION_REQUEST_AUTHORIZATION: return [self canRequestAuthorization];
    case ST_COMPANION_ACTION_STOP_RUN: return [self canStopRun];
    case ST_COMPANION_ACTION_SHOW_COMPANION:
    case ST_COMPANION_ACTION_SHOW_TARGETS: return YES;
    default: return NO;
    }
}

/* --- Presentation ---------------------------------------------------------- */

- (void)announce:(NSString *)text
{
    if (text.length != 0) [_ports.announcer announce:text];
}

/* When the run ends or the connection fails, a probe that was requested or
 * running has no known outcome. Its earlier result no longer describes it. */
- (void)settleInFlightPhases
{
    NSMutableIndexSet *rows = [NSMutableIndexSet indexSet];
    for (size_t code = 0; code < ST_COMPANION_CAPABILITY_COUNT; ++code) {
        for (size_t context = 0; context < ST_COMPANION_CONTEXT_COUNT; ++context) {
            st_companion_observation *observation = &_observations[code][context];
            if (observation->phase == ST_COMPANION_PHASE_NONE) continue;
            memset(observation, 0, sizeof *observation);
            observation->phase = ST_COMPANION_PHASE_NONE;
            observation->result = ST_COMPANION_RESULT_INDETERMINATE;
            observation->reported = false;
            [rows addIndex:code];
        }
    }
    if (rows.count == 0) return;
    NSTableView *table = _interface.capabilityTable;
    [table reloadDataForRowIndexes:rows
                     columnIndexes:[NSIndexSet indexSetWithIndexesInRange:
                                                   NSMakeRange(0, (NSUInteger)table.numberOfColumns)]];
}

- (void)clearRequestedPhases
{
    /* Reload only changed rows. A full reload resets the selection, which
     * would move keyboard and VoiceOver focus away from the person's row. */
    NSMutableIndexSet *rows = [NSMutableIndexSet indexSet];
    for (size_t code = 0; code < ST_COMPANION_CAPABILITY_COUNT; ++code) {
        for (size_t context = 0; context < ST_COMPANION_CONTEXT_COUNT; ++context) {
            if (_observations[code][context].phase == ST_COMPANION_PHASE_REQUESTED) {
                _observations[code][context].phase = ST_COMPANION_PHASE_NONE;
                [rows addIndex:code];
            }
        }
    }
    if (rows.count == 0) return;
    NSTableView *table = _interface.capabilityTable;
    [table reloadDataForRowIndexes:rows
                     columnIndexes:[NSIndexSet indexSetWithIndexesInRange:
                                                   NSMakeRange(0, (NSUInteger)table.numberOfColumns)]];
}

- (void)refreshAll
{
    _batchingRefresh = YES;
    [self refreshRun];
    [self refreshDetail];
    [self refreshAuthorization];
    [self refreshTargets];
    [self refreshControls];
    _batchingRefresh = NO;
    [_interface fitMainWindow];
}

- (void)refreshCapability:(uint32_t)code
{
    NSTableView *table = _interface.capabilityTable;
    [table reloadDataForRowIndexes:[NSIndexSet indexSetWithIndex:code - 1]
                     columnIndexes:[NSIndexSet indexSetWithIndexesInRange:
                                                   NSMakeRange(0, (NSUInteger)table.numberOfColumns)]];
    if (code == [self selectedCapability]) [self refreshDetail];
}

static void show_status(NSImageView *symbol, NSTextField *title, NSTextField *detail,
                        st_companion_status_copy copy, NSString *extra)
{
    symbol.image = STCompanionSymbol(copy.symbol);
    symbol.contentTintColor = STCompanionToneColor(copy.tone);
    title.stringValue = STCompanionString(copy.title);
    NSString *text = STCompanionString(copy.detail);
    if (extra.length != 0) {
        text = text.length == 0 ? extra : [text stringByAppendingFormat:@" %@", extra];
    }
    detail.stringValue = text;
}

- (void)refreshRun
{
    [self showRun];
    if (!_batchingRefresh) [_interface fitMainWindow];
}

- (void)showRun
{
    show_status(_interface.runSymbol, _interface.runTitle, _interface.runDetail,
                st_companion_run_copy(_runState), _runNote);
}

- (void)refreshPanel:(STContextPanel *)panel
          capability:(const st_companion_capability *)capability
             context:(st_companion_context)context
{
    const st_companion_context_copy *copy = &capability->contexts[context];
    const st_companion_observation *observation = &_observations[capability->code - 1][context];
    panel.whoAsks.stringValue =
        copy->process == ST_COMPANION_PROCESS_NONE
            ? @"No System Permission applies"
            : [@"Requesting process: " stringByAppendingString:STCompanionString(copy->requester)];
    panel.explanation.stringValue = STCompanionString(copy->explanation);

    st_companion_status_copy status;
    if (observation->phase != ST_COMPANION_PHASE_NONE) {
        status = st_companion_result_copy(ST_COMPANION_RESULT_PENDING);
        status.detail = NULL;
        show_status(panel.resultSymbol, panel.resultTitle, panel.resultDetail, status,
                    phase_detail(observation->phase));
        panel.resultTitle.stringValue = phase_title(observation->phase);
    } else {
        status = st_companion_result_copy(observation->result);
        show_status(panel.resultSymbol, panel.resultTitle, panel.resultDetail, status, nil);
    }

    if (!observation->reported) {
        panel.observed.stringValue = @"";
        panel.observed.hidden = YES;
    } else {
        NSString *text = [NSString
            stringWithFormat:@"Reported responsible process: %@. Permission check before the "
                             @"probe: %@. Effect: %@. Cleanup: %@.",
                             STCompanionString(st_companion_process_name(observation->responsible)),
                             preflight_text(observation->preflight),
                             observed_text(observation->effect),
                             cleanup_text(observation->cleanup)];
        if (copy->process != ST_COMPANION_PROCESS_NONE &&
            observation->responsible != ST_COMPANION_PROCESS_UNKNOWN &&
            observation->responsible != copy->process) {
            text = [text stringByAppendingString:@" This differs from the process macOS is "
                                                 @"expected to ask."];
        }
        panel.observed.stringValue = text;
        panel.observed.hidden = NO;
    }
    panel.accessibilityLabel = [NSString
        stringWithFormat:@"%@, %@", context_title(context),
                         context == ST_COMPANION_CONTEXT_GUI_PARENTED ? @"GUI-parented shell"
                                                                      : @"daemon-parented shell"];
}

- (void)refreshDetail
{
    [self showDetail];
    if (!_batchingRefresh) [_interface fitMainWindow];
}

- (void)showDetail
{
    const st_companion_capability *capability =
        st_companion_capability_find([self selectedCapability]);
    if (capability == NULL) return;
    _interface.detailTitle.stringValue = STCompanionString(capability->name);
    _interface.detailSummary.stringValue = STCompanionString(capability->summary);
    [self refreshPanel:_interface.guiPanel
            capability:capability
               context:ST_COMPANION_CONTEXT_GUI_PARENTED];
    [self refreshPanel:_interface.daemonPanel
            capability:capability
               context:ST_COMPANION_CONTEXT_DAEMON_PARENTED];
    _interface.waitLabel.stringValue =
        capability->needs_person
            ? [NSString stringWithFormat:@"Each probe waits up to %u seconds, because a person "
                                         @"may need to answer macOS or act in the test targets "
                                         @"window.",
                                         capability->wait_seconds]
            : [NSString stringWithFormat:@"Each probe waits up to %u seconds.",
                                         capability->wait_seconds];
    _interface.openSettingsButton.hidden = STCompanionSettingsURL(capability) == nil;
    NSString *note = capability->code == _probeCapability ? _probeNote : nil;
    if (note == nil) {
        note = st_companion_run_accepts_requests(_runState)
                   ? @"Request Probe asks the evidence controller to run this case in a "
                     @"SpaceTerm window and in a server session."
                   : @"Probes are available while an evidence run is active.";
    }
    _interface.probeStatusLabel.stringValue = note;
}

- (void)refreshAuthorization
{
    [self showAuthorization];
    if (!_batchingRefresh) [_interface fitMainWindow];
}

- (void)showAuthorization
{
    if (_authBusy) {
        st_companion_status_copy busy = {"In progress", NULL, "hourglass",
                                         ST_COMPANION_TONE_PROGRESS};
        show_status(_interface.authorizationSymbol, _interface.authorizationTitle,
                    _interface.authorizationDetail, busy, _authBusyDetail);
        return;
    }
    if (_confirming && _authFlow.phase == ST_COMPANION_AUTH_CONFIRMING) {
        st_companion_status_copy waiting = {"Waiting for your confirmation", NULL,
                                            "questionmark.circle", ST_COMPANION_TONE_PROGRESS};
        show_status(_interface.authorizationSymbol, _interface.authorizationTitle,
                    _interface.authorizationDetail, waiting,
                    @"macOS has not been asked yet. It asks only after you confirm.");
        return;
    }
    st_companion_status_copy copy = st_companion_auth_outcome_copy(_authFlow.outcome);
    NSString *extra = nil;
    if (_authFlow.outcome == ST_COMPANION_AUTH_OUTCOME_FAILED) {
        const char *name = st_companion_failure_name(_authFlow.failure);
        extra = name != NULL ? [NSString stringWithFormat:@"Failure: %s.", name]
                             : @"Failure: unknown.";
    }
    if (_authFlow.phase == ST_COMPANION_AUTH_FINISHED) {
        NSString *operation = STCompanionString(st_companion_operation_title(_authFlow.operation));
        copy.title = NULL;
        show_status(_interface.authorizationSymbol, _interface.authorizationTitle,
                    _interface.authorizationDetail, copy, extra);
        _interface.authorizationTitle.stringValue = [NSString
            stringWithFormat:@"%@: %@", operation,
                             STCompanionString(
                                 st_companion_auth_outcome_copy(_authFlow.outcome).title)];
        return;
    }
    show_status(_interface.authorizationSymbol, _interface.authorizationTitle,
                _interface.authorizationDetail, copy, extra);
}

- (NSString *)receiptLine:(st_companion_target)target
{
    uint32_t accepted = _ledger.accepted[target];
    uint32_t rejected = _ledger.rejected[target];
    if (!_ledger.has_last[target]) {
        return [NSString stringWithFormat:@"No %@ receipt yet.", target_title(target)];
    }
    return [NSString
        stringWithFormat:@"%@ Accepted: %u. Ignored or rejected: %u.",
                         STCompanionString(st_companion_receipt_sentence(_ledger.last[target])),
                         accepted, rejected];
}

- (BOOL)armed:(st_companion_target)target
{
    uint64_t now = [self now];
    for (size_t i = 0; i < ST_COMPANION_MAX_EXPECTATIONS; ++i) {
        const st_companion_expectation *expectation = &_ledger.expectations[i];
        if (expectation->armed && expectation->target == target && expectation->deadline_ns > now) {
            return YES;
        }
    }
    return NO;
}

- (void)refreshTargets
{
    _interface.captureStatus.stringValue =
        _ledger.capture_armed
            ? @"Showing the code for the current Screen Recording probe. The probe captures "
              @"only this window."
            : @"No Screen Recording probe has sent a code yet.";

    NSString *accessibility = [self armed:ST_COMPANION_TARGET_ACCESSIBILITY]
                                  ? @"Expecting a value from the current Accessibility probe."
                                  : @"No Accessibility value is expected now.";
    _interface.accessibilityStatus.stringValue =
        [NSString stringWithFormat:@"%@ %@", accessibility,
                                   [self receiptLine:ST_COMPANION_TARGET_ACCESSIBILITY]];

    NSString *clipboard =
        [self armed:ST_COMPANION_TARGET_CLIPBOARD]
            ? @"Expecting a pasted marker. Select the area above and choose Edit > Paste."
            : @"No marker is expected now. Pasting here reads nothing from the clipboard.";
    _interface.clipboardStatus.stringValue =
        [NSString stringWithFormat:@"%@ %@", clipboard,
                                   [self receiptLine:ST_COMPANION_TARGET_CLIPBOARD]];

    _interface.urlStatus.stringValue =
        [@"URL: " stringByAppendingString:[self receiptLine:ST_COMPANION_TARGET_URL]];
    _interface.appleEventStatus.stringValue =
        [@"Apple Event: " stringByAppendingString:[self receiptLine:ST_COMPANION_TARGET_APPLE_EVENT]];
    [_interface fitTargetsWindow];
}

- (void)refreshControls
{
    _interface.requestProbeButton.enabled = [self canRequestProbe];
    _interface.openSettingsButton.enabled = [self canOpenSettings];
    _interface.requestAuthorizationButton.enabled = [self canRequestAuthorization];
    _interface.operationPopUp.enabled = [self canRequestAuthorization];
    _interface.stopRunButton.enabled = [self canStopRun];
}
@end
