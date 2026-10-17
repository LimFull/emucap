#include <algorithm>
#include <cassert>
#include <cstdint>
#include <set>
#include <vector>

using u32 = uint32_t;
constexpr int BREAKPOINT_EE = 1, MEMCHECK_READ = 1, MEMCHECK_WRITE = 2, MEMCHECK_BREAK = 4;
u32 standardizeBreakpointAddress(u32 address) { return address & 0x1fffffff; }
struct EmuCapBreakpoint { u32 kind, start, end; };
struct MemCheck { u32 start, end; };
std::vector<EmuCapBreakpoint> s_emucap_breakpoints;
bool cpu_thread = false;
int native_calls = 0;
namespace Host {
template<class F> void RunOnCPUThread(F fn, bool wait) {
    assert(wait); cpu_thread = true; fn(); cpu_thread = false;
}
}
struct CBreakPoints {
    static inline std::set<u32> cpu;
    static inline std::vector<MemCheck> memory;
    static bool IsAddressBreakPoint(int, u32 address, bool* enabled) {
        assert(cpu_thread); ++native_calls; *enabled = true;
        return cpu.contains(standardizeBreakpointAddress(address));
    }
    static void AddBreakPoint(int, u32 address) {
        assert(cpu_thread); ++native_calls; cpu.insert(standardizeBreakpointAddress(address));
    }
    static auto GetMemChecks(int) { assert(cpu_thread); ++native_calls; return memory; }
    static void AddMemCheck(int, u32 start, u32 end, int, int) {
        assert(cpu_thread); ++native_calls; memory.push_back({start, end});
    }
    static void RemoveBreakPoint(int, u32 address) {
        assert(cpu_thread); ++native_calls; cpu.erase(standardizeBreakpointAddress(address));
    }
    static void RemoveMemCheck(int, u32 start, u32 end) {
        assert(cpu_thread); ++native_calls;
        std::erase_if(memory, [=](auto p) { return p.start == start && p.end == end; });
    }
};
#include "pcsx2-functions.inc"
int main() {
    // Existing GUI ownership remains independent, including aliases.
    CBreakPoints::cpu.insert(0x10000);
    assert(!add(0, 0x80010000, 0x80010000));
    assert(!clear(0, 0x10000, 0x10000));
    assert(CBreakPoints::cpu.contains(0x10000));
    assert(add(0, 0, 0));
    int before = native_calls;
    assert(!add(0, 0, 0) && native_calls == before);
    for (u32 i = 1; i < 128; ++i) assert(add(i % 3, i * 32, i * 32 + (i % 3 ? 3 : 0)));
    before = native_calls;
    for (u32 kind = 0; kind < 3; ++kind) assert(!add(kind, 0x20000, 0x20003));
    assert(native_calls == before && s_emucap_breakpoints.size() == 128);
    // Clearing another owner's point cannot open a quota slot.
    assert(!clear(0, 0x10000, 0x10000));
    assert(s_emucap_breakpoints.size() == 128 && CBreakPoints::cpu.contains(0x10000));
    for (u32 i = 0; i < 128; ++i) assert(clear(i % 3, i * 32, i * 32 + (i % 3 ? 3 : 0)));
    assert(s_emucap_breakpoints.empty() && CBreakPoints::memory.empty());
    assert(CBreakPoints::cpu.size() == 1);
    assert(add(1, 0x100, 0x103));
    assert(!add(2, 0x80000100, 0x80000103));
    assert(clear(1, 0x100, 0x103));
    assert(add(2, 0x100, 0x103));
}
