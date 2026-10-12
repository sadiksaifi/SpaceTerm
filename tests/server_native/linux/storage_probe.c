#define _GNU_SOURCE
#include "custody/probes.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <linux/stat.h>
#include <sys/stat.h>
#include <unistd.h>

int st_custody_storage_controls(int directory) {
    int file = openat(directory, "storage", O_RDWR | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0600);
    if (file < 0) return 1;
    struct statx before, after, parent;
    unsigned int fields = STATX_BASIC_STATS | STATX_BTIME | STATX_MNT_ID;
    int failed = statx(file, "", AT_EMPTY_PATH, fields, &before) != 0 ||
        statx(directory, "", AT_EMPTY_PATH, fields, &parent) != 0;
    if (!failed) {
        failed |= (before.stx_mask & (STATX_INO | STATX_MNT_ID)) != (STATX_INO | STATX_MNT_ID);
        failed |= before.stx_mnt_id != parent.stx_mnt_id;
        failed |= write(file, "nonsecret data", 14) != 14;
        failed |= fsync(file) != 0 || fsync(directory) != 0;
        failed |= renameat(directory, "storage", directory, "moved") != 0;
        failed |= statx(file, "", AT_EMPTY_PATH, fields, &after) != 0;
        failed |= before.stx_ino != after.stx_ino || before.stx_mnt_id != after.stx_mnt_id;
        if (before.stx_mask & STATX_BTIME) {
            failed |= !(after.stx_mask & STATX_BTIME) ||
                before.stx_btime.tv_sec != after.stx_btime.tv_sec ||
                before.stx_btime.tv_nsec != after.stx_btime.tv_nsec;
        }
        failed |= symlinkat("moved", directory, "substituted") != 0;
        errno = 0;
        int substituted = openat(directory, "substituted", O_RDONLY | O_NOFOLLOW | O_CLOEXEC);
        failed |= substituted >= 0 || errno != ELOOP;
        if (substituted >= 0) close(substituted);
    }
    failed |= close(file) != 0;
    if (unlinkat(directory, "storage", 0) != 0 && errno != ENOENT) failed = 1;
    if (unlinkat(directory, "moved", 0) != 0 && errno != ENOENT) failed = 1;
    if (unlinkat(directory, "substituted", 0) != 0 && errno != ENOENT) failed = 1;
    return failed;
}
