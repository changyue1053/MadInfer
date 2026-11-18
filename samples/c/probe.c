#include "madinfer.h"
#include <assert.h>
#include <stddef.h>
int main(void) {
    assert(madinfer_abi_version() == MADINFER_ABI_VERSION);
    uint32_t backend = 42;
    assert(madinfer_backend_kind(NULL, &backend) == 4001);
    int32_t error = -1;
    madinfer_str empty = {0};
    assert(madinfer_core_create_with_backend(NULL, NULL, 99, empty, &error) == NULL);
    assert(error == 4001);
    madinfer_core_destroy(NULL);
    return 0;
}
