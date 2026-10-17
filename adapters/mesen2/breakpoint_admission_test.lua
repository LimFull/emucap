-- Exercise production registration, callback ownership and expiry with native doubles.
local root = arg[1] or "."
local file = assert(io.open(root .. "/adapters/mesen2/emucap-core.lua", "rb"))
local source = file:read("*a"); file:close()
local body = assert(source:match("(do\nlocal pending_breakpoint_refs.-)end %-%- persistent breakpoint registration scope")) .. "end"
local function count(t) local n = 0; for _ in pairs(t) do n = n + 1 end; return n end
local function setup()
  local env = setmetatable({
    handlers = {}, breakpoints = {}, next_bp_id = 1, events = {}, dropped = 0,
    CPU = 1, EXEC_MAX = 0xffffff, EVENT_CAP = 256, WATCH_REG_BUDGET = 1000000,
    VRAM_RECON_BUDGET = 100000000, STATE = "running", frame = 3,
    HAS_SNES_PPU_OBJ_EVENTS = true, native = {}, added = 0, evaluated = 0, stopped = 0,
    SYS = {dma_supported=true, bank_mirror=true, default_memtype="ram",
      non_bus_write_memtypes={vram="vram_recon"}, vram_write_target=function() return 0, 0 end},
    Memory = {range=function() return {} end},
    Step = {active=function() return false end},
    parse_snapshot_specs=function() return nil end, snum=tonumber,
    as_array=function(t) return t end,
  }, {__index = _G})
  local function add(callback, kind, lo, hi, cpu)
    env.added = env.added + 1
    if env.added == env.fail_add then error("native registration failed") end
    local id = env.added
    env.native[id] = {callback=callback, kind=kind, lo=lo, hi=hi, cpu=cpu}
    return id
  end
  local function remove(ref)
    if env.fail_remove then error("native removal failed") end
    assert(env.native[ref], "duplicate/unowned native removal")
    env.native[ref] = nil
  end
  env.emu = {
    callbackType={exec=1,read=2,write=3},
    eventType={nmi=4,irq=5,snesPpuObjEvaluationStart=6,snesPpuObjScanlineHandoff=7},
    addMemoryCallback=add, addEventCallback=add,
    removeMemoryCallback=remove, removeEventCallback=remove,
    getState=function() env.evaluated=env.evaluated+1; return {["cpu.sp"]=0} end,
  }
  env.record_reg_hit = function(bp) if bp.pause_on_hit then env.stopped=env.stopped+1 end end
  env.record_hit = env.record_reg_hit
  assert(load(body, "production breakpoint registry", "t", env))()
  return env
end
local points = {
  {kind="exec", memory_type="ram",start=0},
  {kind="read", memory_type="ram",start=0x2117},
  {kind="write", memory_type="ram",start=0x2117},
  {kind="nmi"}, {kind="irq"}, {kind="dma"},
  {kind="snes_obj_eval_start",start=0,["end"]=1},
  {kind="snes_obj_handoff",start=0,["end"]=1},
  {kind="write",memory_type="vram",start=0},
}
local e = setup()
for i=1,128 do
  local ok, result
  if i%10==0 then ok,result=e.handlers.watch_register({})
  else ok,result=e.handlers.set_breakpoint(points[(i-1)%#points+1]) end
  assert(ok, tostring(result)); assert(result.id==i)
end
assert(count(e.breakpoints)==128 and count(e.native)<=256)
local added=e.added
for _, p in ipairs(points) do assert(not e.handlers.set_breakpoint(p)) end
assert(not e.handlers.watch_register({}) and e.added==added and e.next_bp_id==129)
e.fail_remove=true
assert(not e.handlers.clear_breakpoint({id=1}))
assert(count(e.breakpoints)==128 and not e.handlers.watch_register({}))
e.fail_remove=false
assert(e.handlers.clear_breakpoint({id=1}))
local ok,result=e.handlers.watch_register({})
assert(ok and result.id==129 and count(e.breakpoints)==128)
assert(e.handlers.clear_all_breakpoints())
assert(count(e.native)==0 and count(e.breakpoints)==0)

-- A failed second mirror/DMA install cannot bypass quota through orphan callbacks.
for _, p in ipairs({points[2],points[3],points[6]}) do
  e=setup()
  for _=1,200 do
    e.fail_add=e.added+2
    local accepted, kind=e.handlers.set_breakpoint(p)
    assert(not accepted and kind=="emulator_error")
    assert(count(e.native)==0 and count(e.breakpoints)==0 and e.next_bp_id==1)
  end
  e.fail_add=e.added+2; e.fail_remove=true
  local accepted, kind=e.handlers.set_breakpoint(p)
  assert(not accepted and kind=="cleanup_failed")
  assert(count(e.native)==1 and e.breakpoints[1].registration_failed and e.next_bp_id==2)
  assert(not e.handlers.clear_all_breakpoints())
  e.fail_remove=false
  assert(e.handlers.clear_all_breakpoints() and count(e.native)==0)
end

-- Expiry still retires ownership when output is saturated; exact step remains eligible.
for _, vram in ipairs({false,true}) do
  e=setup()
  for i=1,256 do e.events[i]={} end
  local accepted, record
  if vram then accepted,record=e.handlers.set_breakpoint({kind="write",memory_type="vram",start=0,max_instructions=2})
  else accepted,record=e.handlers.watch_register({max_instructions=2,pause_on_hit=false}) end
  assert(accepted)
  local callback=next(e.native) and e.native[next(e.native)].callback
  if vram then
    e.STATE="frozen"; callback(0,0); assert(e.breakpoints[record.id].seen==0)
    e.Step.active=function() return true end
  end
  callback(0,0); callback(0,0); callback(0,0)
  assert(count(e.native)==0 and count(e.breakpoints)==0 and e.dropped==3)
end
e=setup()
for i=1,256 do e.events[i]={} end
assert(e.handlers.watch_register({max_instructions=2,min=1,max=2,pause_on_hit=true}))
e.native[next(e.native)].callback(0,0)
assert(e.evaluated==1 and e.stopped==1)

for _, value in ipairs({-1,0.5,0/0,math.huge,"2",50000001}) do
  e=setup(); assert(not e.handlers.watch_register({max_instructions=value}))
  assert(e.added==0 and e.next_bp_id==1)
end
for _, value in ipairs({-1,0.5,0/0,math.huge,"2",100000001}) do
  e=setup(); assert(not e.handlers.set_breakpoint({kind="write",memory_type="vram",start=0,max_instructions=value}))
  assert(e.added==0 and e.next_bp_id==1)
end
e=setup()
local _, default=e.handlers.watch_register({}); assert(default.max_instructions==1000000)
local _, zero=e.handlers.watch_register({max_instructions=0}); assert(zero.max_instructions==1)
local _, maximum=e.handlers.watch_register({max_instructions=50000000}); assert(maximum.max_instructions==50000000)
local _, vram=e.handlers.set_breakpoint({kind="write",memory_type="vram",start=0})
assert(vram.max_instructions==100000000)
e=setup(); e.next_bp_id=9007199254740991
assert(not e.handlers.watch_register({}) and not e.handlers.set_breakpoint(points[1]))
assert(e.added==0 and e.next_bp_id==9007199254740991)
print("Mesen aggregate breakpoint admission, callback rollback and full-queue expiry passed")
