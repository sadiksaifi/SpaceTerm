#define _GNU_SOURCE
#include "prototype.h"

#include <errno.h>
#include <fcntl.h>
#include <linux/stat.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/vfs.h>
#include <unistd.h>

/* Native read-only observations. No publication issuer, IDMAP probe, or lease. */
static int observe(const char *label, const char *path, int directory) {
    int fd = open(path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW | (directory ? O_DIRECTORY : 0));
    struct stat metadata;
    struct statfs filesystem;
    struct statx selected;
    if (fd < 0 || fstat(fd, &metadata) != 0 || fstatfs(fd, &filesystem) != 0 ||
        statx(fd, "", AT_EMPTY_PATH | AT_STATX_SYNC_AS_STAT, STATX_BASIC_STATS | STATX_MNT_ID,
              &selected) != 0 || (selected.stx_mask & STATX_MNT_ID) == 0) {
        puts("scope=linux-root-observation classification=observation-unavailable");
        if (fd >= 0) {
            close(fd);
        }
        return 1;
    }
    printf("scope=linux-root-observation object=%s uid=%u gid=%u mode=%o inode=%llu "
           "mount_id=%llu statx_mask=%x filesystem=%lx authority=unavailable\n",
           label, (unsigned int)metadata.st_uid, (unsigned int)metadata.st_gid,
           metadata.st_mode & 07777, (unsigned long long)metadata.st_ino,
           (unsigned long long)selected.stx_mnt_id, selected.stx_mask,
           (unsigned long)filesystem.f_type);
    close(fd);
    return 0;
}

int main(void) {
    int result = observe("root", "/", 1) | observe("etc", "/etc", 1) |
                 observe("opt", "/opt", 1) |
                 observe("registration-directory", ST_LINUX_EVIDENCE_REGISTRATION_DIRECTORY, 1) |
                 observe("installation", ST_LINUX_EVIDENCE_DIRECTORY, 1) |
                 observe("registration", ST_LINUX_EVIDENCE_REGISTRATION, 0) |
                 observe("server", ST_LINUX_EVIDENCE_SERVER, 0);
#if defined(__x86_64__)
    /* x86_64 syscall 457 is absent on Linux 5.15. A null request cannot issue
       evidence on newer kernels either. Do not infer authority from errno. */
    errno = 0;
    long query = syscall(457, NULL, NULL, 0u, 0u);
    printf("scope=linux-root-observation query=statmount availability=%s\n",
           query == -1 && errno == ENOSYS ? "unsupported" : "not-established");
#endif
    puts("scope=linux-root-observation old_clone_idmap=unproven publication_custody=unproven");
    return result;
}
