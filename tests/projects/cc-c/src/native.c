#include <stdint.h>
#include <string.h>
#include <unistd.h>

// cargo-ohos defines this, like the SDK's clang wrapper does. Without it the musl headers
// take the glibc path in places, so a missing definition has to be a hard error rather than
// something that shows up as a subtle runtime difference.
#ifndef __MUSL__
#error "__MUSL__ is not defined: the C flags from cargo-ohos did not reach cc-rs"
#endif

// Uses the sysroot's headers and a libc call, so a wrong --sysroot fails to compile or link.
uint64_t fixture_page_size(void) {
    return (uint64_t)sysconf(_SC_PAGESIZE);
}

uint64_t fixture_sum(const uint32_t *values, size_t len) {
    uint64_t sum = 0;
    for (size_t i = 0; i < len; i++) {
        sum += values[i];
    }
    return sum;
}

int fixture_pointer_width(void) {
    return (int)(sizeof(void *) * 8);
}
