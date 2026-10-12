#define _GNU_SOURCE
#include "probes.h"

#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    char directory[] = "/tmp/spaceterm-custody-controls-XXXXXX";
    if (mkdtemp(directory) == NULL) {
        puts("classification=fixture-unavailable");
        return 1;
    }
    int fd = open(directory, O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW);
    int result = fd < 0;
    if (!result) {
        result |= st_custody_memory_controls();
        result |= st_custody_retained_writer_control(fd);
        result |= st_custody_storage_controls(fd);
        result |= close(fd) != 0;
    }
    result |= rmdir(directory) != 0;
    puts(result ? "classification=mechanics-failed" : "classification=mechanics-passed");
    return result ? 1 : 0;
}
