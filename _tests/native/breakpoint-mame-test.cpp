#include <cassert>
#include <cstdint>
#include <cstdio>
#include <forward_list>
#include <limits>
#include <list>
#include <map>
#include <memory>
#include <string>
#include <string_view>
#include <vector>

using u32 = uint32_t;
using offs_t = uint64_t;
enum class read_or_write { READ, WRITE, READWRITE };
struct address_space { int slot = 0; int spacenum() const { return slot; } };
struct debugger_cpu {
    u32 m_bpindex = 1, m_wpindex = 1, m_rpindex = 1, m_epindex = 1;
#include "mame-allocator.inc"
};
struct device_debug;
struct Device {
    debugger_cpu points;
    device_debug* owner = nullptr;
    Device& machine() { return *this; }
    Device& debugger() { return *this; }
    debugger_cpu& cpu() { return points; }
    device_debug* debug() { return owner; }
};
struct debug_breakpoint {
    template<class... T> explicit debug_breakpoint(T&&...) {}
};
using debug_watchpoint = debug_breakpoint;
using debug_exceptionpoint = debug_breakpoint;
struct debug_registerpoint {
    int m_index;
    debug_registerpoint(int&, u32 id, const char*, std::string_view) : m_index(id) {}
};
struct device_debug {
    Device m_device;
    int symbols = 0, flags = 0;
    int* m_symtable = &symbols;
    std::multimap<offs_t, std::unique_ptr<debug_breakpoint>> m_bplist;
    std::vector<std::list<std::unique_ptr<debug_watchpoint>>> m_wplist;
    std::forward_list<debug_registerpoint> m_rplist;
    std::multimap<int, std::unique_ptr<debug_exceptionpoint>> m_eplist;
    device_debug() { m_device.owner = this; }
    int breakpoint_set(offs_t, const char* = nullptr, std::string_view = {});
    int watchpoint_set(address_space&, read_or_write, offs_t, offs_t, const char* = nullptr, std::string_view = {});
    int registerpoint_set(const char*, std::string_view);
    int exceptionpoint_set(int, const char*, std::string_view);
    void breakpoint_update_flags() { ++flags; }
};
struct device_memory_interface {
    static constexpr int TR_READ = 0;
    bool valid = true;
    bool translate(int, int, offs_t& offset, address_space*& space) {
        space = nullptr; offset += 16; return valid;
    }
};
struct Console {
    Device* device;
    Device* get_visible_cpu() { return device; }
};
struct debug_gdbstub {
    enum cmd_reply { REPLY_OK, REPLY_ENN, REPLY_UNSUPPORTED };
    device_memory_interface* m_memory;
    address_space* m_address_space;
    Console* m_debugger_console;
    std::map<offs_t, offs_t> m_address_map;
    cmd_reply handle_Z(const char*);
};
bool parse_zZ(int* type, uint64_t* address, int* kind, const char* buf) {
    unsigned long long value = 0;
    if (std::sscanf(buf, "%d,%llx,%x", type, &value, kind) != 3) return false;
    *address = value;
    return true;
}
#include "mame-functions.inc"

int main() {
    constexpr u32 last = std::numeric_limits<int>::max();
    // Every point kind allows its last positive ID once, then refuses without mutation.
    for (int kind = 0; kind < 4; ++kind) {
        device_debug debug;
        auto& ids = debug.m_device.points;
        u32* counter[] = {&ids.m_bpindex, &ids.m_wpindex, &ids.m_rpindex, &ids.m_epindex};
        *counter[kind] = last;
        address_space space;
        auto add = [&] {
            switch (kind) {
            case 0: return debug.breakpoint_set(42);
            case 1: return debug.watchpoint_set(space, read_or_write::READ, 42, 1);
            case 2: return debug.registerpoint_set("pc==42", {});
            default: return debug.exceptionpoint_set(3, nullptr, {});
            }
        };
        assert(add() == int(last) && *counter[kind] == last + 1);
        const int flags = debug.flags;
        space.slot = 100; // Failure must precede watch-container growth as well.
        for (int repeat = 0; repeat < 3; ++repeat) {
            assert(add() == -1);
            assert(*counter[kind] == last + 1 && debug.flags == flags);
            assert(debug.m_bplist.size() == size_t(kind == 0));
            assert(debug.m_wplist.size() == size_t(kind == 1));
            assert(std::distance(debug.m_rplist.begin(), debug.m_rplist.end()) == (kind == 2));
            assert(debug.m_eplist.size() == size_t(kind == 3));
        }
        debug.m_bplist.clear(); debug.m_wplist.clear(); debug.m_rplist.clear(); debug.m_eplist.clear();
        assert(add() == -1 && *counter[kind] == last + 1);
        assert(debug.m_wplist.empty());
        *counter[kind] = 0; // Reserved/invalid allocator state must not issue an identity.
        assert(add() == -1 && *counter[kind] == 0);
    }
    // Exercise the actual native GDB caller, including failed watch translation/admission.
    for (int kind = 0; kind < 5; ++kind) {
        device_debug debug;
        device_memory_interface memory;
        address_space space;
        Console console{&debug.m_device};
        debug_gdbstub gdb{&memory, &space, &console, {{999, 888}}};
        char request[64];
        std::snprintf(request, sizeof(request), "%d,40,1", kind);
        assert(gdb.handle_Z(request) == debug_gdbstub::REPLY_OK);
        if (kind >= 2) assert(gdb.m_address_map.at(0x50) == 0x40);
        const auto prior_map = gdb.m_address_map;
        debug.m_device.points.m_bpindex = last + 1;
        debug.m_device.points.m_wpindex = last + 1;
        std::snprintf(request, sizeof(request), "%d,80,1", kind);
        assert(gdb.handle_Z(request) == debug_gdbstub::REPLY_ENN);
        assert(gdb.m_address_map == prior_map);
        assert(debug.m_bplist.size() == size_t(kind < 2));
        if (kind >= 2) assert(debug.m_wplist[0].size() == 1);
        assert(gdb.handle_Z("bad") == debug_gdbstub::REPLY_ENN);
        assert(gdb.handle_Z("9,40,1") == debug_gdbstub::REPLY_UNSUPPORTED);
    }
}
