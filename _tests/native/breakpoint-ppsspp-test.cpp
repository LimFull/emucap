#include <algorithm>
#include <cassert>
#include <cstdint>
#include <string>
#include <vector>

using u32 = uint32_t;
bool cpu_thread = false;
int compiled = 0, applied = 0, mutations = 0;
struct BreakPoint { u32 addr; };
struct MemCheck { u32 start, end; };
struct Manager {
    std::vector<BreakPoint> cpu;
    std::vector<MemCheck> memory;
    auto& GetBreakpointRefs() { assert(cpu_thread); return cpu; }
    auto& GetMemCheckRefs() { assert(cpu_thread); return memory; }
    void AddBreakPoint(u32 address) {
        assert(cpu_thread);
        ++mutations;
        if (std::none_of(cpu.begin(), cpu.end(), [=](auto p) { return p.addr == address; }))
            cpu.push_back({address});
    }
    void AddMemCheck(u32 address, u32 end, int, int) {
        assert(cpu_thread);
        ++mutations;
        if (std::none_of(memory.begin(), memory.end(), [=](auto p) { return p.start == address && p.end == end; }))
            memory.push_back({address, end});
    }
} g_breakpoints;
struct DebuggerRequest {
    u32 address, end;
    bool valid = true, condition = true, responded = false;
    std::string error;
    void Fail(std::string value) { error = std::move(value); }
    void Respond() { responded = true; }
};
struct Params {
    u32 address = 0, end = 0;
    int cond = 0;
    bool condition = true;
    bool Parse(DebuggerRequest& req) {
        address = req.address; end = req.end; condition = req.condition;
        return req.valid;
    }
    bool CompileCondition(std::string* error) {
        assert(cpu_thread); ++compiled;
        if (!condition) *error = "invalid condition";
        return condition;
    }
    void Apply() { assert(cpu_thread); ++applied; }
    int Action(bool enabled) { return enabled; }
};
using WebSocketCPUBreakpointParams = Params;
using WebSocketMemoryBreakpointParams = Params;
template<class F> void Core_RunOnCPUThread(F fn) { cpu_thread = true; fn(); cpu_thread = false; }
#include "ppsspp-functions.inc"

DebuggerRequest request(u32 address, u32 end) { return {address, end, true, true, false, {}}; }
int main() {
    // Both routes share one quota, including disabled/log-only registrations.
    for (u32 i = 0; i < 128; ++i) {
        auto req = request(i * 16, i * 16 + 4);
        if (i % 2) WebSocketMemoryBreakpointAdd(req); else WebSocketCPUBreakpointAdd(req);
        assert(req.responded && req.error.empty());
    }
    assert(g_breakpoints.cpu.size() == 64 && g_breakpoints.memory.size() == 64);
    for (auto add : {WebSocketCPUBreakpointAdd, WebSocketMemoryBreakpointAdd}) {
        int before = mutations, before_compile = compiled, before_apply = applied;
        auto req = request(0x10000, 0x10004);
        add(req);
        assert(!req.responded && !req.error.empty());
        assert(mutations == before && compiled == before_compile && applied == before_apply);
    }
    // Replacement is allowed at capacity and still updates options.
    auto cpu = request(0, 0), memory = request(16, 20);
    WebSocketCPUBreakpointAdd(cpu); WebSocketMemoryBreakpointAdd(memory);
    assert(cpu.responded && memory.responded && applied == 130);
    assert(g_breakpoints.cpu.size() + g_breakpoints.memory.size() == 128);
    // Same start with another end is a new identity, not replacement.
    auto different_range = request(16, 24);
    WebSocketMemoryBreakpointAdd(different_range);
    assert(!different_range.responded && !different_range.error.empty());
    // Bad replacement conditions do not modify existing registrations.
    cpu = request(0, 0); cpu.condition = false;
    const int before = mutations;
    WebSocketCPUBreakpointAdd(cpu);
    assert(cpu.error == "invalid condition" && mutations == before);
    g_breakpoints.cpu.pop_back();
    cpu = request(0x10000, 0x10004); WebSocketCPUBreakpointAdd(cpu);
    assert(cpu.responded && g_breakpoints.cpu.size() + g_breakpoints.memory.size() == 128);
    // Independent GUI growth above the limit cannot underflow aggregate admission.
    g_breakpoints.cpu.assign(129, BreakPoint{42});
    memory = request(0x20000, 0x20004); WebSocketMemoryBreakpointAdd(memory);
    assert(!memory.responded && !memory.error.empty());
    cpu = request(42, 42); WebSocketCPUBreakpointAdd(cpu);
    assert(cpu.responded && g_breakpoints.cpu.size() == 129);
}
