#include <stdint.h>
#include <unistd.h>

#ifndef __MUSL__
#error "__MUSL__ is not defined: the C flags from cargo-ohos did not reach cmake"
#endif

uint64_t fixture_cmake_answer(void) {
    return 42;
}

uint64_t fixture_cmake_page_size(void) {
    return (uint64_t)sysconf(_SC_PAGESIZE);
}
