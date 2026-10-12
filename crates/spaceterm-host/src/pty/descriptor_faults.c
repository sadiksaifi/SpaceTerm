// Native-test interposition at the OS boundary. This library is never linked into SpaceTerm.
#include <dirent.h>
#include <dlfcn.h>
#include <errno.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static pid_t initial_process;
__attribute__((constructor)) static void capture_process(void) {
    initial_process = getpid();
}

static int fault_is(const char *mode) {
    const char *requested = getenv("SPACETERM_TEST_DESCRIPTOR_FAULT");
    return requested != NULL && strcmp(requested, mode) == 0;
}

#ifdef __APPLE__
#include <libproc.h>
#include <sys/proc_info.h>

#define INTERPOSE(replacement, original) \
    __attribute__((used)) static struct { const void *replace; const void *with; } \
        interpose_##original __attribute__((section("__DATA,__interpose"))) = \
        { (const void *)&replacement, (const void *)&original }

static DIR *fault_opendir(const char *path) {
    if (fault_is("enumeration") && strcmp(path, "/dev/fd") == 0) {
        errno = EACCES;
        return NULL;
    }
    return opendir(path);
}
INTERPOSE(fault_opendir, opendir);

static int fault_proc_pidinfo(int pid, int flavor, uint64_t arg, void *buffer, int size) {
    if (fault_is("cleanup") && getpid() != initial_process && flavor == PROC_PIDLISTFDS) {
        errno = EACCES;
        return 0;
    }
    return proc_pidinfo(pid, flavor, arg, buffer, size);
}
INTERPOSE(fault_proc_pidinfo, proc_pidinfo);
#else
DIR *opendir(const char *path) {
    if (fault_is("enumeration") && strcmp(path, "/dev/fd") == 0) {
        errno = EACCES;
        return NULL;
    }
    DIR *(*original)(const char *) = dlsym(RTLD_NEXT, "opendir");
    return original(path);
}

#endif
