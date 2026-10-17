local original_getenv, original_open = os.getenv, io.open
local env, bytes, reads, closes = {}, "", 0, 0
os.getenv = function(key) return env[key] end
io.open = function()
  return {
    read = function(_, n) reads = reads + 1; assert(n == 1025); return bytes:sub(1, n) end,
    close = function() closes = closes + 1; return true end,
  }
end
local function fresh() return dofile("adapters/mesen2/emucap_auth.lua") end
assert(fresh().session_token() == nil)
env = {EMUCAP_SESSION_TOKEN = "override", EMUCAP_SESSION_TOKEN_FILE = "private"}
assert(fresh().session_token() == "override" and reads == 0)
env = {EMUCAP_SESSION_TOKEN_FILE = "private"}
bytes = "launch-token"
local auth = fresh()
assert(auth.session_token() == bytes)
bytes = "replacement"
assert(auth.session_token() == "launch-token" and reads == 1)
for _, value in ipairs({"", string.rep("x", 1025)}) do
  bytes = value
  assert(not pcall(fresh().session_token))
end
bytes = string.rep("x", 1024)
assert(fresh().session_token() == bytes)
assert(reads == closes)
io.open = function() return nil end
assert(not pcall(fresh().session_token))
os.getenv, io.open = original_getenv, original_open
print("Mesen credential admission tests passed")
