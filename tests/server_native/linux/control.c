#define _GNU_SOURCE
#include "prototype.h"

#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <sys/stat.h>
#include <unistd.h>

static void denied(const char *classification) {
    fprintf(stderr, "scope=linux-launcher-prototype classification=%s\n", classification);
    exit(1);
}

static int protected_directory(int parent, const char *name) {
    struct stat metadata;
    int fd = openat(parent, name, O_RDONLY | O_CLOEXEC | O_DIRECTORY | O_NOFOLLOW);
    if (fd < 0 || fstat(fd, &metadata) != 0 || metadata.st_uid != 0 ||
        metadata.st_gid != 0 || (metadata.st_mode & 0022) != 0) {
        denied("unsafe-directory");
    }
    return fd;
}

int main(int argc, char **argv) {
    if (getuid() != 0 || geteuid() != 0 || argc != 2 ||
        (strcmp(argv[1], "running") != 0 && strcmp(argv[1], "stopped") != 0)) {
        denied("invalid-invocation");
    }
    int root = protected_directory(AT_FDCWD, "/");
    int etc = protected_directory(root, "etc");
    int directory = protected_directory(etc, "spaceterm-issue520-linux-evidence");
    int lock = openat(directory, "lock", O_RDWR | O_CLOEXEC | O_NOFOLLOW);
    struct stat metadata;
    if (lock < 0 || fstat(lock, &metadata) != 0 || !S_ISREG(metadata.st_mode) ||
        metadata.st_uid != 0 || metadata.st_gid != 0 || metadata.st_nlink != 1 ||
        (metadata.st_mode & 07777) != 0600 || flock(lock, LOCK_EX) != 0) {
        denied("unsafe-lock");
    }
    int old = openat(directory, "registration", O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    if (old < 0 || fstat(old, &metadata) != 0 || !S_ISREG(metadata.st_mode) ||
        metadata.st_uid != 0 || metadata.st_gid != 0 || metadata.st_nlink != 1 ||
        (metadata.st_mode & 07777) != 0600) {
        denied("unsafe-registration");
    }
    unsigned char bytes[ST_LINUX_REGISTRATION_MAX_BYTES + 1];
    ssize_t length = read(old, bytes, sizeof(bytes));
    struct st_linux_registration registration;
    if (length < 0 || st_linux_parse_registration(bytes, (size_t)length, &registration) != 0) {
        denied("unsafe-registration");
    }
    close(old);
    char next[ST_LINUX_REGISTRATION_MAX_BYTES];
    int size = snprintf(next, sizeof(next), "STLN1\nuid=%u\ngid=%u\nstate=%s\n",
                        (unsigned int)registration.uid, (unsigned int)registration.gid, argv[1]);
    if (size < 0 || (size_t)size >= sizeof(next)) {
        denied("unsafe-registration");
    }
    /* A leftover next record is uncertain publication, never implicit recovery. */
    int new = openat(directory, "registration.next", O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC |
                    O_NOFOLLOW, 0600);
    if (new < 0) {
        denied("publication-unavailable");
    }
    if (write(new, next, (size_t)size) != size || fsync(new) != 0 || close(new) != 0 ||
        renameat(directory, "registration.next", directory, "registration") != 0 ||
        fsync(directory) != 0) {
        denied("publication-uncertain");
    }
    printf("scope=linux-launcher-prototype desired_state=%s durable=1\n", argv[1]);
    close(lock);
    close(directory);
    close(etc);
    close(root);
    return 0;
}
