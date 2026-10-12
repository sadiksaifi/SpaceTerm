#ifndef SPACETERM_NATIVE_RECEIVER_ENTRY_H
#define SPACETERM_NATIVE_RECEIVER_ENTRY_H
#include "spaceterm_native.h"

/* Earliest controlled native server entry, before Rust runtime construction.
 * Restores inheritance protection on context 3, ready 4, delivery 5,
 * confirmation 6, acknowledgement 7 and Darwin group report 8. These are
 * anonymous launcher channels, never selectors supplied by argv/environment.
 * This operation grants no release authority and never creates a listener. */
st_native_result st_native_receiver_prepare(void);
#endif
