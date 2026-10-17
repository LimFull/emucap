#include "emucap_pattern.h"
#include <cassert>
#include <cstdint>

struct Byte {
    unsigned value;
    static std::size_t comparisons;
    bool operator==(const Byte& other) const { ++comparisons; return value == other.value; }
};
std::size_t Byte::comparisons = 0;

int main()
{
    std::vector<Byte> data(1024 * 1024, Byte{0}), pattern(65536, Byte{0});
    pattern.back().value = 1;
    emucap_pattern_matches(data.data(), data.size(), pattern.data(), pattern.size(),
        [](std::size_t) { assert(false); return true; });
    assert(Byte::comparisons <= 4 * (data.size() + pattern.size()));

    // Exhaustively compare small binary strings against the simple specification.
    for (unsigned n = 0; n <= 7; ++n) for (unsigned bits = 0; bits < (1u << n); ++bits) {
        std::vector<uint8_t> input(n);
        for (unsigned i = 0; i < n; ++i) input[i] = (bits >> i) & 1;
        for (unsigned m = 1; m <= 4; ++m) for (unsigned pb = 0; pb < (1u << m); ++pb) {
            std::vector<uint8_t> needle(m);
            for (unsigned i = 0; i < m; ++i) needle[i] = (pb >> i) & 1;
            for (unsigned cap = 1; cap <= 3; ++cap) for (unsigned align = 1; align <= 3; ++align) {
                std::vector<std::size_t> expected, actual;
                for (unsigned i = 0; i + m <= n && expected.size() < cap; ++i) {
                    if ((i + 1) % align) continue;
                    bool same = true;
                    for (unsigned j = 0; j < m; ++j) if (input[i+j] != needle[j]) same = false;
                    if (same) expected.push_back(i);
                }
                emucap_pattern_matches(input.data(), input.size(), needle.data(), needle.size(),
                    [&](std::size_t i) {
                        if ((i + 1) % align) return true;
                        actual.push_back(i);
                        return actual.size() < cap;
                    });
                assert(actual == expected);
            }
        }
    }
}
