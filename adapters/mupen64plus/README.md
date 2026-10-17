# Mupen64Plus N64 adapter

This adapter pins the upstream 2.6.0 source bundle, builds the core
with debugger support, and keeps build products under `work/`. Standard N64 cartridge images do
not require a BIOS. N64DD is outside this adapter's current scope.

Build the pinned bundle and verify its structural PoC:

```sh
./adapters/mupen64plus/build.sh
./adapters/mupen64plus/poc/inspect.sh
```

The bundled `m64p_test_rom.v64` is the upstream project's GPL test ROM. Its digest is pinned in
`upstream.lock`; the ROM and all generated binaries remain in the ignored work tree. The local
core patch makes savestate and worker initialization fail closed. Its digest is also pinned, and
the build restores every patched source from the verified archive before applying it.

The MCP `launch` path validates the build metadata against `upstream.lock` and gives each launch
generation an emucap-owned configuration directory. Both modes load the pinned SDL input, RSP and Rice video plugins;
`display: true` makes the rendering window visible. The frontend writes its controller and
screenshot configuration only inside that runtime directory.

The common surface is status and ROM identity, pause/resume, synchronous reset, R4300 state and
exact instruction stepping, bounded RDRAM reads/writes while frozen, port-0 `set_input`,
R4300 exec/read/write breakpoints with hit-time evidence, event polling, and disassembly. An empty
input set returns native input ownership. Port 0 advertises raw signed `stick_x` and `stick_y`
axes (-128..127). Nonempty controller state overrides native input as one complete poll response;
explicit zero axes hold neutral. Persistent state survives load/reset. Both managed display modes
support `tap` with buttons and axes, restoring the preceding input state on completion or cancellation. Frozen sessions
also expose a bounded, best-effort R4300 call stack reconstructed from native control-transfer
history. It reports completeness and dropped history explicitly; it never mixes RSP frames into the
main-CPU result.

Both modes expose frozen `read_memory_batch` over RDRAM and `debug` operation `execution_speed`,
which sets and reads back the core's speed factor (1–1000 percent) and speed limiter. Core patch
0003 sleeps the limiter wait in 10 ms slices that end on a pause or speed change.
The maintained host uses ABI generation 8 and requires the native controller-input, poll, and state-boundary callback exports; the pinned patch set also includes debugger integer-type
and MinGW link fixes. Actual frame/debugger parks
reanchor pacing on resume. New native snapshots preserve interrupt-jitter state; native M64P
file loads validate and retain their bytes before releasing the guest. Rejected inputs preserve
the frozen state. Legacy snapshots cannot restore an absent jitter history; PJ64 and slot
imports retain their existing behavior. RDRAM observations use a bounded storage-only native
copy at the existing frozen boundary, preserving read watchpoints and debugger state.
Rebuild the host and bridge together.

Both visible and headless launch advertise rendered-frame stepping, bounded `run_frames`,
`press_buttons`, PNG screenshot, native save/load, and Control-composed atomic state/frame/memory
`probe`. Its callback barrier freezes inside the
exact Mupen64Plus frame callback and rearms before releasing the preceding frame, so multiple
display lists in one R4300 task cannot escape between requests. The adapter does not combine that
barrier with the core's separate frame-advance pause. Readiness remains false until the
first rendered callback, so a caller cannot pause renderer initialization and move its latency
into the first frame request. Frame and instruction counts use the shared 5,000-operation
admission limit and a 250-second total backend budget, below the outer deferred-request deadline.

Screenshot and state requests are asynchronous upstream commands. Screenshot completion is
reported by the video plugin after the core's display-list callback, so capture does not block the
first callback while waiting for work that can only finish later. It waits for the native
completion notification, freezes at the first following callback, and reports both the frame
before the request and that post-completion boundary. Those values may differ by more than one;
ordinary `step(frames, 1)` still requires exactly one callback. Save/load use the native state
boundary to request a debugger pause and wait for both the completion result and the observed
paused state within the operation deadline. A plain frame-operation failure is recovered to a
debugger instruction boundary before the error response. If a scheduled screenshot or state operation, or
transient input cleanup, cannot be closed, the dedicated launch generation is stopped rather than
carrying an unowned effect into a later request. A save is first written to a unique sibling and
then published through rollback-aware replacement. Loading invalidates the pre-load frame
sequence and resynchronizes it at the next rendered callback. State completion itself does not
advance to that callback. Responses keep input-poll, VI and rendered-frame counters separate.

The pinned core and plugin bundle completed a clean Clang `scan-build` run with no analyzer
reports. Compiler warnings in the excluded N64DD and Transfer Pak paths remain outside this
cartridge profile; this result is not a claim that those families are ready. The upstream test ROM
passed the maintained control smoke after this ownership correction. A representative commercial
cartridge passed three consecutive cold-launch runs. The smoke covers
consecutive exact frame steps, return to an R4300 instruction boundary, persistent-input
restoration after a bounded pulse, `set_input([])` ownership release, PNG identity, a non-empty
native state, RDRAM restoration after load, immediate post-load PNG completion, and exact frame
step after that capture. This proves the native controller response path and cleanup, but it does not
substitute for manual native-keyboard testing in every window environment.

Preparation is rollback-owned. If configuration, ROM loading, plugin startup/attachment, or
callback registration fails after `CoreStartup`, the frontend detaches every attached plugin,
closes the ROM, shuts down the core, and unloads each library in reverse order. Dropping a prepared
frontend before execution follows the same closure path.

Headless launch uses the same Rice renderer with a hidden window. Linux requires an OpenGL
context (for example Xvfb); display:false controls window visibility. Frame, poll and instruction
stepping remain available. Headless save/load waits for native completion and a debugger stop
acknowledgment; results expose any poll/VI advance. RSP state remains unadvertised.

For poll-based TAS, use `tap(buttons=[], axes={stick_x:37,stick_y:-90},
timing={unit:"input_polls",press:1,after:0})`, or `step(count=1,unit="input_polls")` for neutral
advance. `status.input_poll_capability` defines the port and boundary. A poll commits the PIF
response before SI DMA and guest copying; RAM input structures can still contain the preceding
sample at this boundary. `debug.read_input_polls` returns final controller words with an exclusive
sequence cursor, generation identity and explicit ring-buffer gaps. The 4096-entry history and
counter continue across load/reset and restart with the launch generation.

### Native compatibility and build identity

`M64P_HOST_API` identifies the published host ABI generation. Keep it unchanged for
additive exports and implementation fixes. An incompatible change to an existing
interface requires a new generation; unpublished iterations share that generation.

The loader checks required exports and each extension validates its own ABI argument.
Managed launch separately matches the bundle and patchset digests in `upstream.lock`.
A matching ABI generation therefore does not substitute for the required native build.
Commits and digests identify development builds.

On Windows, `build.sh` stages the transitive MinGW runtime DLLs beside the core and plugins. This
matches the host's safe DLL search policy and keeps the generated native bundle runnable without
adding a toolchain directory to the emulator process search path.
