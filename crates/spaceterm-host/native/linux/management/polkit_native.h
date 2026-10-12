#ifndef SPACETERM_MANAGEMENT_POLKIT_NATIVE_H
#define SPACETERM_MANAGEMENT_POLKIT_NATIVE_H
#include <stdint.h>
typedef struct sd_bus sd_bus;
typedef struct sd_bus_message sd_bus_message;
typedef struct { const char *name, *message; int need_free; } sd_bus_error;
typedef struct {
    void *library;
    int (*bus_new)(sd_bus **);
    int (*set_address)(sd_bus *, const char *);
    int (*set_client)(sd_bus *, int);
    int (*start)(sd_bus *);
    int (*set_timeout)(sd_bus *, uint64_t);
    int (*call)(sd_bus *, const char *, const char *, const char *, const char *,
        sd_bus_error *, sd_bus_message **, const char *, ...);
    int (*enter)(sd_bus_message *, char, const char *);
    int (*exit)(sd_bus_message *);
    int (*read)(sd_bus_message *, const char *, ...);
    int (*at_end)(sd_bus_message *, int);
    const char *(*sender)(sd_bus_message *);
    sd_bus_message *(*message_unref)(sd_bus_message *);
    sd_bus *(*bus_unref)(sd_bus *);
    void (*error_free)(sd_bus_error *);
} st_management_bus_api;
#endif
