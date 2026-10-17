-- Exercise memory admission through the production dispatcher on both shared profiles.
local root = arg[1] or "."
package.path = root .. "/adapters/mame-pc98/plugins/?.lua;" .. package.path
local profile = os.getenv("EMUCAP_MAME_PROFILE") or "pc98"
local callbacks, replies = {}, {}
local reads, writes, native_reads = 0, 0, 0
local first_write, last_write
local space = {}
function space:readv_u8(address)
  reads = reads + 1
  assert(reads <= 0x10000, "unbounded fallback read")
  return address % 256
end
function space:writev_u8(address, byte)
  writes = writes + 1
  assert(writes <= 0x10000, "unbounded write")
  local item = { address, byte }
  first_write = first_write or item
  last_write = item
end
local function native_read(_, address, length)
  native_reads = native_reads + 1
  assert(length >= 1 and length <= 0x10000, "unbounded native read")
  return string.rep(string.char(address % 256), length)
end
local cpu = { shortname = profile == "pc98" and "i386" or "m68000", spaces = { program = space } }
local debugger = { execution_state = "stop" }
manager = { machine = { screens = { at = function() return { frame_number = 7 } end }, video = {} } }
local socket = { open = function() end, read = function() return "" end,
  write = function(_, value) replies[#replies + 1] = value; return #value end }
emu = { file = function() return socket end,
  register_periodic = function(f) callbacks.periodic = f end }
for _, name in ipairs({ "reset", "stop", "pre_save", "post_load", "frame" }) do
  emu["add_machine_" .. name .. "_notifier"] = function(f) return f end
end
local fixture = dofile(root .. "/_tests/adapters/pc98/control_io_fixture.lua")
fixture.install(emu, socket)
dofile(root .. "/adapters/mame-pc98/plugins/emucap_gdbstub/init.lua").startplugin()
local function upvalue(f, name, replacement)
  for i = 1, 100 do
    local key, value = debug.getupvalue(f, i)
    if key == name then
      if replacement then debug.setupvalue(f, i, replacement) end
      return value
    end
    if not key then break end
  end
  error("missing callback capture: " .. name)
end
upvalue(callbacks.periodic, "cpu", cpu)
upvalue(callbacks.periodic, "debugger", debugger)
local handle = upvalue(upvalue(callbacks.periodic, "service_frozen_socket"), "handle")
local function request(payload)
  replies = {}
  reads, writes, native_reads = 0, 0, 0
  first_write, last_write = nil, nil
  fixture.call(handle, payload)
  return assert(replies[#replies]:match("^%$(.*)#%x%x$"), "missing response")
end

local rejected = {
  "m0,10001", "m0,0", "mffffffff,2", "m100000000,1", "m0,10000000000000001",
  "m10000000000000000,1", "m000000000,1", "m0,1junk", "m0,1\n", "m-1,1",
  "M0,1:", "M0,0:", "M0,1:a", "M0,2:aa", "M0,1:aabb", "M0,1:aagz",
  "M0,1:aa\n", "Mffffffff,2:aabb", "M100000000,1:aa", "M10000000000000000,1:aa",
  "M0,10000000000000001:aa", "M0,10001:" .. string.rep("aa", 0x10001),
}
for _, native in ipairs({ false, true }) do
  space.read_peek_block = native and native_read or nil
  for index, payload in ipairs(rejected) do
    local response = request(payload)
    assert(response == "E00", "invalid packet accepted at case " .. index)
    assert(reads == 0 and writes == 0 and native_reads == 0, "invalid packet touched guest")
  end
  assert(#request("m0,10000") == 0x20000)
  assert(reads == (native and 0 or 0x10000) and native_reads == (native and 1 or 0))
  assert(request("mFFFFFFFF,1") == "ff")
  assert(reads == (native and 0 or 1) and native_reads == (native and 1 or 0))
  if native then
    space.read_peek_block = function() native_reads = native_reads + 1; return nil end
    assert(request("m0,1") == "E1C" and native_reads == 1 and reads == 0)
  end
end
assert(request("Mffff0000,10000:" .. string.rep("ab", 0x10000)) == "OK")
assert(writes == 0x10000 and first_write[1] == 0xffff0000 and last_write[1] == 0xffffffff)
assert(first_write[2] == 0xab and last_write[2] == 0xab)
assert(request("MFFFFFFFF,1:AB") == "OK" and writes == 1 and last_write[1] == 0xffffffff)
assert(reads == 0 and native_reads == 0)
print(profile .. ": bounded memory admission, no invalid side effects, native/fallback controls passed")
