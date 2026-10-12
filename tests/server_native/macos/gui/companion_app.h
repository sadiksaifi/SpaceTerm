#ifndef SPACETERM_EVIDENCE_COMPANION_APP_H
#define SPACETERM_EVIDENCE_COMPANION_APP_H

#import <AppKit/AppKit.h>

#include "companion_authorization.h"
#include "companion_model.h"
#include "companion_protocol.h"
#include "companion_receipts.h"

/*
 * AppKit layer of the temporary native evidence companion.
 *
 * Every side effect goes through a port so interaction tests can run the real
 * windows and controller without touching the pasteboard, System Settings,
 * Authorization Services, the evidence controller, or application
 * termination. Production ports live in companion_ports.m and
 * companion_pasteboard.m.
 */

NS_ASSUME_NONNULL_BEGIN

@protocol STCompanionChannel <NSObject>
/* Sends one complete record, including its newline. */
- (void)sendRecord:(const char *)record length:(size_t)length;
- (void)close;
@end

@protocol STCompanionChannelDelegate <NSObject>
- (void)channelDidReceiveRecord:(const char *)record length:(size_t)length;
/* state is CONTROLLER_LOST or PROTOCOL_ERROR. */
- (void)channelDidCloseWithState:(st_companion_run_state)state;
@end

/* The only route to clipboard content. Metadata calls never read content. */
@protocol STCompanionPasteboard <NSObject>
- (NSInteger)changeCount;
/* True when the pasteboard holds plain text and nothing else. */
- (BOOL)holdsOnlyPlainText;
/* Copies plain text when it fits in capacity. Called only for a pasted marker
 * that the ledger allowed. */
- (BOOL)copyPlainText:(uint8_t *)buffer capacity:(size_t)capacity length:(size_t *)length;
@end

typedef void (^STCompanionDecision)(BOOL confirmed);

/* The companion's own confirmation. It is never the macOS authorization
 * prompt, which only the management adapter can cause. */
@protocol STCompanionConfirmer <NSObject>
- (void)confirmWithTitle:(NSString *)title
                 message:(NSString *)message
            confirmTitle:(NSString *)confirmTitle
             destructive:(BOOL)destructive
                  window:(nullable NSWindow *)window
              completion:(STCompanionDecision)completion;
@end

@protocol STCompanionAnnouncer <NSObject>
- (void)announce:(NSString *)text;
@end

/* Opens only Privacy & Security panes from the fixed allowlist. */
@protocol STCompanionSettingsOpener <NSObject>
- (void)openSettingsURL:(NSURL *)url;
@end

/* Runs authorization port calls off the main thread in production. */
@protocol STCompanionExecutor <NSObject>
- (void)perform:(dispatch_block_t)work then:(dispatch_block_t)completion;
@end

/* Orders windows in. Activation happens only for a person's own request. */
@protocol STCompanionWindowPresenter <NSObject>
- (void)presentWindow:(NSWindow *)window activate:(BOOL)activate;
@end

/* Production presenter. Defined with the windows so interaction tests can
 * check how it orders a window without activating the companion. */
@interface STWindowPresenter : NSObject <STCompanionWindowPresenter>
@end

@interface STCompanionPorts : NSObject
@property (nonatomic, strong) id<STCompanionPasteboard> pasteboard;
@property (nonatomic, strong) id<STCompanionConfirmer> confirmer;
@property (nonatomic, strong) id<STCompanionAnnouncer> announcer;
@property (nonatomic, strong) id<STCompanionSettingsOpener> settingsOpener;
@property (nonatomic, strong) id<STCompanionExecutor> authorizationExecutor;
@property (nonatomic, strong) id<STCompanionWindowPresenter> presenter;
@property (nonatomic, copy) uint64_t (^clock)(void);
@property (nonatomic, copy) void (^quitReply)(BOOL quit);
/* NULL selects st_companion_authorization_unavailable(). */
@property (nonatomic, assign, nullable) const st_companion_authorization_port *authorization;
@end

/* Defined in companion_ports.m, which interaction tests do not link. */
@interface STCompanionPorts (Production)
+ (instancetype)productionPorts;
@end

/* One "In a SpaceTerm window" or "In a server session" block. */
@interface STContextPanel : NSStackView
@property (nonatomic, readonly) NSTextField *heading;
@property (nonatomic, readonly) NSTextField *caption;
@property (nonatomic, readonly) NSTextField *whoAsks;
@property (nonatomic, readonly) NSTextField *explanation;
@property (nonatomic, readonly) NSImageView *resultSymbol;
@property (nonatomic, readonly) NSTextField *resultTitle;
@property (nonatomic, readonly) NSTextField *resultDetail;
@property (nonatomic, readonly) NSTextField *observed;
@end

@class STAccessibilityTargetField;
@class STClipboardTargetView;
@class STCapturePatternView;

@interface STCompanionInterface : NSObject
@property (nonatomic, readonly) NSWindow *mainWindow;
@property (nonatomic, readonly) NSWindow *targetsWindow;

@property (nonatomic, readonly) NSTextField *purposeLabel;
@property (nonatomic, readonly) NSTextField *scopeLabel;
@property (nonatomic, readonly) NSTableView *capabilityTable;
@property (nonatomic, readonly) NSTextField *detailTitle;
@property (nonatomic, readonly) NSTextField *detailSummary;
@property (nonatomic, readonly) STContextPanel *guiPanel;
@property (nonatomic, readonly) STContextPanel *daemonPanel;
@property (nonatomic, readonly) NSTextField *waitLabel;
@property (nonatomic, readonly) NSButton *openSettingsButton;
@property (nonatomic, readonly) NSButton *requestProbeButton;
@property (nonatomic, readonly) NSTextField *probeStatusLabel;

@property (nonatomic, readonly) NSPopUpButton *operationPopUp;
@property (nonatomic, readonly) NSButton *requestAuthorizationButton;
@property (nonatomic, readonly) NSImageView *authorizationSymbol;
@property (nonatomic, readonly) NSTextField *authorizationTitle;
@property (nonatomic, readonly) NSTextField *authorizationDetail;

@property (nonatomic, readonly) NSImageView *runSymbol;
@property (nonatomic, readonly) NSTextField *runTitle;
@property (nonatomic, readonly) NSTextField *runDetail;
@property (nonatomic, readonly) NSButton *showTargetsButton;
@property (nonatomic, readonly) NSButton *stopRunButton;

@property (nonatomic, readonly) STCapturePatternView *capturePattern;
@property (nonatomic, readonly) NSTextField *captureStatus;
@property (nonatomic, readonly) STAccessibilityTargetField *accessibilityTarget;
@property (nonatomic, readonly) NSTextField *accessibilityStatus;
@property (nonatomic, readonly) STClipboardTargetView *clipboardTarget;
@property (nonatomic, readonly) NSTextField *clipboardStatus;
@property (nonatomic, readonly) NSTextField *urlStatus;
@property (nonatomic, readonly) NSTextField *appleEventStatus;

/* target receives every button action; the table uses source for data. */
- (instancetype)initWithTarget:(id)target
                   tableSource:(id<NSTableViewDataSource, NSTableViewDelegate>)source;
/* Sizes both windows to their current content. The main window keeps a
 * maximum height and scrolls its document above the run status. */
- (void)sizeWindowsToFit;
/* Height the main window needs to show its whole document and footer. */
- (CGFloat)mainContentHeight;
/* Grows the main window, keeping its top edge, when longer status text needs
 * room, up to the maximum height and the screen. It never shrinks the window
 * and stops once a person resizes it; the document scrolls instead. */
- (void)fitMainWindow;
/* Resizes the targets window to its content, keeping its top edge, so status
 * text never pushes the capture pattern or intro out of view. */
- (void)fitTargetsWindow;
/* Places the targets window inside visible, beside the main window: to its
 * right when that fits, else to its left, else at the right edge of visible.
 * Its top aligns with the main window's top where the height allows. */
- (void)placeTargetsWindowWithin:(NSRect)visible;
/* Places the targets window once, on the main window's screen. Later calls
 * keep wherever the person moved it. */
- (void)placeTargetsWindow;
/* The controls in main-window key-view order, starting at the table. */
- (NSArray<NSView *> *)mainKeyViewOrder;
- (NSArray<NSView *> *)targetsKeyViewOrder;
@end

@interface STCapturePatternView : NSView
/* NULL draws the frame with an empty interior. */
- (void)showNonce:(nullable const st_companion_id *)nonce;
@end

/* An Accessibility text field element that receives a probe's AXValue
 * without keeping or showing it. It is a plain view, not an NSTextField, so
 * the element carries its own identifier and label: an NSTextField exposes
 * its cell, which drops both. */
@interface STAccessibilityTargetField : NSView
@property (nonatomic, copy, nullable) void (^valueHandler)(id _Nullable value);
@end

/* A focusable target that accepts only Edit > Paste. */
@interface STClipboardTargetView : NSView <NSMenuItemValidation>
@property (nonatomic, copy, nullable) void (^pasteHandler)(void);
/* Edit > Paste. Calls pasteHandler, which decides whether to read. */
- (IBAction)paste:(nullable id)sender;
@end

/* Connects to the evidence controller's socket after checking that its
 * directory and the socket belong to this account and that the peer runs as
 * this account. Returns nil when any check fails. */
@interface STSocketChannel : NSObject <STCompanionChannel>
+ (nullable instancetype)connectToPath:(NSString *)path
                              delegate:(id<STCompanionChannelDelegate>)delegate;
@end

/* Production pasteboard port, defined in companion_pasteboard.m. */
@interface STGeneralPasteboard : NSObject <STCompanionPasteboard>
@end

NSString *STCompanionString(const char *text);
NSColor *STCompanionToneColor(st_companion_tone tone);
NSImage *_Nullable STCompanionSymbol(const char *name);
/* Builds the allowlisted Privacy & Security URL for a capability, or nil. */
NSURL *_Nullable STCompanionSettingsURL(const st_companion_capability *capability);

@interface STCompanion : NSObject <STCompanionChannelDelegate, NSTableViewDataSource,
                                   NSTableViewDelegate, NSMenuItemValidation>
@property (nonatomic, readonly) STCompanionInterface *interface;
@property (nonatomic, readonly) st_companion_run_state runState;
@property (nonatomic, readonly) const st_companion_ledger *ledger;
@property (nonatomic, readonly) const st_companion_auth_flow *authorizationFlow;

- (instancetype)initWithRun:(const st_companion_id *)run ports:(STCompanionPorts *)ports;

/* Starts the handshake on a connected channel. */
- (void)attachChannel:(id<STCompanionChannel>)channel;
/* Records why no channel could be attached. */
- (void)connectionFailedWithState:(st_companion_run_state)state;

- (void)selectCapability:(uint32_t)code;
- (uint32_t)selectedCapability;
- (void)selectOperation:(st_companion_operation)operation;

- (IBAction)requestProbe:(nullable id)sender;
- (IBAction)openPrivacySettings:(nullable id)sender;
- (IBAction)requestAuthorization:(nullable id)sender;
- (IBAction)stopEvidenceRun:(nullable id)sender;
- (IBAction)showCompanion:(nullable id)sender;
- (IBAction)showTestTargets:(nullable id)sender;

- (void)handleURLEvent:(NSAppleEventDescriptor *)event
        withReplyEvent:(NSAppleEventDescriptor *)reply;
- (void)handleReceiptEvent:(NSAppleEventDescriptor *)event
            withReplyEvent:(NSAppleEventDescriptor *)reply;

- (NSApplicationTerminateReply)applicationShouldTerminate:(NSApplication *)sender;

/* The complete menu bar. Every item maps to one st_companion_action. */
- (NSMenu *)mainMenu;
@end

NS_ASSUME_NONNULL_END

#endif
