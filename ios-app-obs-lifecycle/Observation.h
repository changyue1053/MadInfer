/* App-only, fixed local observation input. No strings, IDs, times or payload. */
#include <stdint.h>
typedef enum {
    MI_DEMO_NO_CANCELLATION = 0,
    MI_DEMO_EXPLICIT_CANCELLATION = 3,
    MI_DEMO_DESTROY_CANCELLATION = 4
} mi_demo_cancellation_status;
typedef struct {
    mi_demo_cancellation_status explicit_cancel;
    mi_demo_cancellation_status destroy_cancel;
} mi_demo_cancellation_record;

#ifdef __cplusplus
extern "C" {
#endif
int32_t madinfer_test_obs_lifecycle_after_join(const char *wal_dir, const mi_demo_cancellation_record *record);
#ifdef __cplusplus
}
#endif
