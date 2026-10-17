-- Execute the production restore and dispatcher guards with a native host that
-- mutates before failing. No emulator or game image is needed for this boundary.
local dir = os.getenv("EMUCAP_ADAPTER_DIR") or "."
package.path = dir .. "/?.lua;" .. package.path
local file = assert(io.open(dir .. "/emucap-core.lua", "rb"))
local source = file:read("*a")
file:close()
local restore = assert(source:match("(function StateIo%.restore_guard%.quarantine%(%).-)\nlocal function on_io_exec"))
local function dispatcher(name)
  return assert(source:match("(local function " .. name .. "%(%w+%).-)\n  local selection_error")) .. "\nend\n"
end
local connect = assert(source:match("(local function connect%(%).-)\nlocal function send_line"))
local resume = assert(source:match("(resume_from_freeze = function%(%).-)\n%-%- A command%-line script"))
local chunk = restore .. "\n" .. dispatcher("dispatch") .. dispatcher("handle_in_freeze") .. connect .. "\n" .. resume
  .. "\nreturn StateIo.restore_guard.load, dispatch, handle_in_freeze, connect, resume_from_freeze"
local path = os.tmpname()
local state = assert(io.open(path, "wb")); state:write("state"); state:close()
for _, failure in ipairs({false, "throw"}) do
  local env = setmetatable({
    StateIo = require("emucap_state_io"), boundary_seq = 0,
    STATE = "running", deferred = {}, step_operation = {}, input_hold = {}, freeze_snapshot = {},
    session_epoch = 1, Rx = {reset=function() end}, Tx = {reset=function() end},
    socket = {tcp=function() return {settimeout=function() end, connect=function() end} end},
    FreezeState = {halt=function(reason) return {reason=reason} end},
    parse_request=function() return 7, "resume", {} end,
    reply_err=function(_, kind) assert(kind == "restore_unverified") end,
    emu = {resume=function() error("quarantined guest must not resume") end, breakExecution=function() end, loadSavestate=function()
      if failure == "throw" then error("partially mutated") end
      return false
    end},
  }, {__index=_G})
  env.StateIo.restore_guard = {unverified = false}
  local load_state, dispatch, frozen, reconnect, resume = assert(load(chunk, "production restore", "t", env))()
  assert(not pcall(load_state, path))
  assert(env.StateIo.restore_guard.unverified and env.STATE == "frozen")
  assert(env.deferred == nil and env.step_operation == nil and env.input_hold == nil)
  assert(env.freeze_snapshot == nil)
  assert(dispatch("resume") == nil and frozen("resume") == nil)
  resume()
  assert(env.STATE == "frozen")
  reconnect()
  assert(env.StateIo.restore_guard.unverified and env.session_epoch == 2)
  assert(dispatch("resume") == nil and frozen("resume") == nil)
end
-- Every native restore caller uses the same generation-bound failure callback.
local native_calls = 0
for _ in source:gmatch("StateIo%.load%(") do native_calls = native_calls + 1 end
assert(native_calls == 1)
assert(source:match("pcall%(StateIo%.restore_guard%.load, p%.initial_state%.path%)"))
assert(source:match("if ok then StateIo%.restore_guard%.quarantine%(%) end"))
os.remove(path)
print("ALL RESTORE QUARANTINE TESTS PASSED")
