#define _GNU_SOURCE 1
#include "entry.h"
#include <fcntl.h>
#include <sys/prctl.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <sys/vfs.h>
#include <unistd.h>

st_native_result st_native_receiver_prepare(void) {
    st_native_result result = 0;
    for (int fd = 3; fd <= 7; ++fd) {
        int flags = fcntl(fd, F_GETFD);
        if (flags < 0 || fcntl(fd, F_SETFD, flags | FD_CLOEXEC) != 0) result = 34;
    }
    for (int fd = 3; fd <= 7 && result == 0; ++fd) {
        struct stat metadata;
        struct statfs filesystem;
        int flags = fcntl(fd, F_GETFL);
        int direction = fd == 4 || fd == 6 ? O_WRONLY : O_RDONLY;
        if (flags < 0 || fstat(fd, &metadata) != 0 || !S_ISFIFO(metadata.st_mode) ||
            fstatfs(fd, &filesystem) != 0 || filesystem.f_type != 0x50495045 ||
            (flags & O_ACCMODE) != direction) result = 8;
    }
    struct rlimit limit = {0, 0};
    if (result == 0 && setrlimit(RLIMIT_CORE, &limit) != 0) result = 2;
    int initial = prctl(PR_GET_DUMPABLE, 0, 0, 0, 0);
    /* A readable dumpable-1 exec cannot recover continuous custody by resetting
     * the flag after exec. Reject that transition rather than weakening proof. */
    if (result == 0 && (initial != 0 && initial != 2)) result = 51;
    if (result == 0 && (prctl(PR_SET_DUMPABLE, 0, 0, 0, 0) != 0 ||
        prctl(PR_GET_DUMPABLE, 0, 0, 0, 0) != 0)) result = 51;
    if (result != 0) for (int fd = 3; fd <= 7; ++fd) (void)close(fd);
    return result;
}
