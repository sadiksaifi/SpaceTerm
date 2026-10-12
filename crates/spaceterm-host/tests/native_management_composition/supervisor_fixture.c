#include <stdio.h>
#include <string.h>
#include <unistd.h>

/* A compiled native utility peer. It never touches launchd/systemd or credentials. */
int main(int argc, char **argv) {
    if (argc < 3 || strcmp(argv[1], "--scenario") != 0) return 9;
    if (strcmp(argv[2], "observe") == 0) {
        fputs("LoadState=loaded\nActiveState=active\n", stdout);
        return 0;
    }
    if (strcmp(argv[2], "denied") == 0) return 1;
    if (strcmp(argv[2], "overflow") == 0) {
        for (unsigned i = 0; i < 32768; i++) putchar('x');
        return 0;
    }
    if (strcmp(argv[2], "wait") == 0) {
        for (;;) pause();
    }
    return 9;
}
