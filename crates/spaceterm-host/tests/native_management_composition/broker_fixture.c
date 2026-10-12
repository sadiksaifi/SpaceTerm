/* Unprivileged test peer for retained native execution. Never shipped or linked into production. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/socket.h>
#include <sys/stat.h>
#ifndef OUTPUT_MODE
#define OUTPUT_MODE 0
#endif
int main(int argc, char **argv) {
    int position = 1, polkit = 0;
    const char *operation = NULL;
    if (argc < 5) return 91;
    if (strcmp(argv[position], "--disable-internal-agent") == 0) {
        polkit = 1; position++;
        if (strcmp(argv[position++], "fixture-manager") != 0) return 92;
        operation = argv[position++];
    } else {
        if (strcmp(argv[position++], "-k") != 0) return 91;
        if (strcmp(argv[position], "-n") == 0) position++;
        if (strcmp(argv[position++], "--") != 0
            || strcmp(argv[position++], "fixture-manager") != 0) return 92;
    }
    if (strcmp(argv[position++], "--request-stdin") != 0 || position != argc) return 92;
    if (getenv("LD_PRELOAD") != NULL || getenv("DYLD_INSERT_LIBRARIES") != NULL
        || getenv("SSH_ASKPASS") != NULL || getenv("SUDO_ASKPASS") != NULL
        || getenv("SPACETERM_SIGNING_KEYCHAIN") != NULL) return 93;
    if (getenv("PATH") == NULL || strcmp(getenv("PATH"), "/usr/bin:/bin") != 0
        || getenv("LC_ALL") == NULL || strcmp(getenv("LC_ALL"), "C") != 0) return 94;
#if OUTPUT_MODE == 7
    /* pkexec's documented pre-exec cancellation, before any manager request. */
    return 126;
#elif OUTPUT_MODE == 8
    /* Denial, a missing agent or a native launch failure before manager exec. */
    return 127;
#endif
    struct stat transport;
    if (fstat(0, &transport) != 0 || !S_ISSOCK(transport.st_mode)) return 97;
    unsigned char request[160]; size_t used = 0;
    while (used < sizeof(request)) {
        ssize_t count = read(0, request + used, sizeof(request) - used);
        if (count <= 0) return 95;
        used += (size_t)count;
    }
    if (polkit && (request[5] != 4 || strcmp(operation, "start") != 0)) return 92;
#if OUTPUT_MODE == 5
    fputs("classification=3\n", stdout); return 3;
#elif OUTPUT_MODE == 6
    fputs("progress=", stdout);
    for (int i = 16; i < 32; i++) printf("%02x", request[i]);
    fputs(",7,2\n", stdout); return 0;
#endif
    /* This nonauthorizing peer tests exchange mechanics, not native caller or custody proof. */
    unsigned char challenge[32]; memset(challenge, 42, sizeof(challenge));
    if (write(0, challenge, sizeof(challenge)) != sizeof(challenge)) return 98;
    unsigned char echoed[32]; used = 0;
    while (used < sizeof(echoed)) {
        ssize_t count = read(0, echoed + used, sizeof(echoed) - used);
        if (count <= 0) return 99;
        used += (size_t)count;
    }
    unsigned char excess;
    if (memcmp(challenge, echoed, sizeof(challenge)) != 0
        || read(0, &excess, 1) != 0 || memcmp(request, "STMG", 4) != 0) return 96;
    fputs("discarded-native-stderr\n", stderr);
#if OUTPUT_MODE == 1
    for (int i = 0; i < 128; i++) putchar('x');
#elif OUTPUT_MODE == 2
    fputs("classification=17\n", stdout); return 17;
#elif OUTPUT_MODE == 3
    return 127;
#else
    fputs("progress=", stdout);
    for (int i = 16; i < 32; i++) printf("%02x", request[i]);
    fputs(",7,2\n", stdout);
#if OUTPUT_MODE == 4
    return 1;
#endif
#endif
    return 0;
}
