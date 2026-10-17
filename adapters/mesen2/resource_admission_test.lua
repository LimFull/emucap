-- Execute production admission helpers against oversized input and legitimate controls.
local dir = os.getenv("EMUCAP_ADAPTER_DIR") or "adapters/mesen2"
local function source(name)
  local f = assert(io.open(dir .. "/" .. name, "rb"))
  local s = assert(f:read("*a")); assert(f:close()); return s
end
local core = source("emucap-core.lua")
local reads = 0
local env = setmetatable({
  handlers = {}, READ_CAP = 131072, SCAN_CAP = 131072,
  SYS = {default_memtype = "ram", address_space_size = 262144, region_sizes = {ram = 262144}},
  REGION_SIZE = {ram = 262144},
  emu = {memType = {ram = 1}, read = function() reads = reads + 1; return 0 end},
  Memory = dofile(dir .. "/emucap_memory.lua"), as_array = function(v) return v end,
}, {__index = _G})
local parser = assert(core:match("(local function snum%(%w+%).-)\nlocal function capture_snapshot_specs"))
local parse = assert(load(parser .. "\nreturn parse_snapshot_specs", "snapshot admission", "t", env))()
assert(parse({"ram:0:131072"}))
assert(not parse({"ram:0:131072", "ram:0:1"}))
local specs = {}
for i = 1, 64 do specs[i] = "ram:0:1" end
assert(parse(specs)); specs[65] = "ram:0:1"; assert(not parse(specs))
assert(not parse({"ram:0:131073"}))
local pattern = assert(core:match("(function handlers%.find_pattern%(p%).-)\n%-%- ── 디스어셈블러"))
assert(load(pattern, "pattern admission", "t", env))()
assert(not env.handlers.find_pattern({memory_type="ram", hex=string.rep("00", 131073)}))
assert(reads == 0, "oversized pattern touched memory")
local ok, result = env.handlers.find_pattern({memory_type="ram", hex=string.rep("00", 131072), length=131072})
assert(ok and result.count == 1)
ok, result = env.handlers.find_pattern({memory_type="ram", hex="0000", length=5, align=2})
assert(ok and result.count == 2 and result.matches[1] == 0 and result.matches[2] == 2)

local recording = source("emucap_recording.lua")
local movie_source = assert(recording:match("(local function read_movie%(params, frames%).-)\nfunction M%.validate"))
local movie_env = setmetatable({
  INPUT_MOVIE_FORMAT = "frame-full-state-1", MAX_MOVIE_BYTES = 1048576, MAX_BUTTONS_PER_FRAME = 64,
  integer = function(v) return type(v) == "number" and v == math.floor(v) end,
  digest = function(v) return v == string.rep("a", 64) end,
}, {__index=_G})
local movie = assert(load(movie_source .. "\nreturn read_movie", "movie admission", "t", movie_env))()
local text, closed, count = "0:\n", false, nil
movie_env.io = {open = function()
  return {read = function(_, n) assert(type(n) == "number"); count = n; return text:sub(1,n) end,
    close = function() closed = true; return true end}
end}
local params = {format="frame-full-state-1", port=0, frames=1, bytes=3, sha256=string.rep("a",64), path="fixture"}
assert(movie(params,1) and closed and count == 4)
for _, invalid in ipairs({"0:\nx", "0:", string.rep("x", 10000)}) do
  text, closed = invalid, false
  assert(not movie(params,1) and closed and count == 4)
end
print("Mesen snapshot, pattern and movie admission passed")
