#import "companion_app.h"

#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <unistd.h>

/* Unprivileged connection to the evidence controller. The controller owns a
 * fresh private directory; the companion refuses anything that another
 * account could have created or could read. */

enum { ST_COMPANION_WRITE_TIMEOUT_MS = 1000 };

@implementation STSocketChannel {
    int _socket;
    dispatch_source_t _reader;
    st_companion_framer _framer;
    __weak id<STCompanionChannelDelegate> _delegate;
    BOOL _closed;
}

static int open_checked_directory(const char *path, uid_t uid)
{
    int directory = open(path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
    if (directory < 0) return -1;
    struct stat status;
    if (fstat(directory, &status) != 0 || !S_ISDIR(status.st_mode) || status.st_uid != uid ||
        (status.st_mode & 077) != 0) {
        close(directory);
        return -1;
    }
    return directory;
}

+ (nullable instancetype)connectToPath:(NSString *)path
                              delegate:(id<STCompanionChannelDelegate>)delegate
{
    const char *full = path.fileSystemRepresentation;
    size_t length = strlen(full);
    struct sockaddr_un address;
    memset(&address, 0, sizeof address);
    if (full[0] != '/' || length >= sizeof address.sun_path) return nil;

    NSString *parent = path.stringByDeletingLastPathComponent;
    NSString *leaf = path.lastPathComponent;
    if (parent.length == 0 || leaf.length == 0 || [leaf isEqualToString:@".."] ||
        [leaf isEqualToString:@"."]) {
        return nil;
    }
    uid_t uid = getuid();
    int directory = open_checked_directory(parent.fileSystemRepresentation, uid);
    if (directory < 0) return nil;
    struct stat status;
    BOOL owned = fstatat(directory, leaf.fileSystemRepresentation, &status,
                         AT_SYMLINK_NOFOLLOW) == 0 &&
                 S_ISSOCK(status.st_mode) && status.st_uid == uid;
    close(directory);
    if (!owned) return nil;

    int fd = socket(AF_UNIX, SOCK_STREAM, 0);
    if (fd < 0) return nil;
    int one = 1;
    if (fcntl(fd, F_SETFD, FD_CLOEXEC) != 0 ||
        setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &one, sizeof one) != 0) {
        close(fd);
        return nil;
    }
    address.sun_family = AF_UNIX;
    memcpy(address.sun_path, full, length);
    address.sun_len = (unsigned char)sizeof address;
    if (connect(fd, (const struct sockaddr *)&address, sizeof address) != 0) {
        close(fd);
        return nil;
    }
    uid_t peer_uid = 0;
    gid_t peer_gid = 0;
    if (getpeereid(fd, &peer_uid, &peer_gid) != 0 || peer_uid != uid) {
        close(fd);
        return nil;
    }
    int flags = fcntl(fd, F_GETFL);
    if (flags < 0 || fcntl(fd, F_SETFL, flags | O_NONBLOCK) != 0) {
        close(fd);
        return nil;
    }
    return [[STSocketChannel alloc] initWithSocket:fd delegate:delegate];
}

- (instancetype)initWithSocket:(int)fd delegate:(id<STCompanionChannelDelegate>)delegate
{
    self = [super init];
    if (self == nil) {
        close(fd);
        return nil;
    }
    _socket = fd;
    _delegate = delegate;
    st_companion_framer_init(&_framer);
    _reader = dispatch_source_create(DISPATCH_SOURCE_TYPE_READ, (uintptr_t)fd, 0,
                                     dispatch_get_main_queue());
    __weak STSocketChannel *weakSelf = self;
    dispatch_source_set_event_handler(_reader, ^{
        [weakSelf readAvailable];
    });
    dispatch_source_set_cancel_handler(_reader, ^{
        close(fd);
    });
    dispatch_resume(_reader);
    return self;
}

- (void)readAvailable
{
    uint8_t bytes[1024];
    for (;;) {
        if (_closed) return;
        ssize_t got = read(_socket, bytes, sizeof bytes);
        if (got < 0 && errno == EINTR) continue;
        if (got < 0 && (errno == EAGAIN || errno == EWOULDBLOCK)) return;
        if (got <= 0) {
            [self finishWithState:ST_COMPANION_RUN_CONTROLLER_LOST];
            return;
        }
        size_t offset = 0;
        while (offset < (size_t)got && !_closed) {
            size_t consumed = 0;
            const char *record = NULL;
            size_t record_length = 0;
            st_companion_frame_status status =
                st_companion_framer_push(&_framer, bytes + offset, (size_t)got - offset,
                                         &consumed, &record, &record_length);
            offset += consumed;
            if (status == ST_COMPANION_FRAME_POISONED) {
                [self finishWithState:ST_COMPANION_RUN_PROTOCOL_ERROR];
                return;
            }
            if (status == ST_COMPANION_FRAME_RECORD) {
                [_delegate channelDidReceiveRecord:record length:record_length];
            }
            if (consumed == 0) break;
        }
    }
}

- (void)finishWithState:(st_companion_run_state)state
{
    if (_closed) return;
    [self close];
    [_delegate channelDidCloseWithState:state];
}

- (void)sendRecord:(const char *)record length:(size_t)length
{
    size_t sent = 0;
    while (!_closed && sent < length) {
        ssize_t wrote = write(_socket, record + sent, length - sent);
        if (wrote > 0) {
            sent += (size_t)wrote;
            continue;
        }
        if (wrote < 0 && errno == EINTR) continue;
        if (wrote < 0 && (errno == EAGAIN || errno == EWOULDBLOCK)) {
            struct pollfd wait = {.fd = _socket, .events = POLLOUT, .revents = 0};
            if (poll(&wait, 1, ST_COMPANION_WRITE_TIMEOUT_MS) > 0) continue;
        }
        [self finishWithState:ST_COMPANION_RUN_CONTROLLER_LOST];
        return;
    }
}

- (void)close
{
    if (_closed) return;
    _closed = YES;
    dispatch_source_cancel(_reader);
}

- (void)dealloc
{
    if (!_closed) {
        _closed = YES;
        dispatch_source_cancel(_reader);
    }
}
@end
