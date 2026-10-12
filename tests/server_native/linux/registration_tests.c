#include "prototype.h"

#include <stdint.h>
#include <stdio.h>
#include <string.h>

static unsigned int cases;

static int accepted(const char *record, uint32_t uid, uint32_t gid, int running) {
    struct st_linux_registration out = {0};
    cases++;
    return st_linux_parse_registration((const unsigned char *)record, strlen(record), &out) == 0 &&
           out.uid == uid && out.gid == gid && out.running == running;
}

static int rejected(const unsigned char *record, size_t length) {
    struct st_linux_registration out = {37, 41, 1};
    struct st_linux_registration original = out;
    cases++;
    return st_linux_parse_registration(record, length, &out) != 0 &&
           out.uid == original.uid && out.gid == original.gid && out.running == original.running;
}

int main(void) {
    static const char *invalid[] = {
        "", "STLN1\n", "STLN2\nuid=1\ngid=1\nstate=running\n",
        "STLN1\nuid=0\ngid=1\nstate=running\n",
        "STLN1\nuid=1\ngid=0\nstate=running\n",
        "STLN1\nuid=01\ngid=1\nstate=running\n",
        "STLN1\nuid=1\ngid=01\nstate=running\n",
        "STLN1\nuid=-1\ngid=1\nstate=running\n",
        "STLN1\nuid=+1\ngid=1\nstate=running\n",
        "STLN1\nuid= 1\ngid=1\nstate=running\n",
        "STLN1\nuid=4294967295\ngid=1\nstate=running\n",
        "STLN1\nuid=1\ngid=4294967295\nstate=running\n",
        "STLN1\nuid=4294967296\ngid=1\nstate=running\n",
        "STLN1\nuid=18446744073709551616\ngid=1\nstate=running\n",
        "STLN1\nuid=1\ngid=1\nstate=unknown\n",
        "STLN1\nuid=1\ngid=1\nstate=Running\n",
        "STLN1\nuid=1\ngid=1\nstate=running",
        "STLN1\nuid=1\ngid=1\nstate=running\n\n",
        "STLN1\nuid=1\ngid=1\nstate=running\npath=/bin/sh\n",
        "STLN1\nuid=1\ngid=1\nstate=running\nuid=2\n",
        "STLN1\r\nuid=1\r\ngid=1\r\nstate=running\r\n",
        "STLN1\ngid=1\nuid=1\nstate=running\n",
        "STLN1\nuid=\ngid=1\nstate=running\n",
    };
    if (!accepted("STLN1\nuid=1000\ngid=1000\nstate=running\n", 1000, 1000, 1) ||
        !accepted("STLN1\nuid=1\ngid=2\nstate=stopped\n", 1, 2, 0) ||
        !accepted("STLN1\nuid=4294967294\ngid=4294967294\nstate=running\n",
                  UINT32_MAX - 1, UINT32_MAX - 1, 1)) {
        puts("scope=linux-launcher-prototype case=registration result=failed");
        return 1;
    }
    for (size_t index = 0; index < sizeof(invalid) / sizeof(invalid[0]); index++) {
        if (!rejected((const unsigned char *)invalid[index], strlen(invalid[index]))) {
            puts("scope=linux-launcher-prototype case=registration result=failed");
            return 1;
        }
    }
    static const unsigned char valid[] = "STLN1\nuid=1000\ngid=1000\nstate=running\n";
    for (size_t length = 0; length < sizeof(valid) - 1; length++) {
        if (!rejected(valid, length)) {
            puts("scope=linux-launcher-prototype case=registration result=failed");
            return 1;
        }
    }
    unsigned char binary[ST_LINUX_REGISTRATION_MAX_BYTES + 1];
    memset(binary, '1', sizeof(binary));
    if (!rejected(valid, sizeof(valid)) || !rejected(binary, sizeof(binary)) ||
        !rejected(NULL, 0)) {
        puts("scope=linux-launcher-prototype case=registration result=failed");
        return 1;
    }
    cases++;
    if (st_linux_parse_registration(valid, sizeof(valid) - 1, NULL) == 0) {
        puts("scope=linux-launcher-prototype case=registration result=failed");
        return 1;
    }
    printf("scope=linux-launcher-prototype case=registration result=passed cases=%u\n", cases);
    return 0;
}
