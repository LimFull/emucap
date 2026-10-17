-- Exercise the registered production callbacks with a controlled scheduler/socket.
-- A stop must publish after scheduler unwind; only the pause owner may resume.
local root = arg[1] or "."
package.path = root .. "/adapters/mame-pc98/plugins/?.lua;" .. package.path
assert(not os.getenv("EMUCAP_MAME_PROFILE"), "run with default PC-98 profile")
local callbacks, writes, input = {}, {}, {}
local paused, resumed, goes, native_calls = 0, 0, 0, 0
local debugger = { execution_state = "run" }
local cpu = { shortname = "i386", debug = {} }
function cpu.debug:go() goes = goes + 1; debugger.execution_state = "run" end
local machine = { paused = false, state_io_boundary = false,
  screens = { at = function() return { frame_number = 7, raster_state_supported = true } end }, video = {} }
local outcome = "completed"
function machine:state_file_io(load, path)
  native_calls = native_calls + 1
  assert(path == "test.sta")
  return outcome
end
manager = { machine = machine }
local socket = {
  open = function() end,
  write = function(_, value) writes[#writes + 1] = value; return #value end,
  read = function() return table.remove(input, 1) or "" end,
}
emu = { file = function() return socket end,
  pause = function() machine.paused = true; paused = paused + 1 end,
  unpause = function() machine.paused = false; resumed = resumed + 1 end,
  register_periodic = function(f) callbacks.periodic = f end,
}
for _, name in ipairs({"reset", "stop", "pre_save", "post_load", "frame"}) do
  emu["add_machine_" .. name .. "_notifier"] = function(f) callbacks[name] = f; return f end
end
local fixture = dofile(root .. "/_tests/adapters/pc98/control_io_fixture.lua")
fixture.install(emu, socket)
dofile(root .. "/adapters/mame-pc98/plugins/emucap_gdbstub/init.lua").startplugin()
local function upvalue(f, name, value, write)
  for i = 1, 100 do
    local found, old = debug.getupvalue(f, i)
    if found == name then
      if write then debug.setupvalue(f, i, value) end
      return old
    end
    if not found then break end
  end
  error("missing callback capture: " .. name)
end
local function set(f, name, value) upvalue(f, name, value, true) end
set(callbacks.periodic, "cpu", cpu)
set(callbacks.periodic, "debugger", debugger)
local service = upvalue(callbacks.periodic, "service_frozen_socket")
local handle = upvalue(service, "handle")

local register_writes = 0
local names = {"EAX", "ECX", "EDX", "EBX", "ESP", "EBP", "ESI", "EDI",
  "EIP", "EFLAGS", "CS", "SS", "DS", "ES", "FS", "GS"}
cpu.state = {}
for _, name in ipairs(names) do
  local value = 0
  cpu.state[name] = setmetatable({}, {
    __index = function(_, key) assert(key == "value"); return value end,
    __newindex = function(_, key, v) assert(key == "value"); value = v; register_writes = register_writes + 1 end,
  })
end
set(callbacks.periodic, "service_frozen_socket", function() end)
local function hex(s) return (s:gsub(".", function(c) return string.format("%02x", c:byte()) end)) end
local valid = string.rep("aabbccdd", 16)
for _, method in ipairs({"regload", "regprobe", "G"}) do
  for _, packet in ipairs({"", "00", valid:sub(1, 120), valid:sub(1, 127),
      valid .. "0", valid .. "00000000", string.rep("z", 128), string.rep("가", 43)}) do
    register_writes = 0
    writes = {}
    local spec = method == "regprobe" and (packet .. "|1|0|1") or packet
    fixture.call(handle, method == "G" and ("G" .. packet) or ("qEmucap," .. method .. "," .. hex(spec)))
    assert(register_writes == 0 and goes == 0, "invalid packet changed CPU or executed")
    assert(writes[#writes]:match("^%$E"), "invalid packet reported success")
  end
end
for _, range in ipairs({"10000000000000000|1", "0|10000000000000001", "ffffffff|2", "0|4001"}) do
  register_writes = 0
  writes = {}
  fixture.call(handle, "qEmucap,regprobe," .. hex(valid .. "|1|" .. range))
  assert(register_writes == 0 and goes == 0, "invalid probe range changed CPU or executed")
  assert(writes[#writes]:match("^%$E00#"), "invalid probe range reported success")
end
for _, packet in ipairs({valid, valid:upper()}) do
  register_writes = 0
  writes = {}
  fixture.call(handle, "qEmucap,regload," .. hex(packet))
  assert(register_writes == 16 and writes[#writes]:match("^%$OK|"))
end
print("PC-98 register admission and full-packet restore passed")

-- Backend normalization is a failed restore, before regprobe may run any frames.
cpu.state.EAX = setmetatable({}, {
  __index = function() return 0 end,
  __newindex = function() register_writes = register_writes + 1 end,
})
writes = {}
fixture.call(handle, "qEmucap,regprobe," .. hex(valid .. "|1|0|1"))
assert(writes[#writes]:match("^%$E14#") and goes == 0)
fixture.call(handle, "c")
assert(goes == 0, "failed readback allowed resume")
