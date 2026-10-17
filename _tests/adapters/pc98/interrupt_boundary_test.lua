-- Production interrupt/retirement callbacks with real bounded transport and native doubles.
local root = arg[1] or "."
package.path = root .. "/adapters/mame-pc98/plugins/?.lua;" .. package.path
assert(os.getenv("EMUCAP_MAME_PROFILE") == "pc98")
local fixture = dofile(root .. "/_tests/adapters/pc98/control_io_fixture.lua")
local function set(f, name, value)
  for i = 1, 100 do
    local key = debug.getupvalue(f, i)
    if key == name then debug.setupvalue(f, i, value); return end
    if not key then break end
  end
  error("missing callback capture: " .. name)
end
local function setup()
  local t = { callbacks = {}, input = {}, writes = {}, pauses = 0, resumes = 0, goes = 0 }
  t.debugger = { execution_state = "run" }
  t.cpu = { shortname = "i386", debug = {} }
  function t.cpu.debug:go() t.goes = t.goes + 1; t.debugger.execution_state = "run" end
  t.screen = { frame_number = 7 }
  t.machine = { paused = false, screens = { at = function() return t.screen end }, video = {} }
  manager = { machine = t.machine }
  local native = {
    write = function(_, value)
      if t.write_failure then error("stop reply transport failed") end
      if value:match("^%$S05#") then assert(t.machine.paused, "stop published before pause") end
      t.writes[#t.writes + 1] = value
      return #value
    end,
    read = function()
      if t.read_failure then error("native read failed") end
      return table.remove(t.input, 1) or ""
    end,
  }
  emu = {
    pause = function() if not t.pause_failure then t.machine.paused = true; t.pauses = t.pauses + 1 end end,
    unpause = function() t.machine.paused = false; t.resumes = t.resumes + 1 end,
    register_periodic = function(f) t.callbacks.periodic = f end,
  }
  for _, name in ipairs({"reset", "stop", "pre_save", "post_load", "frame"}) do
    emu["add_machine_" .. name .. "_notifier"] = function(f) t.callbacks[name] = f; return f end
  end
  fixture.install(emu, native)
  dofile(root .. "/adapters/mame-pc98/plugins/emucap_gdbstub/init.lua").startplugin()
  set(t.callbacks.periodic, "cpu", t.cpu)
  set(t.callbacks.periodic, "debugger", t.debugger)
  t.service = fixture.upvalue(t.callbacks.periodic, "service_frozen_socket")
  t.handle = fixture.upvalue(t.service, "handle")
  t.transport = fixture.upvalue(t.handle, "socket")
  t.throttle = fixture.upvalue(t.callbacks.reset, "throttle_wait_hook")
  return t
end

local t = setup()
for name, value in pairs({frame_wait_target=120, frame_wait_requested=120, frame_wait_stop=true, frame_wait_screen_start=7}) do
  set(t.callbacks.frame, name, value)
end
t.input = {fixture.wire("c")}
fixture.call(t.handle, "\3")
assert(t.pauses == 1 and t.resumes == 1 and t.goes == 1)
assert(fixture.upvalue(t.callbacks.frame, "frame_wait_target") == nil)
local count = #t.writes
t.callbacks.frame()
assert(#t.writes == count, "cancelled wait emitted a later completion")

t = setup()
t.pause_failure = true
local ok, err = pcall(t.handle, "\3")
assert(not ok and tostring(err):find("native pause was not established", 1, true))
assert(#t.writes == 0)
fixture.call(t.handle, "c")
assert(t.goes == 0 and t.writes[2]:find("STATE:load_failed_unverified", 1, true))

t = setup()
t.write_failure = true
ok, err = pcall(t.handle, "\3")
assert(not ok and tostring(err):find("stop reply transport failed", 1, true))
assert(t.transport.terminal and t.machine.paused and t.resumes == 0)
assert(not fixture.upvalue(t.service, "in_frozen_socket_service"))

for _, release_fails in ipairs({false, true}) do
  t = setup()
  local commands = fixture.upvalue(t.handle, "handle_emucap")
  local set_inputs = fixture.upvalue(commands, "set_inputs")
  local clear_inputs = fixture.upvalue(set_inputs, "clear_inputs")
  local releases = 0
  local field = {set_value=function() end, clear_value=function()
    releases = releases + 1
    if release_fails then error("native release failed") end
  end}
  set(set_inputs, "input_fields", {enter=field})
  assert(set_inputs({"enter"}))
  set(t.handle, "frame_owner", {id="lost", start_frame=7, requested=120, completed=0})
  set(t.callbacks.frame, "frame_wait_target", 120)
  t.screen.frame_number = 9
  t.machine.paused = true
  t.read_failure = true
  t.callbacks.periodic()
  assert(t.transport.terminal and t.machine.paused and t.resumes == 0 and t.goes == 0)
  assert(releases >= 1 and not fixture.upvalue(t.service, "in_frozen_socket_service"))
  local owner = fixture.upvalue(t.handle, "frame_owner")
  assert(owner.completed == 2 and owner.result == "interrupted" and owner.reason == "transport_lost")
  assert((fixture.upvalue(clear_inputs, "active_input_fields")[field] ~= nil) == release_fails)
  count = #t.writes
  t.handle("c")
  t.callbacks.frame()
  t.callbacks.periodic()
  assert(t.goes == 0 and #t.writes == count, "terminal transport accepted work or emitted a stale reply")
end
t = setup()
t.read_failure = true
t.handle("\3")
assert(t.transport.terminal and t.machine.paused and t.resumes == 0)
assert(not fixture.upvalue(t.service, "in_frozen_socket_service"))

for _, failure in ipairs({"write", "read", "handler_reply"}) do
  t = setup()
  t.input = {fixture.wire("qEmucap,frame")}
  if failure == "read" then t.read_failure = true else t.write_failure = true end
  if failure == "handler_reply" then
    local safe_handle = fixture.upvalue(t.service, "handle_payload_safely")
    set(safe_handle, "injected_failure_request", "frame")
  end
  assert(t.throttle(), "terminal transport must end the native throttle wait: " .. failure)
  assert(t.transport.terminal and t.machine.paused and t.resumes == 0)
  assert(t.throttle(), "retired transport must keep ending any remaining throttle wait")
end
t = setup()
t.input = {fixture.wire("qEmucap,frame")}
assert(not t.throttle(), "a normal observation must preserve the pacing wait")
assert(not t.transport.terminal and not t.machine.paused)
print("PC-98 interrupt, terminal transport, owned progress and input cleanup checks passed")
