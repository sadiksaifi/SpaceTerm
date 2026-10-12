#define _GNU_SOURCE
#include "prototype.h"

#include <errno.h>
#include <fcntl.h>
#include <selinux/context.h>
#include <selinux/selinux.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

static unsigned int denials;

static void failed(const char *operation) {
    fprintf(stderr, "scope=linux-selinux-prototype classification=assertion-failed operation=%s\n", operation);
    exit(1);
}

static void denied(int result, const char *operation) {
    int failure = errno;
    if (result >= 0 || (failure != EACCES && failure != EPERM)) {
        failed(operation);
    }
    denials++;
}

int main(int argc, char **argv) {
    const char *directory = NULL;
    const char *label = NULL;
    if (argc < 2 || argc > 3 || getuid() == 0) {
        return 2;
    }
    if (strcmp(argv[1], "installation") == 0) {
        directory = ST_LINUX_EVIDENCE_DIRECTORY "/custody";
        label = "installation";
    } else if (strcmp(argv[1], "registration") == 0) {
        directory = ST_LINUX_EVIDENCE_REGISTRATION_DIRECTORY "/custody";
        label = "registration";
    } else if (strcmp(argv[1], "alias") == 0) {
        directory = "/mnt/spaceterm-issue520-linux-evidence-alias";
        label = "ordinary-bind-alias";
    } else {
        return 2;
    }
    char file[256], victim[256], entry[256];
    snprintf(file, sizeof(file), "%s/file", directory);
    snprintf(victim, sizeof(victim), "%s/victim", directory);
    snprintf(entry, sizeof(entry), "%s/new-entry", directory);
    char *context = NULL;
    if (is_selinux_enabled() != 1 || security_getenforce() != 1 || getcon(&context) != 0) {
        failed("enforcement");
    }
    if (argc == 3 && strcmp(argv[2], "--unconfined-control") == 0) {
        if (strstr(context, ":unconfined_t:") == NULL) {
            failed("unconfined-context");
        }
        int fd = open(file, O_WRONLY | O_CLOEXEC);
        if (fd < 0 || write(fd, "0", 1) != 1 || close(fd) != 0) {
            failed("unconfined-write-control");
        }
        puts("scope=linux-selinux-prototype domain=unconfined_t account_write=allowed full_account_custody=unproven");
        freecon(context);
        return 0;
    }
    if (argc != 2 || strstr(context, ":unconfined_t:") == NULL) {
        failed("initial-context");
    }
    context_t confined = context_new(context);
    if (confined == NULL || context_type_set(confined, "stln_probe_t") != 0 ||
        setcon(context_str(confined)) != 0) {
        failed("subject-transition");
    }
    context_free(confined);
    freecon(context);
    context = NULL;
    if (getcon(&context) != 0 || strstr(context, ":stln_probe_t:") == NULL) {
        failed("confined-context");
    }
    freecon(context);
    int read_fd = open(file, O_RDONLY | O_CLOEXEC);
    char byte;
    if (read_fd < 0 || read(read_fd, &byte, 1) != 1 || read(4, &byte, 1) != 1) {
        failed("protected-read-control");
    }
    close(read_fd);

    errno = 0;
    denied(open(file, O_WRONLY | O_CLOEXEC), "write-open");
    errno = 0;
    denied(truncate(file, 0), "truncate");
    errno = 0;
    denied(chmod(file, 0600), "attributes");
    errno = 0;
    denied(link(file, entry), "link");
    errno = 0;
    denied(unlink(victim), "unlink");
    errno = 0;
    denied(rename(victim, entry), "rename");
    errno = 0;
    denied(open(entry, O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC, 0600), "create");
    errno = 0;
    denied(mkdir(entry, 0700), "directory-create");
    errno = 0;
    denied((int)write(4, "0", 1), "retained-write");
    errno = 0;
    denied(ftruncate(4, 0), "retained-truncate");
    errno = 0;
    denied(fchmod(4, 0600), "retained-attributes");
    errno = 0;
    void *mapping = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, 4, 0);
    if (mapping != MAP_FAILED || (errno != EACCES && errno != EPERM)) {
        failed("retained-shared-writable-mapping");
    }
    denials++;

    /* Independently writable DAC/SELinux control, not another protected path. */
    const char *control = "/var/tmp/spaceterm-issue520-linux-evidence-control/file";
    int fd = open(control, O_RDWR | O_CLOEXEC);
    if (fd < 0 || write(fd, "1", 1) != 1 || fchmod(fd, 0666) != 0) {
        failed("writable-control");
    }
    mapping = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (mapping == MAP_FAILED) {
        failed("mapping-control");
    }
    ((char *)mapping)[0] = '0';
    munmap(mapping, 4096);
    close(fd);
    const char *renamed = "/var/tmp/spaceterm-issue520-linux-evidence-control/renamed";
    if (rename(control, renamed) != 0 || unlink(renamed) != 0) {
        failed("entry-control");
    }
    printf("scope=linux-selinux-prototype object=%s denied_operations=%u retained_fd=4 "
           "read_control=passed write_control=passed mapping_control=passed entry_control=passed\n",
           label, denials);
    return 0;
}
