#define _GNU_SOURCE
#include "custody/probes.h"

#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/ptrace.h>
#include <sys/types.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <unistd.h>

struct witness {
    uintptr_t address;
    int descriptor;
    int dumpable;
};

static int child_control(int commands, int observations, int dumpable) {
    /* This is nonsecret control data, never a native or application credential. */
    volatile unsigned long control = 42;
    int fd = open("/dev/null", O_RDONLY | O_CLOEXEC);
    struct witness witness = {(uintptr_t)&control, fd, dumpable};
    if (fd < 0 || prctl(PR_SET_DUMPABLE, dumpable) != 0 ||
        prctl(PR_GET_DUMPABLE) != dumpable ||
        write(observations, &witness, sizeof(witness)) != (ssize_t)sizeof(witness)) {
        if (fd >= 0) close(fd);
        return 1;
    }
    char command;
    while (read(commands, &command, 1) == 1 && command == '0') {
        witness.dumpable = 0;
        if (prctl(PR_SET_DUMPABLE, 0) != 0 || prctl(PR_GET_DUMPABLE) != 0 ||
            write(observations, &witness, sizeof(witness)) != (ssize_t)sizeof(witness)) {
            close(fd);
            return 1;
        }
    }
    close(fd);
    return 0;
}

static int denied(int result) {
    return result == -1 && (errno == EPERM || errno == EACCES);
}

static int control(int dumpable) {
    int command[2], observed[2];
    if (pipe2(command, O_CLOEXEC) != 0) return 1;
    if (pipe2(observed, O_CLOEXEC) != 0) {
        close(command[0]); close(command[1]);
        return 1;
    }
    pid_t child = fork();
    if (child == 0) {
        alarm(10);
        close(command[1]); close(observed[0]);
        int result = child_control(command[0], observed[1], dumpable);
        close(command[0]); close(observed[1]);
        _exit(result);
    }
    close(command[0]); close(observed[1]);
    if (child < 0) {
        close(command[1]); close(observed[0]);
        return 1;
    }
    struct witness witness;
    int failed = read(observed[0], &witness, sizeof(witness)) != (ssize_t)sizeof(witness);
    int memory = -1;
    if (!failed) {
        unsigned long value = 0;
        struct iovec local = {&value, sizeof(value)};
        struct iovec remote = {(void *)witness.address, sizeof(value)};
        errno = 0;
        ssize_t result = process_vm_readv(child, &local, 1, &remote, 1, 0);
        failed |= dumpable ? result != (ssize_t)sizeof(value) || value != 42 : !denied((int)result);
        char path[80];
        snprintf(path, sizeof(path), "/proc/%ld/mem", (long)child);
        errno = 0;
        memory = open(path, O_RDONLY | O_CLOEXEC);
        failed |= dumpable ? memory < 0 : !denied(memory);
        if (memory >= 0) {
            failed |= pread(memory, &value, sizeof(value), (off_t)witness.address) !=
                (ssize_t)sizeof(value) || value != 42;
        }
        snprintf(path, sizeof(path), "/proc/%ld/environ", (long)child);
        errno = 0;
        int environment = open(path, O_RDONLY | O_CLOEXEC);
        failed |= dumpable ? environment < 0 : !denied(environment);
        if (environment >= 0) close(environment);
        snprintf(path, sizeof(path), "/proc/%ld/fd/%d", (long)child, witness.descriptor);
        char link[80];
        errno = 0;
        result = readlink(path, link, sizeof(link));
        failed |= dumpable ? result <= 0 : !denied((int)result);
        errno = 0;
        long attached = ptrace(PTRACE_ATTACH, child, NULL, NULL);
        int attached_error = errno;
        if (attached == 0) {
            int status;
            if (waitpid(child, &status, 0) != child || !WIFSTOPPED(status) ||
                ptrace(PTRACE_DETACH, child, NULL, NULL) != 0) failed = 1;
        }
        errno = attached_error;
        failed |= dumpable ? attached != 0 : !denied((int)attached);
        errno = 0;
        long seized = ptrace(PTRACE_SEIZE, child, NULL, NULL);
        attached_error = errno;
        if (seized == 0) {
            int status;
            if (ptrace(PTRACE_INTERRUPT, child, NULL, NULL) != 0 ||
                waitpid(child, &status, 0) != child || !WIFSTOPPED(status) ||
                ptrace(PTRACE_DETACH, child, NULL, NULL) != 0) failed = 1;
        }
        errno = attached_error;
        failed |= dumpable ? seized != 0 : !denied((int)seized);
        if (dumpable && memory >= 0 && !failed) {
            /* A pre-opened proc memory handle survives a later dumpability change.
               This control prevents treating a late PR_SET_DUMPABLE as custody. */
            failed |= write(command[1], "0", 1) != 1;
            failed |= read(observed[0], &witness, sizeof(witness)) != (ssize_t)sizeof(witness);
            failed |= witness.dumpable != 0;
            failed |= pread(memory, &value, sizeof(value), (off_t)witness.address) !=
                (ssize_t)sizeof(value) || value != 42;
        }
    }
    if (memory >= 0) close(memory);
    close(command[1]); close(observed[0]);
    int status;
    if (waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0) failed = 1;
    return failed;
}

int st_custody_memory_controls(void) {
    return control(1) | control(0);
}
