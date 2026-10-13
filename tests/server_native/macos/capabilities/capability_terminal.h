#ifndef SPACETERM_CAPABILITY_TERMINAL_H
#define SPACETERM_CAPABILITY_TERMINAL_H
#include "capability_probe.h"
#include <sys/types.h>
#include <termios.h>

typedef struct {
    int owner, master, slave, raw;
    struct termios original, forwarding;
} st_capability_terminal;

int st_capability_terminal_prepare(int owner, st_capability_terminal *out);
/* Called only in the gated child, before exec. No allocation or framework calls. */
int st_capability_terminal_child_setup(st_capability_terminal *terminal);
int st_capability_terminal_wait(st_capability_terminal *terminal, pid_t child, uint64_t deadline, int *status);
int st_capability_terminal_finish(st_capability_terminal *terminal);
/* Restore only original modes or this adapter's exact raw forwarding modes. */
int st_capability_terminal_restore_owner(int owner, const struct termios *original);
#endif
