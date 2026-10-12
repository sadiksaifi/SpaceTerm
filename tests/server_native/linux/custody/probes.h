#ifndef SPACETERM_CUSTODY_PROBES_H
#define SPACETERM_CUSTODY_PROBES_H

/* Mechanics only. These controls neither issue custody nor receive credentials. */
int st_custody_memory_controls(void);
int st_custody_retained_writer_control(int directory);
int st_custody_storage_controls(int directory);

#endif
