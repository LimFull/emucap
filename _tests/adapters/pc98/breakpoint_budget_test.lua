-- Both protocol routes enter the production shared Lua admission and retirement paths.
local root = arg[1] or "."
package.path = root .. "/adapters/mame-pc98/plugins/?.lua;" .. package.path
local F = dofile(root .. "/_tests/adapters/pc98/control_io_fixture.lua")
local function set(f, name, value)
  for i=1,100 do
    local key = debug.getupvalue(f,i)
    if key==name then debug.setupvalue(f,i,value); return end
    if not key then break end
  end
  error("missing capture: " .. name)
end
local function count(t) local n=0; for _ in pairs(t) do n=n+1 end; return n end
local t = {callbacks={}, writes={}, commands={}, log={}, next_id=1, native={}}
t.debugger={execution_state="run"}
function t.debugger:command(command)
  t.commands[#t.commands+1]=command
  local kind=command:match("^(bpset)") or command:match("^(wp) ") or command:match("^(rpset)")
  if kind then
    local id=t.next_id; t.next_id=id+1; t.native[id]=kind
    t.log[#t.log+1]=string.format("%s %X set", ({bpset="Breakpoint",wp="Watchpoint",rpset="Registerpoint"})[kind],id)
  else
    local id=assert(tonumber(command:match("clear (%x+)"),16),command)
    assert(t.native[id]); t.native[id]=nil
  end
end
function t.debugger:command_with_parameters(command, parameters)
  t.parameters = {command=command, values=parameters}
  -- The native fixture checks that parameter text never reaches the console parser.
  self:command(command .. " " .. parameters[1])
end
t.cpu={shortname="i386",debug={go=function() t.debugger.execution_state="run" end},
  state=setmetatable({}, {__index=function() return {value=0} end})}
manager={machine={paused=false,screens={at=function() return {frame_number=7} end},video={}}}
emu={pause=function() manager.machine.paused=true end,unpause=function() manager.machine.paused=false end,
  register_periodic=function(f) t.callbacks.periodic=f end}
for _, name in ipairs({"reset","stop","pre_save","post_load","frame"}) do
  emu["add_machine_"..name.."_notifier"]=function(f) t.callbacks[name]=f; return f end
end
F.install(emu,{read=function() return "" end,write=function(_,data) t.writes[#t.writes+1]=data; return #data end})
local output=print; print=function() end
dofile(root .. "/adapters/mame-pc98/plugins/emucap_gdbstub/init.lua").startplugin()
set(t.callbacks.periodic,"cpu",t.cpu); set(t.callbacks.periodic,"debugger",t.debugger)
local service=F.upvalue(t.callbacks.periodic,"service_frozen_socket")
local handle=F.upvalue(service,"handle")
local set_bp=F.upvalue(handle,"set_bp")
local set_wp=F.upvalue(handle,"set_wp")
local emucap=F.upvalue(handle,"handle_emucap")
local set_rp=F.upvalue(emucap,"set_rp")
local breaks={byidx={},byaddr={},pause={}}
local watches={byidx={},byaddr={}}
local regpoints={byidx={}}
set(set_bp,"breaks",breaks); set(set_wp,"watches",watches); set(set_rp,"regpoints",regpoints)
set(set_bp,"consolelog",t.log)
local function call(payload)
  local first=#t.writes+1
  F.call(handle,payload)
  return table.concat(t.writes,"",first)
end
local function hex(s) return (s:gsub(".",function(c) return string.format("%02x",c:byte()) end)) end
local function q(name,value) return "qEmucap,"..name..","..hex(value) end
for _, condition in ipairs({"1; bpset 200", "1,{bpset 200;g}", "max(esp, eax) == 1"}) do
  for _, spec in ipairs({
    {"setpoint", "0|100|1|1|", "bpset", 2, "bp"},
    {"setpoint", "0|100|1|", "bpset", 2, "bp"},
    {"setpoint", "2|100|1|1|", "wp", 4, "wp"},
    {"setpoint", "2|100|1|", "wp", 4, "wp"},
    {"setregpoint", "1|", "rpset", 1, "rp"},
    {"setregpoint", "", "rpset", 1, "rp"},
  }) do
    local before=#t.commands
    assert(not call(q(spec[1],spec[2]..condition)):find("$E",1,true))
    assert(#t.commands==before+1)
    assert(t.parameters.command==spec[3] and #t.parameters.values==spec[4])
    assert(t.parameters.values[spec[4]]==condition)
    assert(call(q("clearpoint",spec[5].."|"..(t.next_id-1))):find("OK",1,true))
  end
end
local structured=t.debugger.command_with_parameters
t.debugger.command_with_parameters=nil
local before=#t.commands
local ok,err=pcall(call,"Z0,100,1")
assert(not ok and tostring(err):find("rebuild",1,true))
assert(#t.commands==before and count(t.native)==0 and count(breaks.byidx)==0)
t.debugger.command_with_parameters=structured
t.next_id=1
for i=0,127 do
  local response
  if i%3==0 then response=call(string.format("Z0,%x,1",i))
  elseif i%3==1 then response=call(q("setpoint",string.format("2|%x|1|0|",i)))
  else response=call(q("setregpoint","0|esp < 1")) end
  assert(not response:find("$E",1,true),response)
end
assert(count(t.native)==128 and count(breaks.byidx)+count(watches.byidx)+count(regpoints.byidx)==128)
local commands=#t.commands
for _, request in ipairs({"Z0,1000,1","Z2,1000,1","Z3,1000,1","Z4,1000,1",
  q("setpoint","0|1000|1|1|"),q("setpoint","3|1000|1|1|"),q("setregpoint","esp < 2")}) do
  assert(call(request):find("E0E",1,true))
end
assert(#t.commands==commands)
assert(call("z0,0,1"):find("OK",1,true))
assert(breaks.pause[1]==nil and count(t.native)==127)
assert(call("Z1,1000,1"):find("OK",1,true))
local id=t.next_id-1
assert(call(q("clearpoint","bp|"..id)):find("OK",1,true))
assert(breaks.pause[id]==nil)
for _=1,256 do
  assert(call("Z0,1000,1"):find("OK",1,true))
  id=t.next_id-1
  assert(call(q("clearpoint","bp|"..id)):find("OK",1,true))
end
assert(count(breaks.pause)==count(breaks.byidx),"explicit clears leaked pause metadata")

-- A native one-shot releases capacity before a same-public-ID consumer rearms.
assert(call(q("setpoint","0|1000|1|0|")):find("BP:",1,true))
id=t.next_id-1
t.log[#t.log+1]=string.format("Stopped at breakpoint %X",id)
t.debugger.execution_state="stop"
assert(F.upvalue(t.callbacks.periodic,"note_breakpoint")())
assert(not breaks.byidx[id] and not breaks.pause[id] and count(t.native)==127)
assert(call(q("setpoint","0|1000|1|0|")):find("BP:",1,true))
assert(count(t.native)==128)
print=output
print("MAME shared breakpoint admission, protocol aliases, clear metadata and native rearm passed")
