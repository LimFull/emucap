// Actual dispatcher/remover methods; the Lua double owns references and calls C++ actions.
#include <algorithm>
#include <cassert>
#include <cstdint>
#include <functional>
#include <map>
#include <utility>
#include <vector>
using std::vector;
enum class CpuType { Main, Other };
enum class MemoryType { Relative, Absolute, Ppu };
enum class CallbackType { Read, Write, Exec };
enum class EventType { Frame, CodeBreakIdleSavestate, LastValue = CodeBreakIdleSavestate };
struct AddressInfo { int32_t Address; MemoryType Type; };
struct MemoryCallback {
    uint32_t StartAddress, EndAddress;
    CpuType Cpu;
    MemoryType MemType;
    int Reference;
};
struct lua_State {
    std::map<int, std::function<void()>> refs;
    int pending = 0, top = 0;
    bool fail = false;
};
constexpr int LUA_REGISTRYINDEX = 0, LUA_MULTRET = -1;
static void luaL_unref(lua_State* lua, int, int ref) { lua->refs.erase(ref); }
static void lua_setwatchdogtimer(lua_State*, void (*)(lua_State*), int) {}
static int lua_gettop(lua_State* lua) { return lua->top; }
static void lua_settop(lua_State* lua, int top) { lua->top = top; }
static void lua_rawgeti(lua_State* lua, int, int ref) {
    assert(lua->refs.contains(ref)); // retired references must never be dispatched
    lua->pending = ref;
    lua->top++;
}
static void lua_pushinteger(lua_State* lua, int) { lua->top++; }
static int lua_pcall(lua_State* lua, int nargs, int, int) {
    auto call = lua->refs.at(lua->pending);
    lua->top -= nargs + 1;
    call();
    return std::exchange(lua->fail, false) ? 1 : 0;
}
static bool lua_isinteger(lua_State*, int) { return false; }
static int lua_tointeger(lua_State*, int) { return 0; }
struct LuaCallHelper {
    explicit LuaCallHelper(lua_State*) {}
    int ReturnCount() { return 0; }
};
struct Timer { void Reset() {} };
struct ScriptManager { void EnablePpuMemoryCallbacks() {} void EnableCpuMemoryCallbacks() {} };
struct Debugger {
    ScriptManager manager;
    ScriptManager* GetScriptManager() { return &manager; }
    AddressInfo GetAbsoluteAddress(AddressInfo a) { a.Type = MemoryType::Absolute; return a; }
};
struct DebugUtilities {
    static bool IsPpuMemory(MemoryType t) { return t == MemoryType::Ppu; }
    static bool IsRelativeMemory(MemoryType t) { return t == MemoryType::Relative; }
};
struct LuaApi { static void SetContext(void*) {} };
class ScriptingContext {
public:
    lua_State state;
    lua_State* _lua = &state;
    Debugger debugger;
    Debugger* _debugger = &debugger;
    Timer _timer;
    bool _allowSaveState = false;
    int errors = 0;
    static ScriptingContext* _context;
    static void ExecutionCountHook(lua_State*) {}
    void ProcessLuaError() { errors++; }
#include "mesen-members.inc"
    vector<MemoryCallback> _callbacks[3];
    vector<int> _eventCallbacks[2];
    void RegisterMemoryCallback(CallbackType, int, int, MemoryType, CpuType, int);
    void UnregisterMemoryCallback(CallbackType, int, int, MemoryType, CpuType, int);
    void RegisterEventCallback(EventType, int);
    void UnregisterEventCallback(EventType, int);
    bool IsAddressMatch(MemoryCallback&, AddressInfo);
    template<typename T> void InternalCallMemoryCallback(AddressInfo, T&, CallbackType, CpuType);
    int CallEventCallback(EventType, CpuType);
    bool events = false;
    void add(int id, std::function<void()> f) {
        assert(!state.refs.contains(id));
        state.refs[id] = std::move(f);
        if(events) RegisterEventCallback(EventType::Frame, id);
        else RegisterMemoryCallback(CallbackType::Exec, 0, 65535, MemoryType::Relative, CpuType::Main, id);
    }
    void remove(int id) {
        if(events) UnregisterEventCallback(EventType::Frame, id);
        else UnregisterMemoryCallback(CallbackType::Exec, 0, 65535, MemoryType::Relative, CpuType::Main, id);
    }
    void dispatch() {
        if(events) CallEventCallback(EventType::Frame, CpuType::Main);
        else { uint8_t value = 4; InternalCallMemoryCallback({10, MemoryType::Relative}, value, CallbackType::Exec, CpuType::Main); }
    }
    void expect_empty() {
        assert(state.refs.empty() && _callbacks[2].empty() && _eventCallbacks[0].empty());
    }
};
ScriptingContext* ScriptingContext::_context = nullptr;
#include "mesen-functions.inc"

int main() {
    for(bool events : {false, true}) {
        { // All callbacks expire on the same instruction/event; no skip or stale call.
            ScriptingContext c; c.events = events; vector<int> calls;
            for(int id = 1; id <= 128; id++) c.add(id, [&, id] { calls.push_back(id); c.remove(id); });
            c.dispatch();
            assert(calls.size() == 128);
            for(int id = 1; id <= 128; id++) assert(calls[id - 1] == id);
            c.expect_empty();
        }
        { // Removing the next callback and reusing its Lua reference must not call the replacement.
            ScriptingContext c; c.events = events; vector<int> calls;
            c.add(1, [&] { calls.push_back(1); c.remove(1); c.remove(2); c.add(2, [&] { calls.push_back(20); }); });
            c.add(2, [&] { assert(false); });
            c.add(3, [&] { calls.push_back(3); });
            c.dispatch(); assert((calls == vector<int>{1, 3}));
            calls.clear(); c.dispatch(); assert((calls == vector<int>{3, 20}));
        }
        { // Reallocation during a dispatch keeps existing callbacks and defers additions.
            ScriptingContext c; c.events = events; vector<int> calls;
            c.add(1, [&] {
                calls.push_back(1); c.remove(1);
                for(int id = 3; id <= 128; id++) c.add(id, [&, id] { calls.push_back(id); });
            });
            c.add(2, [&] { calls.push_back(2); });
            c.dispatch(); assert((calls == vector<int>{1, 2}));
            calls.clear(); c.dispatch(); assert(calls.size() == 127);
        }
        { // Inner removals repair the outer cursor as well; Lua errors still retire cursors.
            ScriptingContext c; c.events = events; vector<int> calls;
            c.add(1, [&] { calls.push_back(1); c.remove(1); c.dispatch(); c.state.fail = true; });
            c.add(2, [&] { calls.push_back(2); c.remove(2); });
            c.add(3, [&] { calls.push_back(3); c.remove(3); });
            c.dispatch(); assert((calls == vector<int>{1, 2, 3})); assert(c.errors == 1);
            c.expect_empty(); c.add(4, [&] { calls.push_back(4); c.remove(4); });
            c.dispatch(); assert(calls.back() == 4); c.expect_empty();
        }
        { // Many replacements in one callback release storage immediately, including removed tail slots.
            ScriptingContext c; c.events = events; vector<int> calls;
            c.add(1, [&] {
                calls.push_back(1); c.remove(1);
                for(int n = 0; n < 4096; n++) {
                    c.add(1, [&] { calls.push_back(10); }); c.remove(1);
                    assert(c._callbacks[2].size() + c._eventCallbacks[0].size() == 1);
                }
                c.add(1, [&] { calls.push_back(10); });
            });
            c.add(2, [&] { calls.push_back(2); });
            c.dispatch(); assert((calls == vector<int>{1, 2}));
            calls.clear(); c.dispatch(); assert((calls == vector<int>{2, 10}));
        }
    }
}
