#include "prototype.h"

#include <stdint.h>
#include <string.h>

struct cursor {
    const unsigned char *bytes;
    size_t length;
};

static int literal(struct cursor *cursor, const char *value) {
    size_t length = strlen(value);
    if (cursor->length < length || memcmp(cursor->bytes, value, length) != 0) {
        return -1;
    }
    cursor->bytes += length;
    cursor->length -= length;
    return 0;
}

static int account_id(struct cursor *cursor, uint32_t *out) {
    uint32_t value = 0;
    size_t index = 0;
    if (cursor->length == 0 || cursor->bytes[0] < '1' || cursor->bytes[0] > '9') {
        return -1;
    }
    while (index < cursor->length && cursor->bytes[index] >= '0' && cursor->bytes[index] <= '9') {
        uint32_t digit = cursor->bytes[index] - '0';
        if (value > (UINT32_MAX - 1 - digit) / 10) {
            return -1;
        }
        value = value * 10 + digit;
        index++;
    }
    cursor->bytes += index;
    cursor->length -= index;
    *out = value;
    return 0;
}

int st_linux_parse_registration(const unsigned char *bytes, size_t length,
                                struct st_linux_registration *out) {
    if (bytes == NULL || out == NULL || length > ST_LINUX_REGISTRATION_MAX_BYTES) {
        return -1;
    }
    struct cursor cursor = {bytes, length};
    uint32_t uid, gid;
    int running;
    if (literal(&cursor, "STLN1\nuid=") != 0 || account_id(&cursor, &uid) != 0 ||
        literal(&cursor, "\ngid=") != 0 || account_id(&cursor, &gid) != 0 ||
        literal(&cursor, "\nstate=") != 0) {
        return -1;
    }
    if (literal(&cursor, "running\n") == 0) {
        running = 1;
    } else if (literal(&cursor, "stopped\n") == 0) {
        running = 0;
    } else {
        return -1;
    }
    if (cursor.length != 0 || (uint32_t)(uid_t)uid != uid || (uint32_t)(gid_t)gid != gid) {
        return -1;
    }
    *out = (struct st_linux_registration){(uid_t)uid, (gid_t)gid, running};
    return 0;
}
