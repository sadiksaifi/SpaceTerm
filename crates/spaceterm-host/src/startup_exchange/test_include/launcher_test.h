#ifndef ST_LAUNCHER_TEST_H
#define ST_LAUNCHER_TEST_H
#ifndef ST_NATIVE_TESTING
#error "Native callbacks are available only in test composition"
#endif
#include <sys/types.h>
#include "native_admission.h"
typedef struct {
    int (*groups)(int, const gid_t *);
    int (*gid)(gid_t);
    int (*uid)(uid_t);
    st_native_result (*observe)(uint32_t, st_native_privilege_observation *);
} st_launcher_test_calls;
void st_launcher_test_inject(const st_launcher_test_calls *calls);
#endif
