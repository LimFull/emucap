#!/usr/bin/env python3
"""Run shipped MAME pacing branches against controlled native properties."""
from pathlib import Path
import shutil
import subprocess
import tempfile

root = Path(__file__).resolve().parents[2]
source = (root / 'adapters/mame-pc98/plugins/emucap_gdbstub/init.lua').read_text()
owners = source[source.index('  local function native_pacing()'):source.index('  local function handle_emucap(payload)')]
branch = source.split('    elseif name == "setpacing" then', 1)[1].split('    elseif name == "restorepacing" then', 1)[0]
restore = source.split('    elseif name == "restorepacing" then', 1)[1].split('    elseif name == "opdeadline" then', 1)[0]
setup = r'''
local state = { throttled=true, speed_factor=1000, throttle_rate=0.75, fastforward=false }
local restoring = false
local writes = 0
local video = setmetatable({}, {
  __index=function(_, key)
    -- The native arithmetic domain is tested separately against actual C++ bodies.
    if key == "is_valid_throttle_rate" then return function(_, v) return v >= 1e-6 and v < 1e9 end end
    if restoring and fault == "read_error" and key == "speed_factor" then error("readback failed") end
    return state[key]
  end,
  __newindex=function(_, key, value)
    writes = writes + 1
    if key == "speed_factor" and value == 2000 then
      state[key] = value
      restoring = true
      error("apply failed after mutation")
    end
    if restoring and key == fault then
      if key == "throttled" then value = false
      elseif key == "speed_factor" then value = 1500
      elseif key == "throttle_rate" then value = 0.5 end
    end
    if restoring and fault == "setter_error" then error("restore setter failed") end
    state[key] = value
  end
})
manager = {machine={video=video, options={entries={refreshspeed={value=function() return false end}}}}}
local pacing_revision, pacing_last_key = 0, nil
local function first_screen() return nil end
local function boundary_token() return "1@2.0|9" end
local function hex_to_string(v) return v end
local function string_to_hex(v) return v end
local socket = {}
local reply
local function ack_packet(_, value) reply = value end
'''
lua = shutil.which('lua')
assert lua, 'lua is required'
def run_case(name, case, fault='none'):
    code = ('local fault = "' + fault + '"\n' + setup + owners +
            '\nlocal function invoke(rest)\n' + branch + '\nend\n' +
            '\nlocal function restore(rest)\n' + restore + '\nend\n' + case)
    with tempfile.TemporaryDirectory(prefix='emucap-mame-policy-') as temp:
        path = Path(temp) / 'test.lua'
        path.write_text(code)
        result = subprocess.run([lua, str(path)], capture_output=True, text=True)
        if result.returncode:
            raise AssertionError(f'{name}: {result.stderr}')
    print(f'PASS {name}')

for fault in ['verified', 'speed_factor', 'throttle_rate', 'throttled', 'read_error', 'setter_error']:
    expected = 'restored' if fault == 'verified' else 'unrestored'
    run_case(fault, 'invoke("limited|2000")\n' + f'assert(reply:match("^E1D:{expected}:"), reply)\n', fault)

run_case('invalid_restore_is_nonmutating', r'''
pacing_payload()
for _, rate in ipairs({"0", "0.0", ".0", "000", "-0", "-1", ".", "1..2", "nan", "inf", "1e999", "1e-999", "0.00000000000000000000001", "1000000000"}) do
  restore("1|0|500|" .. rate)
  assert(reply == "E00", rate .. ": " .. tostring(reply))
  assert(writes == 0 and pacing_revision == 1 and state.throttled and state.speed_factor == 1000 and state.throttle_rate == 0.75)
end
for _, speed in ipairs({"-1", "1.5", "2147483648", "18446744073709551615", string.rep("9", 400)}) do
  restore("1|0|" .. speed .. "|1")
  assert(reply == "E00" and writes == 0 and pacing_revision == 1, speed)
end
''')
run_case('custom_restore_zero_speed_and_conflict', r'''
pacing_payload()
restore("0|0|500|0.75")
assert(reply:match("^CONFLICT|") and writes == 0)
restore("1|0|0|2e0")
assert(state.speed_factor == 0 and state.throttle_rate == 2 and not state.throttled)
restore(tostring(pacing_revision) .. "|1|2147483647|7.5e-1")
assert(state.speed_factor == 2147483647 and state.throttle_rate == 0.75 and state.throttled)
invoke("unlimited")
assert(not state.throttled and state.speed_factor == 2147483647 and state.throttle_rate == 0.75)
invoke("limited|100000")
assert(state.throttled and state.speed_factor == 100000 and state.throttle_rate == 1)
''')
run_case('unsafe_previous_is_rejected_before_transaction', r'''
for _, bad in ipairs({0, 1e-100, math.huge}) do
  state.throttle_rate = bad
  invoke("limited|500")
  assert(reply == "E00" and writes == 0 and pacing_revision == 0)
  invoke("unlimited")
  assert(reply == "E00" and writes == 0 and pacing_revision == 0)
end
''')
