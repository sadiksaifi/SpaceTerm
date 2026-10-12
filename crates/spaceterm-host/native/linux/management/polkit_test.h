#ifndef SPACETERM_MANAGEMENT_POLKIT_TEST_H
#define SPACETERM_MANAGEMENT_POLKIT_TEST_H
#if !defined(ST_MANAGEMENT_TESTING)
#error "private native test composition only"
#endif
#include "polkit.h"
#include "polkit_native.h"
void st_management_polkit_test_set_api(const st_management_bus_api *api);
#endif
