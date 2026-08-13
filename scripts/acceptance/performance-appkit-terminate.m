#import <AppKit/AppKit.h>
#import <Foundation/Foundation.h>
#import <errno.h>
#import <libproc.h>
#import <limits.h>
#import <stdlib.h>
#import <string.h>
#import <unistd.h>

static void usage(void) {
    fprintf(stderr, "usage: performance-appkit-terminate --pid PID "
        "--process-start-identity SEC:USEC --bundle-identifier ID "
        "--executable ABSOLUTE_PATH --timeout-seconds N\n");
}

static BOOL parse_positive(const char *text, unsigned long long *value) {
    if (text == NULL || *text == '\0' || *text == '+' || *text == '-') return NO;
    char *end = NULL;
    errno = 0;
    unsigned long long parsed = strtoull(text, &end, 10);
    if (errno != 0 || end == text || *end != '\0' || parsed == 0) return NO;
    *value = parsed;
    return YES;
}

static BOOL read_start(pid_t pid, unsigned long long *seconds,
                       unsigned long long *microseconds) {
    struct proc_bsdinfo info = {0};
    int count = proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &info, sizeof(info));
    if (count != sizeof(info)) return NO;
    *seconds = info.pbi_start_tvsec;
    *microseconds = info.pbi_start_tvusec;
    return YES;
}

int main(int argc, const char *argv[]) {
    @autoreleasepool {
        const char *pid_text = NULL;
        const char *start_text = NULL;
        const char *bundle_text = NULL;
        const char *executable_text = NULL;
        const char *timeout_text = NULL;
        for (int index = 1; index < argc; index += 2) {
            if (index + 1 >= argc) { usage(); return 2; }
            if (strcmp(argv[index], "--pid") == 0) pid_text = argv[index + 1];
            else if (strcmp(argv[index], "--process-start-identity") == 0) start_text = argv[index + 1];
            else if (strcmp(argv[index], "--bundle-identifier") == 0) bundle_text = argv[index + 1];
            else if (strcmp(argv[index], "--executable") == 0) executable_text = argv[index + 1];
            else if (strcmp(argv[index], "--timeout-seconds") == 0) timeout_text = argv[index + 1];
            else { usage(); return 2; }
        }
        unsigned long long pid_value = 0, timeout_value = 0;
        unsigned long long expected_seconds = 0, expected_microseconds = 0;
        if (!parse_positive(pid_text, &pid_value) || pid_value > INT_MAX
            || !parse_positive(timeout_text, &timeout_value) || timeout_value > 120
            || start_text == NULL || bundle_text == NULL || executable_text == NULL
            || executable_text[0] != '/') {
            usage(); return 2;
        }
        char trailing = '\0';
        if (sscanf(start_text, "%llu:%llu%c", &expected_seconds,
                   &expected_microseconds, &trailing) != 2) {
            usage(); return 2;
        }
        pid_t pid = (pid_t)pid_value;
        unsigned long long actual_seconds = 0, actual_microseconds = 0;
        if (!read_start(pid, &actual_seconds, &actual_microseconds)
            || actual_seconds != expected_seconds || actual_microseconds != expected_microseconds) {
            fprintf(stderr, "exact process identity is not live\n"); return 3;
        }
        NSRunningApplication *application =
            [NSRunningApplication runningApplicationWithProcessIdentifier:pid];
        NSString *expectedBundle = [NSString stringWithUTF8String:bundle_text];
        NSString *expectedExecutable = [[NSString stringWithUTF8String:executable_text]
            stringByStandardizingPath];
        NSString *actualExecutable = application.executableURL.path.stringByStandardizingPath;
        if (application == nil || application.processIdentifier != pid
            || ![application.bundleIdentifier isEqualToString:expectedBundle]
            || ![actualExecutable isEqualToString:expectedExecutable]) {
            fprintf(stderr, "AppKit identity does not match frozen subject\n"); return 3;
        }
        if (![application terminate]) {
            fprintf(stderr, "normal AppKit termination was refused\n"); return 4;
        }
        unsigned long long polls = timeout_value * 100;
        for (unsigned long long index = 0; index < polls; index += 1) {
            unsigned long long seconds = 0, microseconds = 0;
            if (!read_start(pid, &seconds, &microseconds)) return 0;
            if (seconds != expected_seconds || microseconds != expected_microseconds) {
                fprintf(stderr, "PID was reused before exact absence was observed\n"); return 5;
            }
            usleep(10000);
        }
        fprintf(stderr, "normal AppKit termination timed out\n");
        return 6;
    }
}
