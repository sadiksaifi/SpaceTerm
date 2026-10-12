#import "companion_app.h"

/* The companion's only pasteboard access. The controller calls the metadata
 * methods only while a marker is armed, and copyPlainText only after the
 * ledger allowed a person's Edit > Paste. Nothing here writes, clears, or
 * restores the pasteboard. */

static NSArray<NSPasteboardType> *plain_text_types(void)
{
    return @[NSPasteboardTypeString, @"NSStringPboardType"];
}

@implementation STGeneralPasteboard

- (NSInteger)changeCount
{
    return NSPasteboard.generalPasteboard.changeCount;
}

- (BOOL)holdsOnlyPlainText
{
    NSArray<NSPasteboardItem *> *items = NSPasteboard.generalPasteboard.pasteboardItems;
    if (items.count != 1) return NO;
    NSArray<NSPasteboardType> *types = items.firstObject.types;
    if (types.count == 0) return NO;
    NSArray<NSPasteboardType> *allowed = plain_text_types();
    for (NSPasteboardType type in types) {
        if (![allowed containsObject:type]) return NO;
    }
    return YES;
}

- (BOOL)copyPlainText:(uint8_t *)buffer capacity:(size_t)capacity length:(size_t *)length
{
    *length = 0;
    NSString *text = [NSPasteboard.generalPasteboard stringForType:NSPasteboardTypeString];
    if (text == nil || text.length > capacity) return NO;
    NSUInteger used = 0;
    NSRange remaining = NSMakeRange(0, 0);
    BOOL converted = [text getBytes:buffer
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
@end
