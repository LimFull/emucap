#ifndef EMUCAP_PATTERN_H
#define EMUCAP_PATTERN_H
#include <cstddef>
#include <vector>

// Exact, overlapping matches in O(size + pattern_size) comparisons.
// Returning false from visit ends the scan; offsets are relative to data.
template<class Byte, class Visit>
void emucap_pattern_matches(const Byte* data, std::size_t size,
                           const Byte* pattern, std::size_t pattern_size, Visit visit)
{
    if (!pattern_size || pattern_size > size) return;
    std::vector<std::size_t> prefix(pattern_size, 0);
    for (std::size_t i = 1, matched = 0; i < pattern_size; ++i) {
        while (matched && !(pattern[i] == pattern[matched])) matched = prefix[matched - 1];
        if (pattern[i] == pattern[matched]) ++matched;
        prefix[i] = matched;
    }
    for (std::size_t i = 0, matched = 0; i < size; ++i) {
        while (matched && !(data[i] == pattern[matched])) matched = prefix[matched - 1];
        if (data[i] == pattern[matched]) ++matched;
        if (matched == pattern_size) {
            if (!visit(i + 1 - pattern_size)) return;
            matched = prefix[matched - 1];
        }
    }
}
#endif
