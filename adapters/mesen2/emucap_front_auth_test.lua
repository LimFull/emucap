local source = debug.getinfo(1, "S").source:sub(2)
local dir = source:match("^(.*[/\\])") or ""
package.path = dir .. "?.lua;" .. package.path

local Auth = require("emucap_front_auth")

local function hex(bytes)
  return (bytes:gsub(".", function(byte) return string.format("%02x", byte:byte()) end))
end

assert(hex(Auth._sha256("abc")) ==
  "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
assert(hex(Auth._hmac("key", {"a", "bc"})) ==
  "ba41d3aaace0b4bb31fb49a559ae1df182e1fd8add9b6b3b5603e6cb342dce22")

local key = "front-direct-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
local launch_id = "launch-01arz3ndektsv4rrffq69g5fav"
local challenge = "ECFA" .. string.char(1, 1, 1) .. string.pack(">I2", 47800)
  .. string.char(9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24)
local fake = { inbound = challenge, outbound = "" }
function fake:receive(size)
  if #self.inbound == 0 then return nil, "timeout", "" end
  local chunk = self.inbound:sub(1, size)
  self.inbound = self.inbound:sub(#chunk + 1)
  if #chunk == size then return chunk end
  return nil, "timeout", chunk
end
function fake:send(bytes, first)
  self.outbound = self.outbound .. bytes:sub(first)
  return #bytes
end

local state = assert(Auth.new(key, launch_id, 47800, 10))
assert(Auth.poll(state, fake, 10.1) == "pending")
assert(Auth.poll(state, fake, 10.2) == "pending")
assert(#fake.outbound == 88 and fake.outbound:sub(1, 7) == "ECFA" .. string.char(1, 2, 1))
local transcript = challenge .. fake.outbound:sub(8, 56)
fake.inbound = "ECFA" .. string.char(1, 3, 1)
  .. Auth._hmac(key, {"emucap-producer-front-auth-v1", "listener", transcript})
assert(Auth.poll(state, fake, 10.3) == "pending")
assert(Auth.poll(state, fake, 10.4) == "pending")
local commit = fake.outbound:sub(89)
assert(commit == "ECFA" .. string.char(1, 4, 1)
  .. Auth._hmac(key, {"emucap-producer-front-auth-v1", "commit", transcript}))
fake.inbound = "ECFA" .. string.char(1, 5, 1)
assert(Auth.poll(state, fake, 10.5) == "authenticated")

local counterfeit = setmetatable({ inbound = challenge, outbound = "" }, { __index = fake })
local rejected = assert(Auth.new(key, launch_id, 47800, 30))
assert(Auth.poll(rejected, counterfeit, 30.1) == "pending")
assert(Auth.poll(rejected, counterfeit, 30.2) == "pending")
counterfeit.inbound = "ECFA" .. string.char(1, 3, 1) .. string.rep("\0", 32)
assert(Auth.poll(rejected, counterfeit, 30.3) == "failed")
assert(#counterfeit.outbound == 88)

local expired = assert(Auth.new(key, launch_id, 47800, 20))
assert(Auth.poll(expired, fake, 25) == "failed")

print("mesen producer front authentication tests passed")
