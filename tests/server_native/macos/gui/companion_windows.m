#import "companion_app.h"

/* Window and view construction only. STCompanion owns all state and text
 * updates; this file builds the hierarchy, the key-view loops, and the
 * accessibility structure. Colors are semantic so both appearances work,
 * except the capture pattern, which must draw the same pixels in both.
 */

NSString *STCompanionString(const char *text)
{
    return text == NULL ? @"" : (NSString *)[NSString stringWithUTF8String:text];
}

NSColor *STCompanionToneColor(st_companion_tone tone)
{
    switch (tone) {
    case ST_COMPANION_TONE_POSITIVE: return NSColor.systemGreenColor;
    case ST_COMPANION_TONE_NEGATIVE: return NSColor.systemRedColor;
    case ST_COMPANION_TONE_CAUTION: return NSColor.systemOrangeColor;
    case ST_COMPANION_TONE_PROGRESS: return NSColor.controlAccentColor;
    case ST_COMPANION_TONE_NEUTRAL: break;
    }
    return NSColor.secondaryLabelColor;
}

NSImage *STCompanionSymbol(const char *name)
{
    /* Decorative: the adjacent text always states the meaning. */
    return [NSImage imageWithSystemSymbolName:STCompanionString(name) accessibilityDescription:nil];
}

NSURL *STCompanionSettingsURL(const st_companion_capability *capability)
{
    static const char *const allowed[] = {"Privacy_FilesAndFolders", "Privacy_Microphone",
                                          "Privacy_ScreenCapture", "Privacy_Accessibility",
                                          "Privacy_Automation"};
    if (capability == NULL || capability->settings_anchor == NULL) return nil;
    for (size_t i = 0; i < sizeof allowed / sizeof allowed[0]; ++i) {
        if (strcmp(allowed[i], capability->settings_anchor) == 0) {
            NSString *text = [@"x-apple.systempreferences:com.apple.settings.PrivacySecurity."
                               @"extension?" stringByAppendingString:STCompanionString(allowed[i])];
            return [NSURL URLWithString:text];
        }
    }
    return nil;
}

/* --- Labels ---------------------------------------------------------------- */

/* A multi-line label that rewraps to whatever width layout gives it. */
@interface STWrappingLabel : NSTextField
@end

@implementation STWrappingLabel
- (void)layout
{
    [super layout];
    CGFloat width = NSWidth(self.bounds);
    if (width > 1 && fabs(self.preferredMaxLayoutWidth - width) > 0.5) {
        self.preferredMaxLayoutWidth = width;
        [self invalidateIntrinsicContentSize];
    }
}
@end

static NSFont *text_font(NSFontTextStyle style, BOOL bold)
{
    NSFont *font = [NSFont preferredFontForTextStyle:style options:@{}];
    if (!bold) return font;
    NSFontDescriptor *descriptor =
        [font.fontDescriptor fontDescriptorWithSymbolicTraits:NSFontDescriptorTraitBold];
    NSFont *bold_font = [NSFont fontWithDescriptor:descriptor size:font.pointSize];
    return bold_font != nil ? bold_font : font;
}

static NSTextField *make_label(NSString *text, NSFontTextStyle style, BOOL bold, NSColor *color,
                               BOOL wraps)
{
    NSTextField *label = wraps ? [[STWrappingLabel alloc] initWithFrame:NSZeroRect]
                               : [[NSTextField alloc] initWithFrame:NSZeroRect];
    label.stringValue = text;
    label.editable = NO;
    label.selectable = NO;
    label.bezeled = NO;
    label.bordered = NO;
    label.drawsBackground = NO;
    label.font = text_font(style, bold);
    label.textColor = color;
    label.translatesAutoresizingMaskIntoConstraints = NO;
    if (wraps) {
        label.usesSingleLineMode = NO;
        label.maximumNumberOfLines = 0;
        label.lineBreakMode = NSLineBreakByWordWrapping;
        label.cell.wraps = YES;
        label.cell.scrollable = NO;
        label.preferredMaxLayoutWidth = 300;
        [label setContentCompressionResistancePriority:NSLayoutPriorityDefaultLow
                                        forOrientation:NSLayoutConstraintOrientationHorizontal];
        /* Wrapping text takes the width its container offers. */
        [label setContentHuggingPriority:1
                          forOrientation:NSLayoutConstraintOrientationHorizontal];
    }
    return label;
}

static NSTextField *heading_label(NSString *text)
{
    NSTextField *label = make_label(text, NSFontTextStyleHeadline, YES, NSColor.labelColor, NO);
    label.accessibilityRole = NSAccessibilityStaticTextRole;
    return label;
}

static NSImageView *symbol_view(void)
{
    NSImageView *view = [[NSImageView alloc] initWithFrame:NSZeroRect];
    view.translatesAutoresizingMaskIntoConstraints = NO;
    view.symbolConfiguration =
        [NSImageSymbolConfiguration configurationWithTextStyle:NSFontTextStyleBody
                                                         scale:NSImageSymbolScaleLarge];
    view.image = STCompanionSymbol("circle.dashed");
    view.accessibilityElement = NO;
    [view setContentHuggingPriority:NSLayoutPriorityRequired
                     forOrientation:NSLayoutConstraintOrientationHorizontal];
    [view.widthAnchor constraintEqualToConstant:22].active = YES;
    return view;
}

static NSStackView *stack(NSUserInterfaceLayoutOrientation orientation, CGFloat spacing,
                          NSArray<NSView *> *views)
{
    NSStackView *view = [NSStackView stackViewWithViews:views];
    view.orientation = orientation;
    view.spacing = spacing;
    BOOL vertical = orientation == NSUserInterfaceLayoutOrientationVertical;
    view.alignment = vertical ? NSLayoutAttributeLeading : NSLayoutAttributeCenterY;
    if (!vertical) view.distribution = NSStackViewDistributionFill;
    view.translatesAutoresizingMaskIntoConstraints = NO;
    view.detachesHiddenViews = YES;
    return view;
}

/* Lets one view in a filling horizontal stack take the spare width. */
static void flexible(NSView *view)
{
    if ([view isKindOfClass:NSStackView.class]) {
        [(NSStackView *)view setHuggingPriority:1
                                 forOrientation:NSLayoutConstraintOrientationHorizontal];
    }
    [view setContentHuggingPriority:1 forOrientation:NSLayoutConstraintOrientationHorizontal];
}

/* Height of view laid out at width. Wrapping labels settle after a few
 * passes because each pass updates their preferred width. */
static CGFloat fitting_height(NSView *view, CGFloat width)
{
    NSLayoutConstraint *pin = [view.widthAnchor constraintEqualToConstant:width];
    pin.active = YES;
    for (int pass = 0; pass < 4; ++pass) {
        [view setFrameSize:NSMakeSize(width, MAX(view.fittingSize.height, 1))];
        [view layoutSubtreeIfNeeded];
    }
    CGFloat height = ceil(view.fittingSize.height);
    pin.active = NO;
    return height;
}

/* Makes each wrapping child of a vertical stack span the stack's width. */
static void fill_width(NSStackView *container, NSArray<NSView *> *views)
{
    for (NSView *view in views) {
        [view.widthAnchor constraintEqualToAnchor:container.widthAnchor
                                         constant:-(container.edgeInsets.left +
                                                    container.edgeInsets.right)]
            .active = YES;
    }
}

static NSBox *separator(void)
{
    NSBox *box = [[NSBox alloc] initWithFrame:NSZeroRect];
    box.boxType = NSBoxSeparator;
    box.translatesAutoresizingMaskIntoConstraints = NO;
    return box;
}

static NSButton *button(NSString *title, id target, SEL action)
{
    NSButton *control = [NSButton buttonWithTitle:title target:target action:action];
    control.translatesAutoresizingMaskIntoConstraints = NO;
    /* Push buttons keep their natural width; text takes any extra space. */
    [control setContentHuggingPriority:NSLayoutPriorityDefaultHigh
                        forOrientation:NSLayoutConstraintOrientationHorizontal];
    return control;
}

/* --- Context panel --------------------------------------------------------- */

@implementation STContextPanel {
    NSTextField *_heading;
    NSTextField *_caption;
    NSTextField *_whoAsks;
    NSTextField *_explanation;
    NSImageView *_resultSymbol;
    NSTextField *_resultTitle;
    NSTextField *_resultDetail;
    NSTextField *_observed;
}

- (instancetype)initWithHeading:(NSString *)heading caption:(NSString *)caption
{
    self = [super initWithFrame:NSZeroRect];
    if (self == nil) return nil;
    _heading = heading_label(heading);
    _caption = make_label(caption, NSFontTextStyleCaption1, NO, NSColor.secondaryLabelColor, NO);
    _whoAsks = make_label(@"", NSFontTextStyleBody, YES, NSColor.labelColor, YES);
    _explanation = make_label(@"", NSFontTextStyleCallout, NO, NSColor.secondaryLabelColor, YES);
    _resultSymbol = symbol_view();
    _resultTitle = make_label(@"", NSFontTextStyleBody, YES, NSColor.labelColor, NO);
    _resultDetail = make_label(@"", NSFontTextStyleCallout, NO, NSColor.labelColor, YES);
    _observed =
        make_label(@"", NSFontTextStyleSubheadline, NO, NSColor.secondaryLabelColor, YES);
    NSStackView *result = stack(NSUserInterfaceLayoutOrientationHorizontal, 6,
                                @[_resultSymbol, _resultTitle]);
    result.distribution = NSStackViewDistributionGravityAreas;

    self.orientation = NSUserInterfaceLayoutOrientationVertical;
    self.alignment = NSLayoutAttributeLeading;
    self.spacing = 6;
    self.edgeInsets = NSEdgeInsetsMake(12, 12, 12, 12);
    self.translatesAutoresizingMaskIntoConstraints = NO;
    for (NSView *view in @[_heading, _caption, _whoAsks, _explanation, result, _resultDetail,
                           _observed]) {
        [self addArrangedSubview:view];
    }
    [self setCustomSpacing:2 afterView:_heading];
    [self setCustomSpacing:10 afterView:_caption];
    [self setCustomSpacing:12 afterView:_explanation];
    fill_width(self, @[_whoAsks, _explanation, _resultDetail, _observed]);

    self.accessibilityElement = YES;
    self.accessibilityRole = NSAccessibilityGroupRole;
    self.accessibilityLabel = [NSString stringWithFormat:@"%@, %@", heading, caption];
    return self;
}

/* Drawn rather than layer-backed so the fill resolves for the current
 * appearance and appears in cached renders. */
- (void)drawRect:(NSRect)dirty
{
    (void)dirty;
    NSBezierPath *path = [NSBezierPath bezierPathWithRoundedRect:self.bounds xRadius:8 yRadius:8];
    [NSColor.quaternarySystemFillColor setFill];
    [path fill];
    [NSColor.separatorColor setStroke];
    NSBezierPath *outline =
        [NSBezierPath bezierPathWithRoundedRect:NSInsetRect(self.bounds, 0.5, 0.5)
                                        xRadius:8
                                        yRadius:8];
    outline.lineWidth = 1;
    [outline stroke];
}

- (NSTextField *)heading { return _heading; }
- (NSTextField *)caption { return _caption; }
- (NSTextField *)whoAsks { return _whoAsks; }
- (NSTextField *)explanation { return _explanation; }
- (NSImageView *)resultSymbol { return _resultSymbol; }
- (NSTextField *)resultTitle { return _resultTitle; }
- (NSTextField *)resultDetail { return _resultDetail; }
- (NSTextField *)observed { return _observed; }
@end

/* --- Synthetic targets ----------------------------------------------------- */

@implementation STCapturePatternView {
    uint8_t _cells[ST_COMPANION_CAPTURE_ROWS][ST_COMPANION_CAPTURE_COLUMNS];
}

- (instancetype)initWithFrame:(NSRect)frame
{
    self = [super initWithFrame:frame];
    if (self == nil) return nil;
    st_companion_capture_pattern(NULL, _cells);
    self.translatesAutoresizingMaskIntoConstraints = NO;
    self.accessibilityElement = YES;
    self.accessibilityRole = NSAccessibilityImageRole;
    self.accessibilityLabel = @"Screen Recording test pattern";
    self.accessibilityHelp = @"A red frame around a black and white code for the current probe.";
    return self;
}

- (NSSize)intrinsicContentSize
{
    return NSMakeSize(ST_COMPANION_CAPTURE_COLUMNS * ST_COMPANION_CAPTURE_CELL_POINTS,
                      ST_COMPANION_CAPTURE_ROWS * ST_COMPANION_CAPTURE_CELL_POINTS);
}

- (BOOL)isFlipped
{
    return YES;
}

- (void)showNonce:(const st_companion_id *)nonce
{
    st_companion_capture_pattern(nonce, _cells);
    self.needsDisplay = YES;
}

- (void)drawRect:(NSRect)dirty
{
    (void)dirty;
    /* Fixed sRGB values in both appearances so a capture decodes the same. */
    NSColor *colors[3] = {[NSColor colorWithSRGBRed:1 green:1 blue:1 alpha:1],
                          [NSColor colorWithSRGBRed:0 green:0 blue:0 alpha:1],
                          [NSColor colorWithSRGBRed:1 green:0 blue:0 alpha:1]};
    const CGFloat cell = ST_COMPANION_CAPTURE_CELL_POINTS;
    for (size_t row = 0; row < ST_COMPANION_CAPTURE_ROWS; ++row) {
        for (size_t column = 0; column < ST_COMPANION_CAPTURE_COLUMNS; ++column) {
            [colors[_cells[row][column]] setFill];
            NSRectFill(NSMakeRect((CGFloat)column * cell, (CGFloat)row * cell, cell, cell));
        }
    }
}
@end

static NSString *const ST_ACCESSIBILITY_PLACEHOLDER = @"The probe sets this field's value";

static NSFont *accessibility_target_font(void)
{
    return [NSFont preferredFontForTextStyle:NSFontTextStyleBody options:@{}];
}

@implementation STAccessibilityTargetField

- (instancetype)initWithFrame:(NSRect)frame
{
    self = [super initWithFrame:frame];
    if (self == nil) return nil;
    self.translatesAutoresizingMaskIntoConstraints = NO;
    return self;
}

- (NSSize)intrinsicContentSize
{
    NSFont *font = accessibility_target_font();
    CGFloat line = ceil(font.ascender - font.descender + font.leading);
    return NSMakeSize(NSViewNoIntrinsicMetric, line + 8);
}

/* Draws a field bezel and its placeholder. It has no value to draw. */
- (void)drawRect:(NSRect)dirty
{
    (void)dirty;
    NSRect inset = NSInsetRect(self.bounds, 0.5, 0.5);
    NSBezierPath *path = [NSBezierPath bezierPathWithRoundedRect:inset xRadius:6 yRadius:6];
    [NSColor.textBackgroundColor setFill];
    [path fill];
    path.lineWidth = 1;
    [NSColor.separatorColor setStroke];
    [path stroke];
    NSDictionary *attributes = @{
        NSFontAttributeName : accessibility_target_font(),
        NSForegroundColorAttributeName : NSColor.placeholderTextColor,
    };
    NSSize size = [ST_ACCESSIBILITY_PLACEHOLDER sizeWithAttributes:attributes];
    [ST_ACCESSIBILITY_PLACEHOLDER
           drawAtPoint:NSMakePoint(8, floor((NSHeight(self.bounds) - size.height) / 2))
        withAttributes:attributes];
}

- (BOOL)isAccessibilityElement
{
    return YES;
}

- (BOOL)isAccessibilityEnabled
{
    return YES;
}

- (NSAccessibilityRole)accessibilityRole
{
    return NSAccessibilityTextFieldRole;
}

- (NSString *)accessibilityPlaceholderValue
{
    return ST_ACCESSIBILITY_PLACEHOLDER;
}

- (BOOL)isAccessibilitySelectorAllowed:(SEL)selector
{
    if (selector == @selector(setAccessibilityValue:)) return YES;
    return [super isAccessibilitySelectorAllowed:selector];
}

/* The value goes to the receipt ledger and is never stored or drawn. */
- (void)setAccessibilityValue:(id)value
{
    if (self.valueHandler != nil) self.valueHandler(value);
}

- (id)accessibilityValue
{
    return @"";
}
@end

@implementation STClipboardTargetView {
    NSTextField *_prompt;
}

- (instancetype)initWithFrame:(NSRect)frame
{
    self = [super initWithFrame:frame];
    if (self == nil) return nil;
    self.translatesAutoresizingMaskIntoConstraints = NO;
    _prompt = make_label(@"Select this area, then choose Edit > Paste (⌘V).",
                         NSFontTextStyleBody, NO, NSColor.secondaryLabelColor, YES);
    _prompt.alignment = NSTextAlignmentCenter;
    [self addSubview:_prompt];
    [NSLayoutConstraint activateConstraints:@[
        [_prompt.centerYAnchor constraintEqualToAnchor:self.centerYAnchor],
        [_prompt.leadingAnchor constraintEqualToAnchor:self.leadingAnchor constant:12],
        [_prompt.trailingAnchor constraintEqualToAnchor:self.trailingAnchor constant:-12],
        [self.heightAnchor constraintEqualToConstant:64],
    ]];
    self.accessibilityElement = YES;
    self.accessibilityRole = NSAccessibilityGroupRole;
    self.accessibilityLabel = @"Clipboard paste target";
    self.accessibilityHelp = @"Choose Edit > Paste to deliver the probe's marker. The companion "
                             @"reads the clipboard only when the evidence run expects a marker.";
    return self;
}

- (BOOL)acceptsFirstResponder { return YES; }
- (BOOL)canBecomeKeyView { return YES; }
- (BOOL)needsPanelToBecomeKey { return NO; }

/* The targets window comes forward without activating the companion, so the
 * first click must already select this target. */
- (BOOL)acceptsFirstMouse:(nullable NSEvent *)event
{
    (void)event;
    return YES;
}

- (BOOL)becomeFirstResponder
{
    self.needsDisplay = YES;
    return YES;
}

- (BOOL)resignFirstResponder
{
    self.needsDisplay = YES;
    return YES;
}

- (void)mouseDown:(NSEvent *)event
{
    (void)event;
    [self.window makeFirstResponder:self];
}

- (NSRect)focusRingMaskBounds
{
    return self.bounds;
}

- (void)drawFocusRingMask
{
    [[NSBezierPath bezierPathWithRoundedRect:self.bounds xRadius:8 yRadius:8] fill];
}

- (void)drawRect:(NSRect)dirty
{
    (void)dirty;
    NSRect inset = NSInsetRect(self.bounds, 1, 1);
    NSBezierPath *path = [NSBezierPath bezierPathWithRoundedRect:inset xRadius:8 yRadius:8];
    [NSColor.quaternarySystemFillColor setFill];
    [path fill];
    CGFloat dashes[2] = {5, 4};
    [path setLineDash:dashes count:2 phase:0];
    path.lineWidth = 1;
    [NSColor.separatorColor setStroke];
    [path stroke];
}

- (BOOL)validateMenuItem:(NSMenuItem *)item
{
    if (item.action == @selector(paste:)) return YES;
    return NO;
}

/* Edit > Paste is a person's own paste, which macOS allows without a
 * separate pasteboard privacy alert. The handler decides whether to read. */
- (void)paste:(id)sender
{
    (void)sender;
    if (self.pasteHandler != nil) self.pasteHandler();
}
@end


/* --- Presentation ---------------------------------------------------------- */

@implementation STWindowPresenter
- (void)presentWindow:(NSWindow *)window activate:(BOOL)activate
{
    if (activate) {
        [NSApp activate];
        [window makeKeyAndOrderFront:nil];
    } else {
        /* orderFront: from an inactive app leaves the window behind the
         * active app's windows. This orders it in front without taking focus. */
        [window orderFrontRegardless];
    }
}
@end

/* --- Interface ------------------------------------------------------------- */

/* Keeps scrolled content pinned to the top of the main window. */
@interface STFlippedClipView : NSClipView
@end

@implementation STFlippedClipView
- (BOOL)isFlipped
{
    return YES;
}
@end

static const CGFloat ST_MAIN_WIDTH = 1120;
static const CGFloat ST_MAIN_MIN_WIDTH = 1000;
static const CGFloat ST_MAIN_MAX_HEIGHT = 880;
static const CGFloat ST_MAIN_MIN_HEIGHT = 560;
static const CGFloat ST_TARGETS_WIDTH = 480;
static const CGFloat ST_TABLE_WIDTH = 450;
static const CGFloat ST_WINDOW_GAP = 12;

@implementation STCompanionInterface {
    NSWindow *_mainWindow;
    NSWindow *_targetsWindow;
    NSTextField *_purposeLabel;
    NSTextField *_scopeLabel;
    NSTableView *_capabilityTable;
    NSTextField *_detailTitle;
    NSTextField *_detailSummary;
    STContextPanel *_guiPanel;
    STContextPanel *_daemonPanel;
    NSTextField *_waitLabel;
    NSButton *_openSettingsButton;
    NSButton *_requestProbeButton;
    NSTextField *_probeStatusLabel;
    NSPopUpButton *_operationPopUp;
    NSButton *_requestAuthorizationButton;
    NSImageView *_authorizationSymbol;
    NSTextField *_authorizationTitle;
    NSTextField *_authorizationDetail;
    NSImageView *_runSymbol;
    NSTextField *_runTitle;
    NSTextField *_runDetail;
    NSButton *_showTargetsButton;
    NSButton *_stopRunButton;
    STCapturePatternView *_capturePattern;
    NSTextField *_captureStatus;
    STAccessibilityTargetField *_accessibilityTarget;
    NSTextField *_accessibilityStatus;
    STClipboardTargetView *_clipboardTarget;
    NSTextField *_clipboardStatus;
    NSTextField *_urlStatus;
    NSTextField *_appleEventStatus;
    NSStackView *_document;
    NSStackView *_footer;
    BOOL _mainSizedByPerson;
    BOOL _targetsPlaced;
}

- (instancetype)initWithTarget:(id)target
                   tableSource:(id<NSTableViewDataSource, NSTableViewDelegate>)source
{
    self = [super init];
    if (self == nil) return nil;
    [self buildMainWindowWithTarget:target source:source];
    [self buildTargetsWindow];
    return self;
}

static NSWindow *make_window(NSString *title, NSSize size, BOOL resizable)
{
    NSWindowStyleMask style = NSWindowStyleMaskTitled | NSWindowStyleMaskClosable |
                              NSWindowStyleMaskMiniaturizable;
    if (resizable) style |= NSWindowStyleMaskResizable;
    /* defer:NO gives a window number before the window is ever shown. */
    NSWindow *window = [[NSWindow alloc] initWithContentRect:NSMakeRect(0, 0, size.width, size.height)
                                                   styleMask:style
                                                     backing:NSBackingStoreBuffered
                                                       defer:NO];
    window.title = title;
    window.releasedWhenClosed = NO;
    window.autorecalculatesKeyViewLoop = NO;
    window.restorable = NO;
    return window;
}

- (NSTableView *)makeTableWithSource:(id<NSTableViewDataSource, NSTableViewDelegate>)source
{
    NSTableView *table = [[NSTableView alloc] initWithFrame:NSZeroRect];
    struct {
        NSString *identifier;
        NSString *title;
        CGFloat width;
    } columns[] = {
        {@"capability", @"Capability", 150},
        {@"gui", @"In a SpaceTerm window", 135},
        {@"daemon", @"In a server session", 135},
    };
    for (size_t i = 0; i < sizeof columns / sizeof columns[0]; ++i) {
        NSTableColumn *column = [[NSTableColumn alloc] initWithIdentifier:columns[i].identifier];
        column.title = columns[i].title;
        column.width = columns[i].width;
        column.minWidth = 100;
        column.editable = NO;
        column.resizingMask = NSTableColumnAutoresizingMask;
        [table addTableColumn:column];
    }
    table.style = NSTableViewStyleFullWidth;
    table.rowHeight = 24;
    table.intercellSpacing = NSMakeSize(6, 2);
    table.usesAlternatingRowBackgroundColors = YES;
    table.allowsEmptySelection = NO;
    table.allowsMultipleSelection = NO;
    table.allowsColumnReordering = NO;
    table.columnAutoresizingStyle = NSTableViewUniformColumnAutoresizingStyle;
    table.dataSource = source;
    table.delegate = source;
    table.accessibilityLabel = @"Capabilities and their results";
    return table;
}

- (NSView *)permissionsRowWithTarget:(id)target
                              source:(id<NSTableViewDataSource, NSTableViewDelegate>)source
{
    _capabilityTable = [self makeTableWithSource:source];
    NSScrollView *scroll = [[NSScrollView alloc] initWithFrame:NSZeroRect];
    scroll.documentView = _capabilityTable;
    scroll.hasVerticalScroller = YES;
    scroll.autohidesScrollers = YES;
    scroll.borderType = NSBezelBorder;
    scroll.verticalScrollElasticity = NSScrollElasticityNone;
    scroll.translatesAutoresizingMaskIntoConstraints = NO;
    CGFloat rows = (CGFloat)ST_COMPANION_CAPABILITY_COUNT *
                   (_capabilityTable.rowHeight + _capabilityTable.intercellSpacing.height);
    CGFloat header = NSHeight(_capabilityTable.headerView.frame);
    [scroll.widthAnchor constraintEqualToConstant:ST_TABLE_WIDTH].active = YES;
    [scroll.heightAnchor constraintEqualToConstant:rows + header + 4].active = YES;

    _detailTitle = make_label(@"", NSFontTextStyleTitle3, YES, NSColor.labelColor, NO);
    _detailTitle.accessibilityRole = NSAccessibilityStaticTextRole;
    _detailSummary = make_label(@"", NSFontTextStyleBody, NO, NSColor.secondaryLabelColor, YES);
    _guiPanel = [[STContextPanel alloc] initWithHeading:@"In a SpaceTerm window"
                                                caption:@"GUI-parented shell"];
    _daemonPanel = [[STContextPanel alloc] initWithHeading:@"In a server session"
                                                   caption:@"Daemon-parented shell"];
    NSStackView *panels = stack(NSUserInterfaceLayoutOrientationHorizontal, 12,
                                @[_guiPanel, _daemonPanel]);
    panels.alignment = NSLayoutAttributeTop;
    panels.distribution = NSStackViewDistributionFillEqually;
    [_guiPanel.heightAnchor constraintEqualToAnchor:_daemonPanel.heightAnchor].active = YES;

    _openSettingsButton = button(@"Open Privacy Settings", target,
                                 @selector(openPrivacySettings:));
    _openSettingsButton.toolTip = @"Opens Privacy & Security in System Settings. Any change "
                                  @"there is yours to make.";
    _openSettingsButton.accessibilityHelp = _openSettingsButton.toolTip;
    _requestProbeButton = button(@"Request Probe", target, @selector(requestProbe:));
    _requestProbeButton.toolTip = @"Asks the evidence controller to run this case in a "
                                  @"SpaceTerm window and in a server session.";
    _requestProbeButton.accessibilityHelp = _requestProbeButton.toolTip;
    NSStackView *probeButtons = stack(NSUserInterfaceLayoutOrientationHorizontal, 8,
                                      @[_openSettingsButton, _requestProbeButton]);
    probeButtons.distribution = NSStackViewDistributionGravityAreas;
    _probeStatusLabel = make_label(@"", NSFontTextStyleCallout, NO, NSColor.labelColor, YES);
    _waitLabel =
        make_label(@"", NSFontTextStyleSubheadline, NO, NSColor.secondaryLabelColor, YES);

    NSStackView *detail = stack(NSUserInterfaceLayoutOrientationVertical, 6, @[
        _detailTitle, _detailSummary, panels, probeButtons, _probeStatusLabel, _waitLabel
    ]);
    [detail setCustomSpacing:12 afterView:_detailSummary];
    [detail setCustomSpacing:12 afterView:panels];
    [detail setCustomSpacing:8 afterView:probeButtons];
    fill_width(detail, @[_detailSummary, panels, _probeStatusLabel, _waitLabel]);
    detail.accessibilityElement = YES;
    detail.accessibilityRole = NSAccessibilityGroupRole;
    detail.accessibilityLabel = @"Selected capability";

    NSStackView *row = stack(NSUserInterfaceLayoutOrientationHorizontal, 20, @[scroll, detail]);
    row.alignment = NSLayoutAttributeTop;
    flexible(detail);
    return row;
}

- (NSView *)authorizationSection:(id)target
{
    NSTextField *heading = heading_label(@"Server Authorization");
    NSTextField *explanation = make_label(
        @"Requests go to SpaceTerm's management adapter. For Stop Server and Restart Server, the "
        @"companion first shows what will end and asks you to confirm. macOS then asks for "
        @"authorization in its own dialog. The companion never runs privileged code itself.",
        NSFontTextStyleBody, NO, NSColor.secondaryLabelColor, YES);
    NSTextField *operationLabel = make_label(@"Operation:", NSFontTextStyleBody, NO,
                                             NSColor.labelColor, NO);
    _operationPopUp = [[NSPopUpButton alloc] initWithFrame:NSZeroRect pullsDown:NO];
    _operationPopUp.translatesAutoresizingMaskIntoConstraints = NO;
    for (int operation = ST_COMPANION_OPERATION_START; operation <= ST_COMPANION_OPERATION_RESTART;
         ++operation) {
        [_operationPopUp addItemWithTitle:STCompanionString(st_companion_operation_title(
                                              (st_companion_operation)operation))];
        _operationPopUp.lastItem.tag = operation;
    }
    _operationPopUp.accessibilityLabel = @"Server operation";
    /* Assistive apps see the popup's cell, so the cell carries the label too. */
    _operationPopUp.cell.accessibilityLabel = @"Server operation";
    _requestAuthorizationButton =
        button(STCompanionString(st_companion_action_find(ST_COMPANION_ACTION_REQUEST_AUTHORIZATION)
                                     ->title),
               target, @selector(requestAuthorization:));
    _requestAuthorizationButton.toolTip = @"Asks the management adapter to perform the selected "
                                          @"operation. macOS asks for authorization separately.";
    _requestAuthorizationButton.accessibilityHelp = _requestAuthorizationButton.toolTip;
    NSStackView *operationRow = stack(NSUserInterfaceLayoutOrientationHorizontal, 8,
                                      @[operationLabel, _operationPopUp,
                                        _requestAuthorizationButton]);
    operationRow.distribution = NSStackViewDistributionGravityAreas;
    [operationRow setHuggingPriority:NSLayoutPriorityDefaultHigh
                      forOrientation:NSLayoutConstraintOrientationHorizontal];

    _authorizationSymbol = symbol_view();
    _authorizationTitle = make_label(@"", NSFontTextStyleBody, YES, NSColor.labelColor, NO);
    _authorizationDetail = make_label(@"", NSFontTextStyleCallout, NO, NSColor.labelColor, YES);
    NSStackView *text = stack(NSUserInterfaceLayoutOrientationVertical, 2,
                              @[_authorizationTitle, _authorizationDetail]);
    fill_width(text, @[_authorizationDetail]);
    NSStackView *status = stack(NSUserInterfaceLayoutOrientationHorizontal, 8,
                                @[_authorizationSymbol, text]);
    status.alignment = NSLayoutAttributeTop;
    flexible(text);
    status.accessibilityElement = YES;
    status.accessibilityRole = NSAccessibilityGroupRole;
    status.accessibilityLabel = @"Authorization status";

    /* Controls on the left, the latest outcome beside them. */
    NSStackView *controlRow = stack(NSUserInterfaceLayoutOrientationHorizontal, 24,
                                    @[operationRow, status]);
    controlRow.alignment = NSLayoutAttributeTop;
    flexible(status);
    NSStackView *section = stack(NSUserInterfaceLayoutOrientationVertical, 8,
                                 @[heading, explanation, controlRow]);
    [section setCustomSpacing:12 afterView:explanation];
    fill_width(section, @[explanation, controlRow]);
    section.accessibilityElement = YES;
    section.accessibilityRole = NSAccessibilityGroupRole;
    section.accessibilityLabel = @"Server Authorization";
    return section;
}

- (NSStackView *)footerWithTarget:(id)target
{
    _runSymbol = symbol_view();
    _runTitle = make_label(@"", NSFontTextStyleBody, YES, NSColor.labelColor, NO);
    _runDetail = make_label(@"", NSFontTextStyleCallout, NO, NSColor.secondaryLabelColor, YES);
    NSStackView *text = stack(NSUserInterfaceLayoutOrientationVertical, 2, @[_runTitle, _runDetail]);
    fill_width(text, @[_runDetail]);
    _showTargetsButton = button(@"Show Test Targets", target, @selector(showTestTargets:));
    _showTargetsButton.toolTip = @"Shows the synthetic targets the capability probe uses.";
    _showTargetsButton.accessibilityHelp = _showTargetsButton.toolTip;
    _stopRunButton = button(STCompanionString(st_companion_action_find(
                                ST_COMPANION_ACTION_STOP_RUN)->title),
                            target, @selector(stopEvidenceRun:));
    _stopRunButton.hasDestructiveAction = YES;
    _stopRunButton.toolTip = @"Asks the evidence controller to stop the run and clean up. The "
                             @"controller reports the cleanup result.";
    _stopRunButton.accessibilityHelp = _stopRunButton.toolTip;
    NSStackView *footer = stack(NSUserInterfaceLayoutOrientationHorizontal, 8,
                                @[_runSymbol, text, _showTargetsButton, _stopRunButton]);
    flexible(text);
    [footer setCustomSpacing:20 afterView:text];
    footer.edgeInsets = NSEdgeInsetsMake(12, 24, 14, 24);
    footer.accessibilityElement = YES;
    footer.accessibilityRole = NSAccessibilityGroupRole;
    footer.accessibilityLabel = @"Evidence run";
    return footer;
}

- (void)buildMainWindowWithTarget:(id)target
                           source:(id<NSTableViewDataSource, NSTableViewDelegate>)source
{
    NSTextField *title = make_label(@"SpaceTerm Native Evidence", NSFontTextStyleTitle2, YES,
                                    NSColor.labelColor, NO);
    title.accessibilityRole = NSAccessibilityStaticTextRole;
    _purposeLabel = make_label(
        @"This temporary companion supports the SpaceTerm Server evidence run. It shows which "
        @"process macOS asks for each System Permission, gives the capability probe synthetic "
        @"test targets, and requests server authorization through SpaceTerm's management "
        @"adapter. It never grants permissions, resets privacy settings, changes sudo or PAM "
        @"settings, or cleans up on its own.",
        NSFontTextStyleBody, NO, NSColor.secondaryLabelColor, YES);
    NSTextField *permissionsHeading = heading_label(@"System Permissions");
    _scopeLabel = make_label(STCompanionString(st_companion_permission_scope()),
                             NSFontTextStyleBody, NO, NSColor.labelColor, YES);
    NSView *permissions = [self permissionsRowWithTarget:target source:source];
    NSBox *divider = separator();
    NSView *authorization = [self authorizationSection:target];

    NSStackView *document = stack(NSUserInterfaceLayoutOrientationVertical, 8, @[
        title, _purposeLabel, permissionsHeading, _scopeLabel, permissions, divider,
        authorization
    ]);
    document.edgeInsets = NSEdgeInsetsMake(20, 24, 20, 24);
    [document setCustomSpacing:20 afterView:_purposeLabel];
    [document setCustomSpacing:12 afterView:_scopeLabel];
    [document setCustomSpacing:20 afterView:permissions];
    [document setCustomSpacing:16 afterView:divider];
    fill_width(document, @[_purposeLabel, _scopeLabel, permissions, divider, authorization]);

    NSStackView *footer = [self footerWithTarget:target];
    _document = document;
    _footer = footer;

    /* The document scrolls when the window is shorter than its content; the
     * run status and Stop Evidence Run stay visible below it. */
    NSScrollView *scroll = [[NSScrollView alloc] initWithFrame:NSZeroRect];
    STFlippedClipView *clip = [[STFlippedClipView alloc] initWithFrame:NSZeroRect];
    clip.drawsBackground = NO;
    scroll.contentView = clip;
    scroll.documentView = document;
    scroll.hasVerticalScroller = YES;
    scroll.autohidesScrollers = YES;
    scroll.drawsBackground = NO;
    scroll.borderType = NSNoBorder;
    scroll.translatesAutoresizingMaskIntoConstraints = NO;
    [NSLayoutConstraint activateConstraints:@[
        [document.topAnchor constraintEqualToAnchor:clip.topAnchor],
        [document.leadingAnchor constraintEqualToAnchor:clip.leadingAnchor],
        [document.widthAnchor constraintEqualToAnchor:clip.widthAnchor],
    ]];

    NSBox *footerDivider = separator();
    NSStackView *content = stack(NSUserInterfaceLayoutOrientationVertical, 0,
                                 @[scroll, footerDivider, footer]);
    content.alignment = NSLayoutAttributeLeading;
    fill_width(content, @[scroll, footerDivider, footer]);
    [scroll setContentHuggingPriority:1 forOrientation:NSLayoutConstraintOrientationVertical];
    [scroll setContentCompressionResistancePriority:1
                                     forOrientation:NSLayoutConstraintOrientationVertical];
    [scroll.heightAnchor constraintGreaterThanOrEqualToConstant:200].active = YES;

    _mainWindow = make_window(@"SpaceTerm Evidence Companion",
                              NSMakeSize(ST_MAIN_WIDTH, ST_MAIN_MAX_HEIGHT), YES);
    _mainWindow.contentMinSize = NSMakeSize(ST_MAIN_MIN_WIDTH, ST_MAIN_MIN_HEIGHT);
    _mainWindow.contentView = content;
    [self linkKeyViews:[self mainKeyViewOrder] window:_mainWindow];
    [NSNotificationCenter.defaultCenter addObserver:self
                                           selector:@selector(mainWindowDidEndLiveResize:)
                                               name:NSWindowDidEndLiveResizeNotification
                                             object:_mainWindow];
}

- (void)mainWindowDidEndLiveResize:(NSNotification *)notification
{
    (void)notification;
    /* After a person resizes the window, keep their size. */
    _mainSizedByPerson = YES;
}

- (CGFloat)mainContentHeight
{
    NSView *content = _mainWindow.contentView;
    /* Wrapping labels settle after a few passes; see STWrappingLabel. */
    for (int pass = 0; pass < 4; ++pass) {
        content.needsLayout = YES;
        [content layoutSubtreeIfNeeded];
    }
    return ceil(_document.fittingSize.height + _footer.fittingSize.height + 1);
}

- (void)sizeWindowsToFit
{
    CGFloat height = MIN(MAX([self mainContentHeight], ST_MAIN_MIN_HEIGHT), ST_MAIN_MAX_HEIGHT);
    [_mainWindow setContentSize:NSMakeSize(NSWidth(_mainWindow.contentView.frame), height)];
    [_mainWindow.contentView layoutSubtreeIfNeeded];
    [self fitTargetsWindow];
}

- (void)fitMainWindow
{
    if (_mainSizedByPerson) return;
    CGFloat needed = [self mainContentHeight];
    NSRect frame = _mainWindow.frame;
    NSRect content = [_mainWindow contentRectForFrameRect:frame];
    CGFloat limit = ST_MAIN_MAX_HEIGHT;
    NSScreen *screen = _mainWindow.screen;
    if (screen != nil) {
        CGFloat chrome = NSHeight(frame) - NSHeight(content);
        limit = MIN(limit, NSMaxY(frame) - NSMinY(screen.visibleFrame) - chrome);
    }
    CGFloat height = MIN(needed, limit);
    /* Grow only. Shrinking on every status change would make the window jump. */
    if (height <= NSHeight(content) + 0.5) return;
    content.origin.y -= height - NSHeight(content);
    content.size.height = height;
    [_mainWindow setFrame:[_mainWindow frameRectForContentRect:content]
                  display:_mainWindow.visible];
    [_mainWindow.contentView layoutSubtreeIfNeeded];
}

- (void)fitTargetsWindow
{
    NSView *targets = _targetsWindow.contentView;
    CGFloat height = fitting_height(targets, ST_TARGETS_WIDTH);
    NSRect content = [_targetsWindow contentRectForFrameRect:_targetsWindow.frame];
    if (fabs(NSHeight(content) - height) >= 0.5 || fabs(NSWidth(content) - ST_TARGETS_WIDTH) >= 0.5) {
        /* Keep the top edge, and so the capture pattern, where it was. */
        content.origin.y += NSHeight(content) - height;
        content.size = NSMakeSize(ST_TARGETS_WIDTH, height);
        NSRect frame = [_targetsWindow frameRectForContentRect:content];
        NSScreen *screen = _targetsWindow.screen;
        if (screen == nil) screen = _mainWindow.screen;
        if (_targetsPlaced && screen != nil) {
            /* Growth never pushes the bottom edge below the screen's. */
            NSRect visible = screen.visibleFrame;
            if (NSMinY(frame) < NSMinY(visible) && NSHeight(frame) <= NSHeight(visible)) {
                frame.origin.y = NSMinY(visible);
            }
        }
        [_targetsWindow setFrame:frame display:_targetsWindow.visible];
    }
    [targets setFrameSize:content.size];
    [targets layoutSubtreeIfNeeded];
}

- (void)placeTargetsWindowWithin:(NSRect)visible
{
    [self fitTargetsWindow];
    NSRect main = _mainWindow.frame;
    NSSize size = _targetsWindow.frame.size;
    CGFloat right = NSMaxX(main) + ST_WINDOW_GAP;
    CGFloat left = NSMinX(main) - ST_WINDOW_GAP - size.width;
    CGFloat x = NSMaxX(visible) - size.width;
    if (right >= NSMinX(visible) && right + size.width <= NSMaxX(visible)) {
        x = right;
    } else if (left >= NSMinX(visible) && left + size.width <= NSMaxX(visible)) {
        x = left;
    }
    x = MAX(x, NSMinX(visible));
    CGFloat y = MIN(NSMaxY(main), NSMaxY(visible)) - size.height;
    if (y < NSMinY(visible)) {
        y = size.height <= NSHeight(visible) ? NSMinY(visible) : NSMaxY(visible) - size.height;
    }
    [_targetsWindow setFrameOrigin:NSMakePoint(x, y)];
}

- (void)placeTargetsWindow
{
    if (_targetsPlaced) return;
    NSScreen *screen = _mainWindow.screen;
    if (screen == nil) screen = NSScreen.mainScreen;
    if (screen == nil) return;
    _targetsPlaced = YES;
    [self placeTargetsWindowWithin:screen.visibleFrame];
}

- (void)buildTargetsWindow
{
    NSTextField *intro = make_label(
        @"The capability probe uses these synthetic targets. They show and accept only test "
        @"markers and never read content from other apps.",
        NSFontTextStyleBody, NO, NSColor.secondaryLabelColor, YES);

    _capturePattern = [[STCapturePatternView alloc] initWithFrame:NSZeroRect];
    _captureStatus = make_label(@"", NSFontTextStyleCallout, NO, NSColor.labelColor, YES);

    _accessibilityTarget = [[STAccessibilityTargetField alloc] initWithFrame:NSZeroRect];
    _accessibilityTarget.accessibilityIdentifier =
        STCompanionString(ST_COMPANION_ACCESSIBILITY_IDENTIFIER);
    _accessibilityTarget.accessibilityLabel = @"Accessibility test field";
    _accessibilityTarget.accessibilityHelp = @"The capability probe sets this field's value "
                                             @"through Accessibility. The companion records "
                                             @"only whether the value matched.";
    [_accessibilityTarget.widthAnchor constraintEqualToConstant:300].active = YES;
    _accessibilityStatus = make_label(@"", NSFontTextStyleCallout, NO, NSColor.labelColor, YES);

    _clipboardTarget = [[STClipboardTargetView alloc] initWithFrame:NSZeroRect];
    _clipboardStatus = make_label(@"", NSFontTextStyleCallout, NO, NSColor.labelColor, YES);

    _urlStatus = make_label(@"", NSFontTextStyleCallout, NO, NSColor.labelColor, YES);
    _appleEventStatus = make_label(@"", NSFontTextStyleCallout, NO, NSColor.labelColor, YES);

    NSTextField *captureHeading = heading_label(@"Screen Recording target");
    NSTextField *accessibilityHeading = heading_label(@"Accessibility target");
    NSTextField *clipboardHeading = heading_label(@"Clipboard target");
    NSTextField *receiptsHeading = heading_label(@"URL and Apple Event receipts");
    NSStackView *root = stack(NSUserInterfaceLayoutOrientationVertical, 8, @[
        intro, captureHeading, _capturePattern, _captureStatus, accessibilityHeading,
        _accessibilityTarget, _accessibilityStatus, clipboardHeading, _clipboardTarget,
        _clipboardStatus, receiptsHeading, _urlStatus, _appleEventStatus
    ]);
    root.edgeInsets = NSEdgeInsetsMake(20, 24, 20, 24);
    for (NSView *view in @[intro, _captureStatus, _accessibilityStatus, _clipboardStatus]) {
        [root setCustomSpacing:18 afterView:view];
    }
    fill_width(root, @[intro, _captureStatus, _accessibilityStatus, _clipboardTarget,
                       _clipboardStatus, _urlStatus, _appleEventStatus]);
    _targetsWindow = make_window(@"SpaceTerm Evidence Targets",
                                 NSMakeSize(ST_TARGETS_WIDTH, 640), NO);
    /* Keep the capture pattern's sRGB values unchanged in the backing store. */
    _targetsWindow.colorSpace = NSColorSpace.sRGBColorSpace;
    _targetsWindow.contentView = root;
    [self linkKeyViews:[self targetsKeyViewOrder] window:_targetsWindow];
}

- (void)linkKeyViews:(NSArray<NSView *> *)order window:(NSWindow *)window
{
    for (NSUInteger i = 0; i < order.count; ++i) {
        order[i].nextKeyView = order[(i + 1) % order.count];
    }
    window.initialFirstResponder = order.firstObject;
}

- (NSArray<NSView *> *)mainKeyViewOrder
{
    return @[_capabilityTable, _openSettingsButton, _requestProbeButton, _operationPopUp,
             _requestAuthorizationButton, _showTargetsButton, _stopRunButton];
}

- (NSArray<NSView *> *)targetsKeyViewOrder
{
    return @[_clipboardTarget];
}

- (NSWindow *)mainWindow { return _mainWindow; }
- (NSWindow *)targetsWindow { return _targetsWindow; }
- (NSTextField *)purposeLabel { return _purposeLabel; }
- (NSTextField *)scopeLabel { return _scopeLabel; }
- (NSTableView *)capabilityTable { return _capabilityTable; }
- (NSTextField *)detailTitle { return _detailTitle; }
- (NSTextField *)detailSummary { return _detailSummary; }
- (STContextPanel *)guiPanel { return _guiPanel; }
- (STContextPanel *)daemonPanel { return _daemonPanel; }
- (NSTextField *)waitLabel { return _waitLabel; }
- (NSButton *)openSettingsButton { return _openSettingsButton; }
- (NSButton *)requestProbeButton { return _requestProbeButton; }
- (NSTextField *)probeStatusLabel { return _probeStatusLabel; }
- (NSPopUpButton *)operationPopUp { return _operationPopUp; }
- (NSButton *)requestAuthorizationButton { return _requestAuthorizationButton; }
- (NSImageView *)authorizationSymbol { return _authorizationSymbol; }
- (NSTextField *)authorizationTitle { return _authorizationTitle; }
- (NSTextField *)authorizationDetail { return _authorizationDetail; }
- (NSImageView *)runSymbol { return _runSymbol; }
- (NSTextField *)runTitle { return _runTitle; }
- (NSTextField *)runDetail { return _runDetail; }
- (NSButton *)showTargetsButton { return _showTargetsButton; }
- (NSButton *)stopRunButton { return _stopRunButton; }
- (STCapturePatternView *)capturePattern { return _capturePattern; }
- (NSTextField *)captureStatus { return _captureStatus; }
- (STAccessibilityTargetField *)accessibilityTarget { return _accessibilityTarget; }
- (NSTextField *)accessibilityStatus { return _accessibilityStatus; }
- (STClipboardTargetView *)clipboardTarget { return _clipboardTarget; }
- (NSTextField *)clipboardStatus { return _clipboardStatus; }
- (NSTextField *)urlStatus { return _urlStatus; }
- (NSTextField *)appleEventStatus { return _appleEventStatus; }
@end
