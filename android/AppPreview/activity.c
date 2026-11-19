// Packaging fixture only. No model, inference, JNI API or lifecycle claim.
#include <android/native_activity.h>
#include <madinfer.h>
_Static_assert(MADINFER_ABI_VERSION == 2u, "Expected public ABI v2");
void ANativeActivity_onCreate(ANativeActivity *activity, void *saved_state, size_t saved_size) {
    (void)activity; (void)saved_state; (void)saved_size;
    // No host callbacks, resources or worker ownership are introduced here.
    (void)madinfer_abi_version();
}
