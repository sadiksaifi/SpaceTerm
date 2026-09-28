#include <stdbool.h>
#include <stdint.h>

typedef void (*SPTUpdateCallback)(void *, uint32_t, const char *, uint64_t, uint64_t);
typedef bool (*SPTUpdateValidator)(const char *, const char *, const char *, bool);
void *spt_updater_create(void *context, SPTUpdateCallback callback, SPTUpdateValidator validator);
bool spt_updater_check(void *session);
bool spt_updater_download(void *session);
void spt_updater_cancel(void *session);
bool spt_updater_install(void *session);
void spt_updater_destroy(void *session);

// Finish an already verified update on ordinary quit, without requesting termination.
bool spt_updater_finish_on_quit(void *session);
uint64_t spt_updater_read_history(void *session, uint8_t *bytes, uint64_t capacity);
void spt_updater_write_history(void *session, const uint8_t *bytes, uint64_t length);
