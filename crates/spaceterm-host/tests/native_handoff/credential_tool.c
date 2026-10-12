/* Fixed unprivileged synthetic tool. No credential literal or native facility. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <sys/random.h>
#include <poll.h>
#include <unistd.h>

#ifndef FIXTURE_REPLACEMENT
extern char **environ;
static int digit(unsigned char ch) {
    if (ch >= 'A' && ch <= 'Z') return ch - 'A';
    if (ch >= 'a' && ch <= 'z') return ch - 'a' + 26;
    if (ch >= '0' && ch <= '9') return ch - '0' + 52;
    if (ch == '+') return 62;
    if (ch == '/') return 63;
    return -1;
}
static size_t decode(const unsigned char *input, size_t length, unsigned char *out) {
    unsigned int bits = 0, value = 0;
    size_t used = 0;
    for (size_t i = 0; i < length; ++i) {
        if (input[i] == '=') break;
        int n = digit(input[i]);
        if (n < 0) return 0;
        value = (value << 6) | (unsigned int)n;
        bits += 6;
        if (bits >= 8) { bits -= 8; out[used++] = (unsigned char)(value >> bits); }
    }
    return used;
}
static int environment(void) {
    const char *expected[] = {"PATH=/usr/bin:/bin", "LC_ALL=C", "SYSTEMD_LOG_LEVEL=emerg"};
    unsigned int seen = 0;
    for (size_t i = 0; environ[i] != NULL; ++i) {
        if (i >= 3) return 0;
        unsigned int bit = 0;
        for (unsigned int j = 0; j < 3; ++j) {
            if (strcmp(environ[i], expected[j]) == 0) bit = 1u << j;
        }
        if (bit == 0 || (seen & bit) != 0) return 0;
        seen |= bit;
    }
    return seen == 7;
}
#endif
int main(int argc, char **argv) {
#ifdef FIXTURE_REPLACEMENT
    (void)argc; (void)argv;
    return 28;
#else
    if (prctl(PR_SET_DUMPABLE, 0, 0, 0, 0) != 0 || prctl(PR_GET_DUMPABLE, 0, 0, 0, 0) != 0) return 28;
    unsigned char encoded[8192], decoded[8192];
    size_t used = 0;
    for (;;) {
        if (used == sizeof(encoded)) return 30;
        ssize_t n = read(0, encoded + used, sizeof(encoded) - used);
        if (n < 0 && errno == EINTR) continue;
        if (n < 0) return 34;
        if (n == 0) break;
        used += (size_t)n;
    }
    size_t length = decode(encoded, used, decoded);
    if (length < 160) return 9;
    unsigned int path_length = decoded[154] | ((unsigned int)decoded[155] << 8);
    unsigned int name_length = decoded[156] | ((unsigned int)decoded[157] << 8);
    if (path_length == 0 || path_length > 4096 || name_length > 128 || length != 160 + path_length + name_length) return 9;
    char path[4097], name[129];
    memcpy(path, decoded + 160, path_length); path[path_length] = 0;
    memcpy(name, decoded + 160 + path_length, name_length); name[name_length] = 0;
    int checked = argc == 8 && strcmp(argv[0], "systemd-creds") == 0 && strcmp(argv[1], "decrypt") == 0 &&
        strcmp(argv[2], name) == 0 && strcmp(argv[3], "--no-ask-password") == 0 &&
        strcmp(argv[4], "--refuse-null") == 0 && strcmp(argv[5], "--newline=no") == 0 &&
        strcmp(argv[6], "-") == 0 && strcmp(argv[7], "-") == 0 && environment();
    /* The executor must close every inherited descriptor, including retained authority. */
    for (int fd = 3; fd < 1024; ++fd) {
        if (fcntl(fd, F_GETFD) != -1 || errno != EBADF) checked = 0;
    }
    int audit = open(path, O_WRONLY | O_APPEND | O_CLOEXEC | O_NOFOLLOW);
    if (audit < 0) return 34;
    uint32_t record[] = {(uint32_t)getpid(), (uint32_t)checked};
    if (write(audit, record, sizeof(record)) != (ssize_t)sizeof(record) || close(audit) != 0) return 34;
    if (!checked) return 9;
    unsigned char *output = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (output == MAP_FAILED || mlock(output, 4096) != 0) return 21;
    if (getrandom(output, 33, 0) != 33) return 21;
    unsigned int scenario = decoded[152];
    size_t count = scenario == 1 ? 31 : scenario == 2 ? 33 : scenario == 5 || scenario == 6 ? 0 : 32;
    if (count != 0 && write(1, output, count) != (ssize_t)count) return 34;
    explicit_bzero(output, 4096);
    if (munlock(output, 4096) != 0 || munmap(output, 4096) != 0) return 33;
    if (scenario == 3 || scenario == 5) (void)poll(NULL, 0, 60000);
    return scenario == 4 ? 1 : 0;
#endif
}
