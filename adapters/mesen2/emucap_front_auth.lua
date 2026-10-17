local M = {}

local MASK = 0xffffffff
local K = {
  0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,
  0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,
  0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,
  0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,
  0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,
  0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,
  0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,
  0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2,
}

local function ror(value, count)
  return ((value >> count) | (value << (32 - count))) & MASK
end

local function sha256(message)
  local bit_length = #message * 8
  local pad = (56 - ((#message + 1) % 64)) % 64
  message = message .. "\128" .. string.rep("\0", pad) .. string.pack(">I8", bit_length)
  local h = {0x6a09e667,0xbb67ae85,0x3c6ef372,0xa54ff53a,
             0x510e527f,0x9b05688c,0x1f83d9ab,0x5be0cd19}
  for offset = 1, #message, 64 do
    local w = {}
    for index = 0, 15 do
      w[index] = string.unpack(">I4", message, offset + index * 4)
    end
    for index = 16, 63 do
      local x, y = w[index - 15], w[index - 2]
      local s0 = ror(x, 7) ~ ror(x, 18) ~ (x >> 3)
      local s1 = ror(y, 17) ~ ror(y, 19) ~ (y >> 10)
      w[index] = (w[index - 16] + s0 + w[index - 7] + s1) & MASK
    end
    local a,b,c,d,e,f,g,hh = table.unpack(h)
    for index = 0, 63 do
      local s1 = ror(e, 6) ~ ror(e, 11) ~ ror(e, 25)
      local choose = (e & f) ~ ((~e) & g)
      local first = (hh + s1 + choose + K[index + 1] + w[index]) & MASK
      local s0 = ror(a, 2) ~ ror(a, 13) ~ ror(a, 22)
      local majority = (a & b) ~ (a & c) ~ (b & c)
      local second = (s0 + majority) & MASK
      hh,g,f,e,d,c,b,a = g,f,e,(d + first) & MASK,c,b,a,(first + second) & MASK
    end
    local values = {a,b,c,d,e,f,g,hh}
    for index = 1, 8 do h[index] = (h[index] + values[index]) & MASK end
  end
  local out = {}
  for index = 1, 8 do out[index] = string.pack(">I4", h[index]) end
  return table.concat(out)
end

local function hmac(key, fields)
  if #key > 64 then key = sha256(key) end
  key = key .. string.rep("\0", 64 - #key)
  local inner, outer = {}, {}
  for index = 1, 64 do
    local byte = key:byte(index)
    inner[index] = string.char(byte ~ 0x36)
    outer[index] = string.char(byte ~ 0x5c)
  end
  local parts = {table.concat(inner)}
  for _, field in ipairs(fields) do
    parts[#parts + 1] = string.pack(">I8", #field)
    parts[#parts + 1] = field
  end
  return sha256(table.concat(outer) .. sha256(table.concat(parts)))
end

local function equal(left, right)
  if #left ~= #right then return false end
  local difference = 0
  for index = 1, #left do difference = difference | (left:byte(index) ~ right:byte(index)) end
  return difference == 0
end

local function boundary(front_key)
  if type(front_key) ~= "string" then return nil end
  local direct = front_key:match("^front%-direct%-([0-9a-fA-F]+)$")
  if direct and #direct == 64 then return 1 end
  local broker = front_key:match("^front%-broker%-([0-9a-fA-F]+)$")
  if broker and #broker == 64 then return 2 end
end

local function valid_launch_id(launch_id)
  return type(launch_id) == "string" and #launch_id == 33
    and launch_id:match("^launch%-[0-9abcdefghjkmnpqrstvwxyz]+$") ~= nil
end

local function header(kind, boundary_id)
  return "ECFA" .. string.char(1, kind, boundary_id)
end

local function receive_record(state, socket, size)
  local need = size - #state.input
  local data, err, partial = socket:receive(need)
  local chunk = data or partial or ""
  state.input = state.input .. chunk
  if err and err ~= "timeout" then return nil, err end
  if #state.input < size then return false end
  if #state.input ~= size then return nil, "oversized authentication record" end
  return true
end

local function send_record(state, socket)
  local sent, err, last = socket:send(state.output, state.output_index)
  local completed = sent or last
  if completed and completed >= state.output_index then state.output_index = completed + 1 end
  if err and err ~= "timeout" then return nil, err end
  return state.output_index > #state.output
end

function M.new(front_key, launch_id, port, now)
  local boundary_id = boundary(front_key)
  if not boundary_id or not valid_launch_id(launch_id)
      or math.type(port) ~= "integer" or port < 1 or port > 65535 then
    return nil, "invalid producer authentication identity"
  end
  return {
    front_key = front_key, launch_id = launch_id, port = port, boundary = boundary_id,
    phase = "challenge", input = "", output = "", output_index = 1,
    deadline = now + 5,
  }
end

function M.poll(state, socket, now)
  if now >= state.deadline then return "failed", "producer authentication deadline expired" end
  if state.phase == "challenge" then
    local ready, err = receive_record(state, socket, 25)
    if ready == nil then return "failed", err end
    if not ready then return "pending" end
    local challenge = state.input
    if challenge:sub(1, 7) ~= header(1, state.boundary)
        or challenge:sub(8, 9) ~= string.pack(">I2", state.port) then
      return "failed", "invalid listener challenge"
    end
    local nonce = hmac(state.front_key, {"emucap-producer-front-nonce-v1", challenge}):sub(1, 16)
    state.transcript = challenge .. state.launch_id .. nonce
    state.output = header(2, state.boundary) .. state.launch_id .. nonce
      .. hmac(state.front_key, {"emucap-producer-front-auth-v1", "producer", state.transcript})
    state.output_index, state.input, state.phase = 1, "", "producer"
    return "pending"
  elseif state.phase == "producer" then
    local ready, err = send_record(state, socket)
    if ready == nil then return "failed", err end
    if ready then state.output, state.output_index, state.phase = "", 1, "listener" end
    return "pending"
  elseif state.phase == "listener" then
    local ready, err = receive_record(state, socket, 39)
    if ready == nil then return "failed", err end
    if not ready then return "pending" end
    local expected = hmac(state.front_key,
      {"emucap-producer-front-auth-v1", "listener", state.transcript})
    if state.input:sub(1, 7) ~= header(3, state.boundary)
        or not equal(state.input:sub(8), expected) then
      return "failed", "invalid listener proof"
    end
    state.output = header(4, state.boundary)
      .. hmac(state.front_key, {"emucap-producer-front-auth-v1", "commit", state.transcript})
    state.output_index, state.input, state.phase = 1, "", "commit"
    return "pending"
  elseif state.phase == "commit" then
    local ready, err = send_record(state, socket)
    if ready == nil then return "failed", err end
    if ready then state.output, state.output_index, state.phase = "", 1, "ok" end
    return "pending"
  elseif state.phase == "ok" then
    local ready, err = receive_record(state, socket, 7)
    if ready == nil then return "failed", err end
    if not ready then return "pending" end
    if state.input ~= header(5, state.boundary) then
      return "failed", "invalid listener completion"
    end
    state.phase = "authenticated"
    return "authenticated"
  end
  return state.phase == "authenticated" and "authenticated" or "failed", "invalid authentication state"
end

M._sha256 = sha256
M._hmac = hmac
return M
