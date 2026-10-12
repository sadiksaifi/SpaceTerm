#ifndef SPACETERM_LINUX_RECEIVER_ENTRY_H
#define SPACETERM_LINUX_RECEIVER_ENTRY_H
#include "spaceterm_native.h"
/* Earliest controlled entry, before Rust runtime construction. The launcher
 * exclusively owns anonymous startup descriptors 3 through 7. */
st_native_result st_native_receiver_prepare(void);
#endif
