#ifndef SPACETERM_CAPABILITY_CLIPBOARD_H
#define SPACETERM_CAPABILITY_CLIPBOARD_H
#include "capability_probe.h"

/* Read only the owner's explicitly prepared synthetic text. No write or restore
 * operation exists: other formats and intervening user changes stay untouched. */
typedef struct {
    void *context;
    int (*change_count)(void *, uint64_t *);
    int (*read_text)(void *, uint8_t *, size_t, size_t *);
    uint64_t (*now)(void *);
} st_capability_clipboard_peer;
void st_capability_clipboard_read(const st_capability_clipboard_peer *, const uint8_t run_id[16],
    uint64_t deadline_ns, st_capability_observation *);
#endif
