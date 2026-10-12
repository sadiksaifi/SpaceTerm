/* Test-only libc peers. Never linked into or shipped with the private tool. */
#define _GNU_SOURCE
#define _FILE_OFFSET_BITS 64
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <sys/random.h>
#include <sys/stat.h>
#include <unistd.h>

static uint32_t observations[9];
static int directory = -1, secret = -1;

static int selected(const char *name) {
    const char *value = getenv("ST_CREDENTIAL_PEER");
    return value && strcmp(value, name) == 0;
}

__attribute__((destructor)) static void finish(void) {
    /* Only bounded call counts leave the peer, through an anonymous pipe. */
    const char *output = getenv("ST_CREDENTIAL_PEER_FD");
    if (output &&
        write(atoi(output), observations, sizeof(observations)) != sizeof(observations))
        _exit(125);
}

int prctl(int operation, ...) {
    if (operation == PR_SET_DUMPABLE) {
        observations[0]++;
        if (selected("set")) {
            errno = EPERM;
            return -1;
        }
        int (*native)(int, ...) = dlsym(RTLD_NEXT, "prctl");
        return native(operation, 0, 0, 0, 0);
    }
    if (operation == PR_GET_DUMPABLE) {
        observations[1]++;
        return selected("get") ? 1 : 0;
    }
    /* The tested interface must not request any other prctl operation. */
    errno = EINVAL;
    return -1;
}

uid_t geteuid(void) { return 0; }

int mlockall(int flags) {
    observations[2]++;
    if (flags != (MCL_CURRENT | MCL_FUTURE) || selected("lock")) {
        errno = EPERM;
        return -1;
    }
    return 0; /* Injected observation, not real locked-memory evidence. */
}

static int native_open(const char *path, int flags, mode_t mode) {
    int (*native)(const char *, int, ...) = dlsym(RTLD_NEXT, "open");
    return native(path, flags, mode);
}

int open(const char *path, int flags, ...) {
    mode_t mode = 0;
    if (flags & O_CREAT) {
        va_list args;
        va_start(args, flags);
        mode = va_arg(args, unsigned);
        va_end(args);
    }
    if (strcmp(path, "/etc/machine-id") == 0) {
        static const char identity[] = "11111111111111111111111111111111\n";
        int fd = memfd_create("machine-identity-peer", MFD_CLOEXEC);
        if (fd < 0 || write(fd, identity, sizeof(identity) - 1) != sizeof(identity) - 1 ||
            lseek(fd, 0, SEEK_SET) < 0)
            abort();
        return fd;
    }
    if (strcmp(path, "/var/lib/systemd") == 0) {
        observations[4]++;
        if (selected("directory-missing")) {
            errno = ENOENT;
            return -1;
        }
        if ((flags & (O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)) !=
            (O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC) || (flags & O_ACCMODE) != O_RDONLY)
            observations[6]++;
        directory = memfd_create("credential-directory-peer", MFD_CLOEXEC);
        return directory;
    }
    return native_open(path, flags, mode);
}

int openat(int fd, const char *path, int flags, ...) {
    mode_t mode = 0;
    if (flags & O_CREAT) {
        va_list args;
        va_start(args, flags);
        mode = va_arg(args, unsigned);
        va_end(args);
    }
    if (fd == AT_FDCWD && strcmp(path, "/etc/machine-id") == 0)
        return open(path, flags);
    if (fd == directory && directory >= 0) {
        observations[5]++;
        if (strcmp(path, "credential.secret") != 0 ||
            (flags & (O_NOFOLLOW | O_CLOEXEC)) != (O_NOFOLLOW | O_CLOEXEC) ||
            (flags & O_ACCMODE) != O_RDONLY)
            observations[6]++;
        if (selected("key-missing")) {
            errno = ENOENT;
            return -1;
        }
        uint8_t data[4112];
        if (getrandom(data, sizeof(data), 0) != sizeof(data))
            abort();
        /* Public machine/app binding for the injected machine ID, not key bytes. */
        static const uint8_t binding[16] = {
            0xfe, 0x93, 0x47, 0x9d, 0xbc, 0x6e, 0x4a, 0xf4,
            0xb4, 0x6c, 0x43, 0xf3, 0x76, 0x96, 0xa2, 0xcc
        };
        memcpy(data, binding, sizeof(binding));
        if (selected("foreign"))
            memset(data, 0, sizeof(binding));
        secret = memfd_create("credential-key-peer", MFD_CLOEXEC);
        if (secret < 0 || write(secret, data, sizeof(data)) != sizeof(data) ||
            lseek(secret, 0, SEEK_SET) < 0)
            abort();
        explicit_bzero(data, sizeof(data));
        return secret;
    }
    int (*native)(int, const char *, int, ...) = dlsym(RTLD_NEXT, "openat");
    return native(fd, path, flags, mode);
}

int fstat(int fd, struct stat *st) {
    int (*native)(int, struct stat *) = dlsym(RTLD_NEXT, "fstat");
    int result = native(fd, st);
    if (result == 0 && fd == secret && secret >= 0) {
        st->st_uid = 0;
        st->st_mode = S_IFREG | 0400;
        st->st_nlink = 1;
        st->st_size = selected("oversized") ? 4113 : 4112;
    }
    return result;
}

ssize_t read(int fd, void *data, size_t size) {
    if (fd == STDIN_FILENO)
        observations[3]++;
    if (fd == secret && secret >= 0)
        observations[7]++;
    ssize_t (*native)(int, void *, size_t) = dlsym(RTLD_NEXT, "read");
    return native(fd, data, size);
}

int mkdir(const char *path, mode_t mode) {
    (void)path;
    (void)mode;
    observations[6]++;
    errno = EPERM;
    return -1;
}

int mkdirat(int fd, const char *path, mode_t mode) {
    (void)fd;
    return mkdir(path, mode);
}

int unlinkat(int fd, const char *path, int flags) {
    (void)fd;
    (void)path;
    (void)flags;
    observations[6]++;
    errno = EPERM;
    return -1;
}

void __explicit_bzero_chk(void *data, size_t size, size_t available) {
    void (*native)(void *, size_t, size_t) = dlsym(RTLD_NEXT, "__explicit_bzero_chk");
    native(data, size, available);
    if (size >= 4112)
        observations[8]++;
}

void explicit_bzero(void *data, size_t size) {
    void (*native)(void *, size_t) = dlsym(RTLD_NEXT, "explicit_bzero");
    native(data, size);
    if (size >= 4112)
        observations[8]++;
}
