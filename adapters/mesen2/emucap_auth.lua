-- Cache the launch-bound front key so reconnects retain the original authority.
local M = {}
local loaded, token = false, nil
function M.front_key()
  if loaded then return token end
  local selected = os.getenv("EMUCAP_SESSION_TOKEN")
  if selected and selected ~= "" then
    token = selected
  else
    local path = os.getenv("EMUCAP_SESSION_TOKEN_FILE")
    if path and path ~= "" then
      local file = assert(io.open(path, "rb"), "cannot open session credential")
      local ok, value = pcall(file.read, file, 1025)
      local closed = file:close()
      assert(ok and closed and type(value) == "string" and #value > 0 and #value <= 1024,
        "invalid session credential")
      token = value
    end
  end
  loaded = true
  return token
end
return M
