-- license:BSD-3-Clause
-- Shared bounded RSP transport. Native methods perform at most one nonblocking call.
local M = {}
local PAYLOAD, CHUNK, CALLS, WORK, REQUESTS = 262144, 4096, 8, 32768, 16
local FRAME = 2 * PAYLOAD + 4
local RX_LIMIT, TX_LIMIT, LIFETIME = FRAME + CHUNK, 2 * FRAME + 2, 5000

function M.new(native, now, on_close)
  local self = { native = native, now = now, on_close = on_close, terminal = false,
    rx = "", queue = {}, queued = 0, interrupt = false,
    phase = "idle", parts = {}, part = {}, part_size = 0, size = 0, encoded = 0,
    tx = {}, tx_bytes = 0, tx_calls = 0, tx_work = 0 }
  return setmetatable(self, { __index = M })
end

function M:close(reason)
  if self.terminal then return end
  self.terminal = true
  pcall(function() self.native:close() end)
  self.rx, self.queue, self.tx = "", {}, {}
  self.parts, self.part = {}, {}
  self.queued, self.tx_bytes, self.size, self.part_size = 0, 0, 0, 0
  self.interrupt, self.started = false, nil
  if self.on_close then self.on_close(reason) end
end

function M:_reset_frame()
  self.phase, self.started, self.escaped = "idle", nil, false
  self.parts, self.part, self.part_size, self.size, self.encoded = {}, {}, 0, 0, 0
end

function M:discard_input()
  self.rx, self.queue, self.queued, self.interrupt, self.rx_time = "", {}, 0, false, nil
  self:_reset_frame()
end

function M:_append(byte)
  self.size = self.size + 1
  if self.size > PAYLOAD then self:close("decoded frame exceeds budget"); return end
  self.part_size = self.part_size + 1
  self.part[self.part_size] = string.char(byte)
  if self.part_size == CHUNK then
    self.parts[#self.parts + 1] = table.concat(self.part)
    self.part, self.part_size = {}, 0
  end
end

function M:_consume(budget)
  if not self:_deadlines() then return 0 end
  local used = 0
  while used < #self.rx and used < budget and #self.queue < REQUESTS and not self.terminal do
    used = used + 1
    local byte = self.rx:byte(used)
    if byte == 3 and not self.escaped then
      -- A raw interrupt abandons an incomplete frame; escaped # is } followed by byte 3.
      self:_reset_frame()
      self.interrupt = true
    elseif self.phase == "idle" then
      if byte == 36 then
        self.phase, self.started, self.sum = "payload", self.rx_time or self.now(), 0
      elseif byte ~= 43 and byte ~= 45 then
        self:close("invalid RSP prefix")
      end
    elseif self.phase == "payload" then
      if not self.escaped and byte == 35 then
        self.phase = "checksum"
        self.checksum = ""
      else
        self.encoded = self.encoded + 1
        if self.encoded > 2 * PAYLOAD then self:close("encoded frame exceeds budget"); break end
        self.sum = (self.sum + byte) % 256
        if self.escaped then
          self.escaped = false
          self:_append(byte ~ 0x20)
        elseif byte == 125 then
          self.escaped = true
        elseif byte == 36 then
          self:close("nested RSP frame")
        else
          self:_append(byte)
        end
      end
    else
      local char = string.char(byte)
      if not char:match("^%x$") then self:close("invalid RSP checksum"); break end
      self.checksum = self.checksum .. char
      if #self.checksum == 2 then
        if not self:_deadlines() then break end
        if tonumber(self.checksum, 16) ~= self.sum then self:close("RSP checksum mismatch"); break end
        self.parts[#self.parts + 1] = table.concat(self.part)
        local payload = table.concat(self.parts)
        self.queue[#self.queue + 1] = payload
        self.queued = self.queued + #payload
        self:_reset_frame()
      end
    end
  end
  if not self.terminal then
    self.rx = self.rx:sub(used + 1)
    if #self.rx == 0 then self.rx_time = nil end
  end
  return used
end

function M:_deadlines()
  if self.terminal then return false end
  local now = self.now()
  if self.started and now - self.started >= LIFETIME then
    self:close("RSP assembly deadline")
  elseif self.tx[1] and now - self.tx[1].started >= LIFETIME then
    self:close("RSP transmit deadline")
  end
  return not self.terminal
end

function M:flush()
  while self:_deadlines() and self.tx[1] and self.tx_calls < CALLS and self.tx_work < WORK do
    local item = self.tx[1]
    local limit = math.min(CHUNK, WORK - self.tx_work)
    local bytes = item.data:sub(item.offset, item.offset + limit - 1)
    self.tx_calls = self.tx_calls + 1
    local ok, state, count = pcall(self.native.write, self.native, bytes)
    if not ok or state == "error" or state == "closed" then
      self:close("native control write failed"); break
    elseif state == "idle" then
      break
    elseif state ~= "data" or type(count) ~= "number" or count < 1 or count > #bytes or count % 1 ~= 0 then
      self:close("invalid native write result"); break
    end
    item.offset = item.offset + count
    self.tx_work = self.tx_work + count
    if item.offset > #item.data then
      self.tx_bytes = self.tx_bytes - #item.data
      table.remove(self.tx, 1)
    end
  end
  return not self.terminal
end

function M:pump()
  self.tx_calls, self.tx_work = 0, 0
  if not self:flush() then return false end
  local used = self:_consume(WORK)
  for _ = 1, CALLS do
    if self.terminal or #self.queue >= REQUESTS or used >= WORK then break end
    local ok, state, data = pcall(self.native.read, self.native)
    if not ok or state == "error" or state == "closed" then
      self:close("native control read failed"); break
    elseif state == "idle" then
      break
    elseif state == "data" then
      if type(data) ~= "string" or #data < 1 or #data > CHUNK
          or #self.rx + self.size + self.queued + #data > RX_LIMIT then
        self:close("native control read exceeds budget"); break
      end
      if #self.rx == 0 then self.rx_time = self.now() end
      self.rx = self.rx .. data
      used = used + self:_consume(WORK - used)
    elseif state ~= "accepted" then
      self:close("invalid native read result"); break
    end
  end
  return self:_deadlines()
end

function M:peek()
  if self.terminal then return nil end
  return self.interrupt and "\3" or self.queue[1]
end

function M:take()
  if self.terminal then return nil end
  if self.interrupt then self.interrupt = false; return "\3" end
  local payload = table.remove(self.queue, 1)
  if payload then self.queued = self.queued - #payload end
  return payload
end

function M:_enqueue(data)
  if not self:_deadlines() then return false end
  if self.tx_bytes + #data > TX_LIMIT or #self.tx >= 2 * REQUESTS then
    self:close("RSP response queue exceeds budget"); return false
  end
  self.tx[#self.tx + 1] = { data = data, offset = 1, started = self.now() }
  self.tx_bytes = self.tx_bytes + #data
  return true
end

function M:ack()
  return self:_enqueue("+")
end

function M:send(payload, ack)
  if self.terminal then return false end
  if type(payload) ~= "string" or #payload > PAYLOAD then
    self:close("RSP response exceeds payload budget"); return false
  end
  local sum = 0
  local encoded = payload:gsub("[\3#$}*]", function(char) return "}" .. string.char(char:byte() ~ 0x20) end)
  for i = 1, #encoded do sum = (sum + encoded:byte(i)) % 256 end
  return self:_enqueue((ack and "+" or "") .. "$" .. encoded .. "#" .. string.format("%02x", sum))
end

return M
