#include <limits.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/stat.h>

static bool identity_is_valid(
    dev_t mount_device,
    mode_t device_mode,
    dev_t device_identity,
    uint64_t mount_flags
) {
    return (S_ISBLK(device_mode) || S_ISCHR(device_mode)) &&
        mount_device == device_identity &&
        (mount_flags & MNT_RDONLY) != 0;
}

static int self_test(void) {
    const dev_t device = 17;
    if (!identity_is_valid(device, S_IFBLK, device, MNT_RDONLY) ||
        !identity_is_valid(device, S_IFCHR, device, MNT_RDONLY) ||
        identity_is_valid(device, S_IFREG, device, MNT_RDONLY) ||
        identity_is_valid(device, S_IFCHR, device + 1, MNT_RDONLY) ||
        identity_is_valid(device, S_IFCHR, device, 0)) {
        return 1;
    }
    return 0;
}

int main(int argc, char **argv) {
    if (argc == 2 && strcmp(argv[1], "--self-test") == 0) {
        return self_test();
    }
    if (argc != 3) {
        return 2;
    }

    char mount_path[PATH_MAX];
    char device_path[PATH_MAX];
    if (realpath(argv[1], mount_path) == NULL || realpath(argv[2], device_path) == NULL ||
        strcmp(argv[1], mount_path) != 0 || strcmp(argv[2], device_path) != 0) {
        return 1;
    }

    struct stat mount_status = {0};
    struct stat device_status = {0};
    struct statfs filesystem = {0};
    if (lstat(mount_path, &mount_status) != 0 || !S_ISDIR(mount_status.st_mode) ||
        lstat(device_path, &device_status) != 0 || statfs(mount_path, &filesystem) != 0 ||
        !identity_is_valid(
            mount_status.st_dev, device_status.st_mode, device_status.st_rdev,
            filesystem.f_flags)) {
        return 1;
    }

    char filesystem_mount[PATH_MAX];
    if (realpath(filesystem.f_mntonname, filesystem_mount) == NULL ||
        strcmp(filesystem_mount, mount_path) != 0) {
        return 1;
    }
    return 0;
}
