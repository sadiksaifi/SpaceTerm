#define _DARWIN_C_SOURCE 1
#include "receiver_entry.h"
#include <fcntl.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <unistd.h>

st_native_result st_native_receiver_prepare(void) {
    st_native_result result = 0;
    /* Set every inheritance flag before validating direction or doing anything
     * that could lead to an ordinary child. Failure still consumes all channels. */
    for (int fd = 3; fd <= 8; ++fd) {
        int flags = fcntl(fd, F_GETFD);
        if (flags < 0 || fcntl(fd, F_SETFD, flags | FD_CLOEXEC) != 0) result = 34;
    }
    for (int fd = 3; fd <= 8 && result == 0; ++fd) {
        struct stat metadata;
        int flags = fcntl(fd, F_GETFL);
        int direction = fd == 4 || fd == 6 || fd == 8 ? O_WRONLY : O_RDONLY;
        if (flags < 0 || fstat(fd, &metadata) != 0 || !S_ISFIFO(metadata.st_mode) ||
            (flags & O_ACCMODE) != direction) result = 8;
    }
    struct rlimit limit = {0, 0};
    if (result == 0 && setrlimit(RLIMIT_CORE, &limit) != 0) result = 2;
    if (result != 0) for (int fd = 3; fd <= 8; ++fd) (void)close(fd);
    return result;
}
