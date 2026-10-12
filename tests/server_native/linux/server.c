#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <grp.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/prctl.h>
#include <unistd.h>

static volatile sig_atomic_t stopped;

static void stop(int signal_number) {
    (void)signal_number;
    stopped = 1;
}

int main(void) {
    uid_t real_uid, effective_uid, saved_uid;
    gid_t real_gid, effective_gid, saved_gid;
    int dumpable = prctl(PR_GET_DUMPABLE, 0, 0, 0, 0);
    int no_new_privs = prctl(PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0);
    if (getresuid(&real_uid, &effective_uid, &saved_uid) != 0 ||
        getresgid(&real_gid, &effective_gid, &saved_gid) != 0 ||
        real_uid == 0 || real_uid != effective_uid || real_uid != saved_uid ||
        real_gid == 0 || real_gid != effective_gid || real_gid != saved_gid ||
        getgroups(0, NULL) != 0 || dumpable != 0 || no_new_privs != 1) {
        fputs("scope=linux-launcher-prototype classification=unsafe-final-state\n", stderr);
        return 1;
    }
    /* close_range plus CLOEXEC must leave only supervisor-owned stdio. */
    for (int fd = 3; fd < 1024; fd++) {
        errno = 0;
        if (fcntl(fd, F_GETFD) != -1 || errno != EBADF) {
            fputs("scope=linux-launcher-prototype classification=inherited-descriptor\n", stderr);
            return 1;
        }
    }
    struct sigaction action = {0};
    action.sa_handler = stop;
    sigemptyset(&action.sa_mask);
    sigaction(SIGTERM, &action, NULL);
    sigaction(SIGINT, &action, NULL);
    unsigned int sessions = 0;
    DIR *session_directory = opendir("/run/systemd/sessions");
    if (session_directory != NULL) {
        struct dirent *entry;
        while ((entry = readdir(session_directory)) != NULL) {
            /* Ignore logind's corresponding .ref FIFO entries. */
            if (strchr(entry->d_name, '.') == NULL) {
                sessions++;
            }
        }
        closedir(session_directory);
    } else if (errno != ENOENT) {
        fputs("scope=linux-launcher-prototype classification=sessions-unavailable\n", stderr);
        return 1;
    }
    printf("scope=linux-launcher-prototype login_sessions_at_entry=%u\n", sessions);
    printf("scope=linux-launcher-prototype uid=%u gid=%u groups=0 dumpable=%d no_new_privs=%d ready=1\n",
           (unsigned int)real_uid, (unsigned int)real_gid, dumpable, no_new_privs);
    fflush(stdout);
    /* No credentials, PTY, terminal programs, or listeners in this placeholder. */
    sigset_t blocked, previous;
    sigemptyset(&blocked);
    sigaddset(&blocked, SIGTERM);
    sigaddset(&blocked, SIGINT);
    sigprocmask(SIG_BLOCK, &blocked, &previous);
    while (!stopped) {
        sigsuspend(&previous);
    }
    return 0;
}
