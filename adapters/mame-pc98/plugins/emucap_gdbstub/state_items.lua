-- license:BSD-3-Clause
-- MAME save-manager item serialization; guest execution is owned by the bridge.

local function save_item_supported(size)
  return size == 1 or size == 2 or size == 4 or size == 8
end

local MAX_MANIFEST_BYTES = 1024 * 1024
local MAX_MANIFEST_ENTRIES = 4095
local MAX_FILENAME_BYTES = 255
local MAX_ITEM_BYTES = 128 * 1024 * 1024
local MAX_TOTAL_BYTES = 512 * 1024 * 1024
local MAX_SAFE_INTEGER = 9007199254740991
local MAX_SAFE_INTEGER_TEXT = "9007199254740991"

local function portable_filename(filename)
  if #filename == 0 or #filename > MAX_FILENAME_BYTES then return false end
  if not filename:match("^[A-Za-z0-9_.%-]+$") or
      filename:sub(1, 1) == "." or filename:sub(-1) == "." then
    return false
  end
  local stem = (filename:match("^([^.]+)") or filename):upper()
  if stem == "CON" or stem == "PRN" or stem == "AUX" or stem == "NUL" then return false end
  if stem:match("^COM[1-9]$") or stem:match("^LPT[1-9]$") then return false end
  return filename:lower() ~= "manifest.txt"
end

local function decimal_integer(raw)
  if not raw:match("^%d+$") then return nil end
  local normalized = raw:gsub("^0+", "")
  if normalized == "" then normalized = "0" end
  if #normalized > #MAX_SAFE_INTEGER_TEXT or
      (#normalized == #MAX_SAFE_INTEGER_TEXT and normalized > MAX_SAFE_INTEGER_TEXT) then
    return nil
  end
  local value = tonumber(normalized, 10)
  if not value or value < 0 or value > MAX_SAFE_INTEGER or value ~= math.floor(value) then
    return nil
  end
  return value
end

local function save_items_to_dir(path)
  -- Refresh device-owned serialization buffers at this frozen guest boundary.
  -- A host without the native presave hook must fail rather than save stale buffers.
  manager.machine:prepare_state_save()
  local manifest_path = path .. "/manifest.txt"
  local manifest, manifest_err = io.open(manifest_path, "wb")
  if not manifest then
    return nil, "manifest open failed: " .. tostring(manifest_err)
  end

  local saved = 0
  local skipped = 0
  local idx = 0
  while true do
    local item = emu.item(idx)
    if not item or item.size == 0 then
      break
    end

    if item.count > 0 then
      local filename = string.format("item_%06d.bin", idx)
      local bytes_len = item.size * item.count
      local ok, data_or_err = pcall(function() return item:read_block(0, bytes_len) end)
      if not ok then
        manifest:close()
        return nil, "item read failed at " .. tostring(idx) .. ": " .. tostring(data_or_err)
      end
      if type(data_or_err) ~= "string" or #data_or_err ~= bytes_len then
        manifest:close()
        return nil, "item read length mismatch at " .. tostring(idx)
      end
      local f, file_err = io.open(path .. "/" .. filename, "wb")
      if not f then
        manifest:close()
        return nil, "item file open failed: " .. tostring(file_err)
      end
      local written, write_err = f:write(data_or_err)
      local closed, close_err = f:close()
      if not written or not closed then
        manifest:close()
        return nil, "item write failed: " .. tostring(write_err or close_err)
      end
      local recorded, record_err = manifest:write(string.format("%d|%d|%d|%d|%s\n", idx, item.size, item.count, bytes_len, filename))
      if not recorded then
        manifest:close()
        return nil, "manifest write failed: " .. tostring(record_err)
      end
      saved = saved + 1
      if not save_item_supported(item.size) then
        skipped = skipped + 1
      end
    end
    idx = idx + 1
  end

  local closed, close_err = manifest:close()
  if not closed then
    return nil, "manifest close failed: " .. tostring(close_err)
  end
  return saved, skipped
end

local function value_from_bytes(data, pos, size)
  local value = 0
  for offset = 0, size - 1 do
    value = value | ((data:byte(pos + offset) or 0) << (offset * 8))
  end
  return value
end

local function load_items_from_dir(path)
  local manifest, manifest_err = io.open(path .. "/manifest.txt", "rb")
  if not manifest then
    return nil, "manifest open failed: " .. tostring(manifest_err)
  end

  local manifest_data, read_err = manifest:read(MAX_MANIFEST_BYTES + 1)
  local manifest_closed, manifest_close_err = manifest:close()
  if not manifest_data and read_err then
    return nil, "manifest read failed: " .. tostring(read_err)
  end
  if not manifest_closed then
    return nil, "manifest close failed: " .. tostring(manifest_close_err)
  end
  manifest_data = manifest_data or ""
  if #manifest_data > MAX_MANIFEST_BYTES then
    return nil, "manifest exceeds " .. tostring(MAX_MANIFEST_BYTES) .. " bytes"
  end

  local entries = {}
  local indexes = {}
  local filenames = {}
  local total_bytes = 0
  local position = 1
  while position <= #manifest_data do
    local newline = manifest_data:find("\n", position, true)
    local line
    if newline then
      line = manifest_data:sub(position, newline - 1)
      position = newline + 1
    else
      line = manifest_data:sub(position)
      position = #manifest_data + 1
    end
    if line == "" then
      return nil, "bad empty manifest line"
    end
    if #entries >= MAX_MANIFEST_ENTRIES then
      return nil, "manifest entry count exceeds " .. tostring(MAX_MANIFEST_ENTRIES)
    end
    local idx_s, size_s, count_s, bytes_s, filename = line:match("^(%d+)|(%d+)|(%d+)|(%d+)|([^|]+)$")
    local idx = decimal_integer(idx_s or "")
    local size = decimal_integer(size_s or "")
    local count = decimal_integer(count_s or "")
    local bytes_len = decimal_integer(bytes_s or "")
    if not idx or not size or not count or not bytes_len or not filename then
      return nil, "bad manifest line: " .. tostring(line)
    end
    if not portable_filename(filename) then
      return nil, "unsafe manifest filename: " .. tostring(filename)
    end
    if indexes[idx] then return nil, "duplicate save item index " .. tostring(idx) end
    local folded_filename = filename:lower()
    if filenames[folded_filename] then
      return nil, "duplicate save item filename " .. tostring(filename)
    end
    local expected_len = 0
    if count > 0 then
      if size > math.floor(MAX_ITEM_BYTES / count) then
        return nil, "save item byte length exceeds " .. tostring(MAX_ITEM_BYTES)
      end
      expected_len = size * count
    end
    if bytes_len ~= expected_len or bytes_len > MAX_ITEM_BYTES then
      return nil, "save item byte length mismatch at " .. tostring(idx)
    end
    total_bytes = total_bytes + bytes_len
    if total_bytes > MAX_TOTAL_BYTES then
      return nil, "save item byte total exceeds " .. tostring(MAX_TOTAL_BYTES)
    end

    local item = emu.item(idx)
    if not item or item.size ~= size or item.count ~= count then
      return nil, "save item mismatch at " .. tostring(idx)
    end
    indexes[idx] = true
    filenames[folded_filename] = true
    entries[#entries + 1] = {
      idx = idx, size = size, count = count, bytes_len = bytes_len,
      filename = filename, item = item,
    }
  end

  local restored = 0
  local skipped = 0
  for _, entry in ipairs(entries) do
    local f, file_err = io.open(path .. "/" .. entry.filename, "rb")
    if not f then
      return nil, "item file open failed: " .. tostring(file_err)
    end
    local data, data_err
    if entry.bytes_len == 0 then
      data = ""
    else
      data, data_err = f:read(entry.bytes_len)
    end
    local extra, extra_err = f:read(1)
    local closed, close_err = f:close()
    if data_err or extra_err or not closed then
      return nil, "item read failed at " .. tostring(entry.idx) .. ": " ..
        tostring(data_err or extra_err or close_err)
    end
    if type(data) ~= "string" or #data ~= entry.bytes_len or extra ~= nil then
      return nil, "item data length mismatch at " .. tostring(entry.idx)
    end

    if entry.count > 0 and save_item_supported(entry.size) then
      for item_entry = 0, entry.count - 1 do
        entry.item:write(item_entry, value_from_bytes(data, (item_entry * entry.size) + 1, entry.size))
      end
      restored = restored + 1
    elseif entry.count > 0 then
      skipped = skipped + 1
    end
  end

  return restored, skipped
end

return { save = save_items_to_dir, load = load_items_from_dir }
