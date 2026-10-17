-- Production savestate file helper tests.
-- Run: EMUCAP_ADAPTER_DIR=adapters/mesen2 lua adapters/mesen2/emucap_state_io_test.lua

local dir = os.getenv("EMUCAP_ADAPTER_DIR") or "."
package.path = dir .. "/?.lua;" .. package.path
local StateIo = require("emucap_state_io")

local base = os.tmpname()
os.remove(base)
local path = base .. ".mss"

local function write(pathname, data)
  local file = assert(io.open(pathname, "wb"))
  assert(file:write(data))
  assert(file:close())
end

local function read(pathname)
  local file = assert(io.open(pathname, "rb"))
  local data = assert(file:read("*a"))
  assert(file:close())
  return data
end

write(path, "old")
local saved_bytes = StateIo.save({
  createSavestate = function() return "new-state" end,
}, path, 1)
assert(saved_bytes == 9, "save must report serialized byte length")
assert(read(path) == "new-state", "save must replace the destination")

write(path, "keep-me")
local ok = pcall(StateIo.save, {
  createSavestate = function() error("serializer failed") end,
}, path, 2)
assert(not ok, "serializer failure must be reported")
assert(read(path) == "keep-me", "serializer failure must preserve the previous file")

local loaded_data
local loaded_bytes = StateIo.load({
  loadSavestate = function(data)
    loaded_data = data
    return true
  end,
}, path)
assert(loaded_bytes == 7 and loaded_data == "keep-me", "load must pass exact file bytes to Mesen")

ok = pcall(StateIo.load, {
  loadSavestate = function() return false end,
}, path)
assert(not ok, "a rejected savestate must be reported")

os.remove(path)

write(path, "state")
for _, fail in ipairs({
  function() return false end,
  function() error("partial native mutation") end,
}) do
  local quarantined = false
  local ok = pcall(StateIo.load, { loadSavestate = fail }, path, function() quarantined = true end)
  assert(not ok and quarantined, "native failures must notify the generation owner")
end
os.remove(path)
local quarantined = false
assert(not pcall(StateIo.load, { loadSavestate = function() error("must not run") end }, path,
  function() quarantined = true end))
assert(not quarantined, "file admission failure must not quarantine untouched native state")

print("ALL EMUCAP STATE I/O TESTS PASSED")

-- Fake handles prove admission happens before allocation/native mutation and reads
-- are bounded even if the file grows after its length was sampled.
local original_open = io.open
for _, size in ipairs({0, 67108865, 1099511627776}) do
  local closed = false
  io.open = function() return {
    seek = function() return size end,
    read = function() error("oversized state must not be read") end,
    close = function() closed = true; return true end,
  } end
  local entered, quarantined = false, false
  assert(not pcall(StateIo.load, {loadSavestate=function() entered=true end}, "fake",
    function() quarantined=true end))
  assert(closed and not entered and not quarantined)
end
for _, data in ipairs({"ab", "abcd", "abc"}) do
  local entered, closed = false, false
  io.open = function() return {
    seek = function(_, where) return where == "end" and 3 or 0 end,
    read = function(_, n) assert(n == 4); return data end,
    close = function() closed=true; return true end,
  } end
  local ok = pcall(StateIo.load, {loadSavestate=function(actual) entered=true; assert(actual==data) end}, "fake")
  assert(ok == (#data == 3) and entered == ok and closed)
end
io.open = original_open
-- A sparse file exercises the real boundary without writing a 64 MiB fixture.
local f = assert(io.open(path, "wb"))
assert(f:seek("set", 67108863)); assert(f:write("x")); assert(f:close())
assert(StateIo.load({loadSavestate=function(data) assert(#data == 67108864) end}, path) == 67108864)
os.remove(path)
