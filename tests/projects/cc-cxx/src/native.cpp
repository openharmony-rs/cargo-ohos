#include <cstdint>
#include <stdexcept>
#include <string>
#include <vector>

#ifndef __MUSL__
#error "__MUSL__ is not defined: the C++ flags from cargo-ohos did not reach cc-rs"
#endif

namespace {

class Counter {
public:
    virtual ~Counter() = default;
    virtual uint64_t count(const std::string &text) const = 0;
};

class VowelCounter final : public Counter {
public:
    uint64_t count(const std::string &text) const override {
        uint64_t found = 0;
        for (char c : text) {
            if (std::string("aeiou").find(c) != std::string::npos) {
                found++;
            }
        }
        return found;
    }
};

} // namespace

extern "C" {

// Heap allocation, std::string, a vtable and the libc++ headers: linking the wrong C++
// runtime shows up here.
uint64_t fixture_count_vowels(const char *text) {
    std::vector<std::string> words{std::string(text)};
    VowelCounter counter;
    uint64_t total = 0;
    for (const auto &word : words) {
        total += counter.count(word);
    }
    return total;
}

// Throws and catches inside C++. Unwinding needs the C++ runtime the binary was linked
// against to be the one actually loaded at runtime: an ABI mismatch between the SDK's
// `libc++_shared.so` and an external toolchain's `libc++.so` (different ABI namespace)
// links fine and only fails here.
int fixture_throws_and_catches(int value) {
    try {
        if (value < 0) {
            throw std::out_of_range("negative");
        }
        return value * 2;
    } catch (const std::out_of_range &) {
        return -1;
    } catch (...) {
        return -2;
    }
}

} // extern "C"
