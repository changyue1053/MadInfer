#ifndef MADINFER_TEST_OBS_ADAPTER_H
#define MADINFER_TEST_OBS_ADAPTER_H
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif
/* Test-only owner-thread function. Caller MUST join all inference callbacks
 * before passing their locked counters. No content/delta/opaque payload API.
 * wal_dir is a fresh local temporary directory, never an event attribute. */
int32_t madinfer_test_obs_after_join(const char *wal_dir, uint64_t started,
                                    uint64_t completed, uint64_t failed);
#ifdef __cplusplus
}
#endif
#endif
