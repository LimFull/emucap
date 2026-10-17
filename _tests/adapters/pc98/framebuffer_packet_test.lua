local root = arg[1] or "."
package.path = root .. "/adapters/mame-pc98/plugins/?.lua;" .. package.path
local fixture = dofile(root .. "/_tests/adapters/pc98/control_io_fixture.lua")
local periodic, replies
local screen = { width = 2, height = 1, frame_number = 7 }
local reads, restores, outcome = 0, 0, "12345678"
function screen:restore_presented_pixels(data, width, height)
  restores = restores + 1
  assert(data == "12345678" and width == 2 and height == 1)
  return true
end
manager = { machine = { screens = { at = function() return screen end }, video = {} } }
emu = { register_periodic = function(f) periodic = f end }
for _, name in ipairs({"reset", "stop", "pre_save", "post_load", "frame"}) do
  emu["add_machine_" .. name .. "_notifier"] = function(f) return f end
end
fixture.install(emu, {
  read = function() return "" end,
  write = function(_, value) replies = (replies or "") .. value; return #value end,
})
emu.emucap_read_framebuffer = function(path, width, height)
  reads = reads + 1
  assert(path == "frame.bin" and width == 2 and height == 1)
  if outcome == "failure" then error("native admission failed") end
  return outcome
end
dofile(root .. "/adapters/mame-pc98/plugins/emucap_gdbstub/init.lua").startplugin()
local handle = fixture.upvalue(fixture.upvalue(periodic, "service_frozen_socket"), "handle")
for i = 1, 100 do
  local name = debug.getupvalue(handle, i)
  if name == "cpu" then debug.setupvalue(handle, i, { shortname = "i386" }) end
  if not name then break end
end
local function load()
  replies = ""
  fixture.call(handle, "qEmucap,loadpixels,6672616d652e62696e")
  return replies
end
for _, geometry in ipairs({{0,1}, {1,0}, {-1,1}, {1.5,1}, {math.huge,1},
    {1,math.huge}, {4194305,1}, {4194304,2}, {0/0,1}}) do
  screen.width, screen.height = geometry[1], geometry[2]
  assert(load():find("$E19#", 1, true))
  assert(reads == 0 and restores == 0, "invalid geometry reached file or screen")
end
screen.width, screen.height = 2, 1
for _, result in ipairs({"failure", "", "1234567", "123456789"}) do
  outcome = result
  assert(load():find("$E19#", 1, true) and restores == 0)
end
outcome = "12345678"
assert(load():find("$OK|2|1|8#", 1, true) and restores == 1 and reads == 5)
emu.emucap_control_socket = nil
local ok, err = pcall(function()
  dofile(root .. "/adapters/mame-pc98/plugins/emucap_gdbstub/init.lua").startplugin()
end)
assert(not ok and tostring(err):find("rebuild the maintained MAME adapter", 1, true))
print("MAME framebuffer: pre-open geometry, native error/size rejection, exact restore and missing-host refusal passed")
