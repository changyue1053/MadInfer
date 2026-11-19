// Independent installed-header/static-archive link check, never executed here.
#include <madinfer.h>
_Static_assert(MADINFER_ABI_VERSION == 2u, "Expected ABI v2 header");
// External entry keeps references reachable without relying on whole-archive.
uint32_t madinfer_preview_link_probe(void) {
    int32_t error = -1;
    madinfer_str path = {0};
    madinfer_core_handle core = madinfer_core_create_with_backend(0, 0, 1, path, &error);
    uint32_t kind = 0;
    if (core) {
        (void)madinfer_backend_kind(core, &kind);
        madinfer_core_destroy(core);
    }
    return madinfer_abi_version() + kind;
}
