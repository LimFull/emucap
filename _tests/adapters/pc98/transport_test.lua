local root = arg[1] or "."
local Transport = dofile(root .. "/adapters/mame-pc98/plugins/emucap_gdbstub/transport.lua")
local Fixture = dofile(root .. "/_tests/adapters/pc98/control_io_fixture.lua")
local wire = Fixture.wire
local function setup()
  local f = { input = {}, output = {}, clock = 0, reads = 0, writes = 0, bytes = 0, closes = 0 }
  local native = {}
  function native:read()
    f.reads = f.reads + 1
    if f.read_failure then return "closed", "" end
    local data = table.remove(f.input, 1)
    return data and "data" or "idle", data or ""
  end
  function native:write(data)
    f.writes = f.writes + 1
    if f.write_failure then return "error", 0 end
    if f.stalled then return "idle", 0 end
    local count = math.min(#data, f.partial or #data)
    f.output[#f.output + 1] = data:sub(1, count)
    f.bytes = f.bytes + count
    return "data", count
  end
  function native:close() f.closes = f.closes + 1 end
  f.transport = Transport.new(native, function() return f.clock end, function(reason) f.reason = reason end)
  function f:feed(data, chunk)
    for i = 1, #data, chunk or 4096 do self.input[#self.input + 1] = data:sub(i, i + (chunk or 4096) - 1) end
  end
  function f:pump()
    local reads, writes, bytes = self.reads, self.writes, self.bytes
    local active = self.transport:pump()
    assert(self.reads - reads <= 8 and self.writes - writes <= 8 and self.bytes - bytes <= 32768)
    assert(#self.transport.rx + self.transport.size + self.transport.queued <= 528388)
    assert(self.transport.tx_bytes <= 1048586)
    return active
  end
  return f
end

for _, payload in ipairs({"", "qEmucap,frame", "#$}*\3", string.rep("x", 262144), string.rep("#", 262144)}) do
  local f = setup()
  f:feed(wire(payload), #payload < 100 and 1 or 4096)
  for _ = 1, 1000 do
    assert(f:pump())
    if f.transport:peek() then break end
  end
  assert(f.transport:take() == payload)
  assert(not f.transport:take())
end

for _, input in ipairs({"$m0,1#00", "$m0,1#xz", "$nested$frame#00", wire(string.rep("x", 262145))}) do
  local f = setup()
  f:feed(input)
  for _ = 1, 1000 do if not f:pump() then break end end
  assert(f.transport.terminal and not f.transport:take() and f.closes == 1)
end

local f = setup()
f:feed("$}")
assert(f:pump())
f.clock = 5000
assert(not f:pump() and f.reason:find("deadline"))
f = setup()
f:feed("$a")
assert(f:pump())
for tick = 1, 4 do
  f.clock = tick * 1000; f:feed("a"); assert(f:pump())
end
f.clock = 5000
f:feed("#e5") -- final completion cannot revive a frame at its deadline
assert(not f:pump() and not f.transport:take())

f = setup()
f:feed(wire("?") .. "\3")
assert(f:pump() and f.transport:take() == "\3" and f.transport:take() == "?")
f = setup()
f:feed("$aborted\3" .. wire("g"))
assert(f:pump() and f.transport:take() == "\3" and f.transport:take() == "g")

-- A continuously readable peer cannot make one pump or retained request queue grow without bound.
f = setup()
f:feed(string.rep(wire("?"), 10000))
for _ = 1, 625 do
  assert(f:pump() and #f.transport.queue <= 16)
  for _ = 1, 16 do assert(f.transport:take() == "?") end
end
assert(not f.transport:take())

f = setup()
assert(f.transport:send("#$}*\3", true))
while #f.transport.tx > 0 do assert(f:pump()) end
assert(table.concat(f.output) == "+" .. wire("#$}*\3"))
f = setup()
f.partial = 1
assert(f.transport:send(string.rep("x", 100), true))
for tick = 0, 4 do f.clock = tick * 1000; assert(f:pump()) end
assert(#f.transport.tx > 0)
f.clock = 5000
assert(not f:pump() and f.reason:find("transmit deadline"))
local sent = f.bytes
assert(not f.transport:send("E1B", true) and f.bytes == sent and f.closes == 1)

f = setup()
f.stalled = true
assert(f.transport:send("ok", true) and f:pump())
f.clock = 5000
assert(not f:pump() and f.bytes == 0)
f = setup()
assert(f.transport:send(string.rep("#", 262144), true))
assert(f.transport:send(string.rep("#", 262144), true))
assert(not f.transport:send("x", true) and f.transport.terminal)
f = setup()
assert(not f.transport:send(string.rep("x", 262145), false) and f.bytes == 0)
f = setup()
f.read_failure = true
assert(not f:pump() and f.closes == 1)
assert(not f:pump() and f.closes == 1)
print("MAME RSP: bounded RX/TX/pumps, checksums, escaping, interruption, deadlines, terminal failure passed")
