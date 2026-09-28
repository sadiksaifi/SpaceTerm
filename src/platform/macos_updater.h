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
