#define _GNU_SOURCE
#include "prototype.h"

#include <errno.h>
#include <fcntl.h>
#include <grp.h>
#include <linux/capability.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/prctl.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

static void denied(const char *classification) {
    fprintf(stderr, "scope=linux-launcher-prototype classification=%s\n", classification);
    exit(1);
}

static int directory(int parent, const char *name) {
    int fd = openat(parent, name, O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW);
    struct stat metadata;
    if (fd < 0 || fstat(fd, &metadata) != 0 || metadata.st_uid != 0 ||
        metadata.st_gid != 0 || (metadata.st_mode & 0022) != 0) {
        denied("unsafe-directory");
    }
    return fd;
}

static int regular_file(int parent, const char *name, mode_t mode) {
    int fd = openat(parent, name, O_RDONLY | O_NONBLOCK | O_CLOEXEC | O_NOFOLLOW);
    struct stat metadata;
    if (fd < 0 || fstat(fd, &metadata) != 0 || !S_ISREG(metadata.st_mode) ||
        metadata.st_uid != 0 || metadata.st_gid != 0 || metadata.st_nlink != 1 ||
        (metadata.st_mode & 07777) != mode) {
        denied("unsafe-file");
    }
    return fd;
}

int main(int argc, char **argv) {
    (void)argv;
    uid_t real_uid, effective_uid, saved_uid;
    if (argc != 1 || getresuid(&real_uid, &effective_uid, &saved_uid) != 0 ||
        real_uid != 0 || effective_uid != 0 || saved_uid != 0) {
        denied("invalid-invocation");
    }
    int root = directory(AT_FDCWD, "/");
    int etc = directory(root, "etc");
    int registration_directory = directory(etc, "spaceterm-issue520-linux-evidence");
    int record = regular_file(registration_directory, "registration", 0600);
    unsigned char bytes[ST_LINUX_REGISTRATION_MAX_BYTES + 1];
    ssize_t length = read(record, bytes, sizeof(bytes));
    struct st_linux_registration registration;
    if (length < 0 || st_linux_parse_registration(bytes, (size_t)length, &registration) != 0) {
        denied("unsafe-registration");
    }
    if (!registration.running) {
        puts("scope=linux-launcher-prototype classification=deliberately-stopped");
        return ST_LINUX_EVIDENCE_STOPPED_EXIT;
    }

    /* DAC checks demonstrate a service shape, not continuous inode custody. */
    int opt = directory(root, "opt");
    int installation = directory(opt, "spaceterm-issue520-linux-evidence");
    int server = regular_file(installation, "server", 0501);
    unsigned char elf[4];
    if (pread(server, elf, sizeof(elf), 0) != sizeof(elf) ||
        elf[0] != 0x7f || elf[1] != 'E' || elf[2] != 'L' || elf[3] != 'F') {
        denied("invalid-executable");
    }
    int dumpable = open("/proc/sys/fs/suid_dumpable", O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    char sysctl[4];
    if (dumpable < 0 || read(dumpable, sysctl, sizeof(sysctl)) != 2 ||
        sysctl[0] != '0' || sysctl[1] != '\n') {
        denied("unsafe-dumpability-policy");
    }
    close(dumpable);
    close(record);
    close(registration_directory);
    close(etc);
    close(installation);
    close(opt);
    close(root);

    int supplementary_groups = getgroups(0, NULL);
    if (supplementary_groups < 0) {
        denied("initial-groups-unavailable");
    }
    printf("scope=linux-launcher-prototype initial_supplementary_groups=%d\n", supplementary_groups);
    fflush(stdout);
    for (int capability = 0; capability <= CAP_LAST_CAP; capability++) {
        if (prctl(PR_CAPBSET_DROP, capability, 0, 0, 0) != 0) {
            denied("privilege-drop-failed");
        }
    }
    if (prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_CLEAR_ALL, 0, 0, 0) != 0 ||
        prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 || setgroups(0, NULL) != 0 ||
        setresgid(registration.gid, registration.gid, registration.gid) != 0 ||
        setresuid(registration.uid, registration.uid, registration.uid) != 0) {
        denied("privilege-drop-failed");
    }
    struct __user_cap_header_struct header = {_LINUX_CAPABILITY_VERSION_3, 0};
    struct __user_cap_data_struct capabilities[2] = {{0}, {0}};
    if (syscall(SYS_capset, &header, capabilities) != 0 ||
        prctl(PR_SET_DUMPABLE, 0, 0, 0, 0) != 0) {
        denied("privilege-drop-failed");
    }
    if (server != 3 && dup3(server, 3, O_CLOEXEC) < 0) {
        denied("descriptor-cleanup-failed");
    }
    if (syscall(SYS_close_range, 4u, ~0u, 0u) != 0) {
        denied("descriptor-cleanup-failed");
    }
    if (chdir("/") != 0) {
        denied("invalid-working-directory");
    }
    char *arguments[] = {"spaceterm-linux-evidence-server", NULL};
    char *environment[] = {"PATH=/usr/bin:/bin", "LANG=C", NULL};
    /* The unreadable main ELF plus suid_dumpable=0 preserves dumpability at exec.
       PR_SET_DUMPABLE alone would be reset by an ordinary readable same-UID exec. */
    fexecve(3, arguments, environment);
    denied("exec-failed");
}
