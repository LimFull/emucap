-- Native-binding double for production callback tests. Transport framing remains real.
local F = {}
function F.upvalue(f, name)
  for i = 1, 100 do
    local key, value = debug.getupvalue(f, i)
    if key == name then return value end
    if not key then break end
  end
  error("missing callback capture: " .. name)
end

function F.install(emu, legacy)
  local now, pending = 0, ""
  emu.emucap_host_time_ms = function() return now end
  emu.emucap_host_wait = function() now = now + 5 end
  emu.emucap_read_framebuffer = function() error("unexpected native framebuffer read") end
  emu.emucap_control_socket = function()
    return {
      close = function() if legacy.close then legacy:close() end end,
      read = function()
        local data = legacy:read(4096)
        if data == "" then return "idle", "" end
        return "data", data
      end,
      write = function(_, data)
        assert(#data <= 4096)
        pending = pending .. data
        while #pending > 0 do
          if pending:sub(1, 1) == "+" then
            legacy:write("+")
            pending = pending:sub(2)
          else
            local hash = pending:find("#", 1, true)
            if not hash or #pending < hash + 2 then break end
            legacy:write(pending:sub(1, hash + 2))
            pending = pending:sub(hash + 3)
          end
        end
        return "data", #data
      end,
    }
  end
end

function F.call(handle, ...)
  local transport = F.upvalue(handle, "socket")
  transport:pump()
  local result = handle(...)
  while not transport.terminal and #transport.tx > 0 do transport:pump() end
  return result
end

function F.wire(payload)
  local encoded = payload:gsub("[\3#$}*]", function(c) return "}" .. string.char(c:byte() ~ 0x20) end)
  local sum = 0
  for i = 1, #encoded do sum = (sum + encoded:byte(i)) % 256 end
  return "$" .. encoded .. "#" .. string.format("%02x", sum)
end
return F
