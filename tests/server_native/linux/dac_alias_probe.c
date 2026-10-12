#define _GNU_SOURCE
#include "custody/probes.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

int st_custody_retained_writer_control(int directory) {
    int writer = openat(directory, "adopted", O_RDWR | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0600);
    if (writer < 0) return 1;
    int failed = ftruncate(writer, 4096) != 0;
    void *mapping = failed ? MAP_FAILED : mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, writer, 0);
    failed |= mapping == MAP_FAILED;
    int fresh = openat(directory, "fresh", O_RDWR | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0600);
    failed |= fresh < 0;
    if (!failed) {
        memcpy(mapping, "nonsecret control", 17);
        failed |= msync(mapping, 4096, MS_SYNC) != 0;
        char copy[4096];
        failed |= pread(writer, copy, sizeof(copy), 0) != (ssize_t)sizeof(copy);
        failed |= write(fresh, copy, sizeof(copy)) != (ssize_t)sizeof(copy);
        struct stat original, destination;
        failed |= fstat(writer, &original) != 0 || fstat(fresh, &destination) != 0;
        failed |= original.st_dev == destination.st_dev && original.st_ino == destination.st_ino;
        failed |= fchmod(writer, 0400) != 0;
        errno = 0;
        int denied = openat(directory, "adopted", O_RDWR | O_NOFOLLOW | O_CLOEXEC);
        failed |= denied >= 0 || errno != EACCES;
        if (denied >= 0) close(denied);
        /* chmod cannot revoke the old writer or its shared writable mapping. */
        failed |= pwrite(writer, "changed", 7, 0) != 7;
        memcpy(mapping, "retained", 8);
        failed |= msync(mapping, 4096, MS_SYNC) != 0;
        char check[8];
        failed |= pread(fresh, check, sizeof(check), 0) != (ssize_t)sizeof(check);
        failed |= memcmp(check, "nonsecre", sizeof(check)) != 0;
        failed |= ftruncate(writer, 1024) != 0;
        failed |= renameat(directory, "adopted", directory, "renamed") != 0;
        failed |= unlinkat(directory, "renamed", 0) != 0;
        failed |= pwrite(writer, "after unlink", 12, 0) != 12;
    }
    if (mapping != MAP_FAILED) failed |= munmap(mapping, 4096) != 0;
    failed |= close(writer) != 0;
    if (fresh >= 0) failed |= close(fresh) != 0;
    if (unlinkat(directory, "adopted", 0) != 0 && errno != ENOENT) failed = 1;
    if (unlinkat(directory, "renamed", 0) != 0 && errno != ENOENT) failed = 1;
    if (unlinkat(directory, "fresh", 0) != 0 && errno != ENOENT) failed = 1;
    return failed;
}
