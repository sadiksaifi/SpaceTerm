#ifndef SPACETERM_LINUX_EVIDENCE_PROTOTYPE_H
#define SPACETERM_LINUX_EVIDENCE_PROTOTYPE_H

#include <stddef.h>
#include <sys/types.h>

/* Test-only service shape. These names never select production SpaceTerm code. */
#define ST_LINUX_EVIDENCE_DIRECTORY "/opt/spaceterm-issue520-linux-evidence"
#define ST_LINUX_EVIDENCE_REGISTRATION_DIRECTORY "/etc/spaceterm-issue520-linux-evidence"
#define ST_LINUX_EVIDENCE_REGISTRATION ST_LINUX_EVIDENCE_REGISTRATION_DIRECTORY "/registration"
#define ST_LINUX_EVIDENCE_LAUNCHER ST_LINUX_EVIDENCE_DIRECTORY "/launcher"
#define ST_LINUX_EVIDENCE_SERVER ST_LINUX_EVIDENCE_DIRECTORY "/server"
#define ST_LINUX_EVIDENCE_STOPPED_EXIT 78
#define ST_LINUX_REGISTRATION_MAX_BYTES 128

struct st_linux_registration {
    uid_t uid;
    gid_t gid;
    int running;
};

/* Exact bounded ASCII record, without paths, commands, credentials, or defaults. */
int st_linux_parse_registration(const unsigned char *bytes, size_t length,
                                struct st_linux_registration *out);

#endif
