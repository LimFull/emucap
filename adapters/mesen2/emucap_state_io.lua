-- Savestate file I/O shared by the live Mesen adapter and its tests.
--
-- Mesen serializes to and from strings. Keep the destination untouched until the complete new
-- state has been written, then replace it. The backup path is only needed on hosts where rename
-- does not replace an existing file.

local M = {}
local MAX_STATE_BYTES = 64 * 1024 * 1024

local function read_all(path)
  local file, err = io.open(path, "rb")
  if not file then error(err or ("cannot open " .. tostring(path))) end
  local size, size_err = file:seek("end")
  if not size or size < 1 or size > MAX_STATE_BYTES then
    file:close()
    error(size_err or "savestate must contain 1..67108864 bytes")
  end
  local position, seek_err = file:seek("set", 0)
  if position ~= 0 then file:close(); error(seek_err or "cannot seek savestate") end
  local data, read_err = file:read(size + 1)
  local closed, close_err = file:close()
  if data == nil then error(read_err or ("cannot read " .. tostring(path))) end
  if not closed then error(close_err or ("cannot close " .. tostring(path))) end
  if #data ~= size then error("savestate size changed during read") end
  return data
end

local function write_all(path, data)
  local file, err = io.open(path, "wb")
  if not file then error(err or ("cannot open " .. tostring(path))) end
  local wrote, write_err = file:write(data)
  if not wrote then
    file:close()
    error(write_err or ("cannot write " .. tostring(path)))
  end
  local closed, close_err = file:close()
  if not closed then error(close_err or ("cannot close " .. tostring(path))) end
end

local function exists(path)
  local file = io.open(path, "rb")
  if not file then return false end
  file:close()
  return true
end

local function token(value)
  return (tostring(value or {}) .. tostring({})):gsub("[^%w_.-]", "_")
end

function M.replace(path, data, request_id)
  if type(path) ~= "string" or path == "" then error("savestate path is required") end
  if type(data) ~= "string" then error("savestate data must be a string") end
  if #data < 1 or #data > MAX_STATE_BYTES then error("savestate must contain 1..67108864 bytes") end

  local suffix = token(request_id)
  local temporary = path .. ".emucap.tmp." .. suffix
  local backup = path .. ".emucap.old." .. suffix
  os.remove(temporary)
  os.remove(backup)

  local wrote, write_err = pcall(write_all, temporary, data)
  if not wrote then
    os.remove(temporary)
    error(write_err)
  end

  local installed, install_err = os.rename(temporary, path)
  if installed then return #data end

  -- Windows does not replace an existing destination with rename. Move the old file aside only
  -- after the complete temporary file exists, and restore it if installing the replacement fails.
  if not exists(path) then
    os.remove(temporary)
    error(install_err or ("cannot install " .. path))
  end
  local moved, move_err = os.rename(path, backup)
  if not moved then
    os.remove(temporary)
    error(move_err or ("cannot preserve " .. path))
  end
  installed, install_err = os.rename(temporary, path)
  if not installed then
    local restored, restore_err = os.rename(backup, path)
    os.remove(temporary)
    if not restored then
      error((install_err or "savestate replacement failed")
        .. "; restoring the previous file also failed: " .. tostring(restore_err))
    end
    error(install_err or ("cannot install " .. path))
  end
  os.remove(backup)
  return #data
end

function M.save(host, path, request_id)
  local data = host.createSavestate()
  if type(data) ~= "string" then error("Mesen returned invalid savestate data") end
  return M.replace(path, data, request_id)
end

function M.load(host, path, on_native_failure)
  local data = read_all(path)
  local ok, loaded = pcall(host.loadSavestate, data)
  if not ok or loaded == false then
    local message = ok and "Mesen rejected the savestate" or tostring(loaded)
    -- Native failure can follow partial CPU/device mutation. File admission
    -- failures above never enter this branch.
    if on_native_failure then on_native_failure(message) end
    error(message)
  end
  return #data
end

return M
