# emucap — PPSSPP (PSP) adapter

The adapter that gives emucap PlayStation Portable support. Headless PPSSPP already exposes an
external debugger over its own JSON WebSocket (`debugger.ppsspp.org`); the emucap **PSP bridge**
(`emucap-ppsspp-bridge`) is a pure WebSocket client that relays it to the emucap wire protocol. A
small repo-owned patch stack adds missing lifecycle and exact-time hooks: savestate, screenshots
while running or CPU-halted, exact VBlank frame stepping, and trustworthy stop provenance.

## Architecture

```
emucap Core ──emucap protocol (TCP)──▶ emucap-ppsspp-bridge ──WebSocket (JSON)──▶ PPSSPPHeadless
                                                          ws://127.0.0.1:<debugger port>/debugger
                                                          subprotocol: debugger.ppsspp.org
```

- No GDB stub, no per-CPU split — PPSSPP has one CPU context and a single JSON WebSocket endpoint
  (`Core/Debugger/WebSocket.cpp`). A request is `{"event": "<name>", ...params}`; the reply reuses
  the event name; PPSSPP also emits spontaneous events (log lines, breakpoint/stepping hits)
  unprompted, so the bridge queues anything it wasn't waiting for.
- The adapter applies source patches to pinned upstream PPSSPP. Upstream supplies CPU-thread
  debugger dispatch, instruction-step sizing, stop reasons, debugger port configuration and the
  current texture-cache replacement path. The maintained patches add:
  - `savestate.save`/`savestate.load`, ticketed headless reset and CPU-halted output readback.
    Frozen state operations acknowledge both native completion and the RAM-writer park.
  - Connection-owned terminal receipts for instruction queues and running debugger plans.
    Stale broadcasts cannot complete a later request. Disconnect and host deadlines retire
    the owning plan; an unverified cleanup terminates the dedicated producer.
  - Exact PSP VBlank stepping, partial progress on interruption, deferred responses and
    same-connection `operation_id` cancellation. `emucap.frameStep.capability` reports protocol v1;
    the bridge negotiates a four-second cleanup budget. The original ticket carries the
    verified terminal and request-owned input is released on cancellation or disconnect.
  - Batched reads on the CPU owner behind the asynchronous RAM-writer fence, plus one native
    pacing owner shared by agent speed, debugger overrides, fast-forward and custom/analog limits.
  - Loopback binding, bearer authentication, required managed origin and exact-port admission.
    The GUI and headless frontend honor `EMUCAP_PPSSPP_MEMSTICK` before creating save directories;
    the GUI applies it before config loading, including macOS custom memory-stick preferences.
  - Headless breakpoint maintenance and optional SDL discovery so a headless-only build remains
    available when SDL3/SDL3_ttf are absent. Managed headless output loads the guest-dialog atlas
    while retaining the native headless execution mode.
- `cpu.stepInto`/`stepOver`/`stepOut`/`runUntil`/`nextHLE` ack via a *differently named* spontaneous
  `cpu.stepping` event rather than a reply of their own name (`SteppingSubscriber.cpp`) — the bridge
  handles this with a send-then-wait-for-a-different-event primitive, not a naive call/reply demux.

## Build

On Windows, run `adapters/ppsspp/build.ps1` with Visual Studio 2022 C++ tools and
CMake. It builds `work/ppsspp/build-headless/Release/PPSSPPHeadless.exe`, stages
runtime assets beside it, and writes build metadata. `-Jobs` sets parallel build
jobs (default 2). `EMUCAP_PPSSPP_WORK` selects an empty or emucap-owned work root;
`EMUCAP_PPSSPP_SRC` supplies a read-only clone origin. The native Windows Headless
target links its Media Foundation GUID and common-control dependencies explicitly.

On macOS or Linux:

```sh
adapters/ppsspp/build.sh
```

Clones `hrydgard/ppsspp` into an emucap-owned work tree (`adapters/ppsspp/work`) pinned to a known
commit, applies the patch stack, and builds **two targets** with CMake (`-DHEADLESS=ON`, one
configure): `PPSSPPHeadless` (the default headless debugging build) and `PPSSPPSDL` (the GUI build
for HITL `display:true`). `HEADLESS=ON` adds the headless target *alongside* the default desktop SDL
target — they are not mutually exclusive, so both come out of the same patched tree and both carry
the identical source patch stack. The patch also adds new source files, so build.sh resets tracked
files to the pinned commit and drops the patch-created untracked sources before re-applying, making
every build idempotent. `EMUCAP_PPSSPP_SRC=/path/to/ppsspp` supplies a read-only upstream checkout to
`git clone` locally (skips the network) instead of `hrydgard/ppsspp`; the pinned checkout, patch
stack, and build still happen only in the emucap-owned work tree, so the supplied checkout is never
patched or built in.

`PPSSPPHeadless` is the **guaranteed default target** — if it fails to build, build.sh fails.
`PPSSPPSDL` (the HITL `display:true` window) is built **best-effort**: it additionally needs SDL3
(`brew install sdl3 sdl3_ttf`), so on a host that has only the headless prerequisites its target is
allowed to fail without failing the whole build — build.sh prints a warning that headless is ready
and SDL3 is needed for `display:true`, and exits successfully. Set `EMUCAP_PPSSPP_REQUIRE_GUI=1` to require both targets; release CI uses this mode. Set `EMUCAP_PPSSPP_BUILD_GUI=0` to
skip the GUI target entirely (headless-only). On macOS the GUI output is an `.app` bundle
(`build-headless/PPSSPPSDL.app`), whose executable emucap launches directly; it runs under the
adhoc-signed JIT from a rebuilt bundle (no crash) and boots a commercial ISO in a Vulkan window.

- Dependencies: `cmake`, a C++ toolchain, and `git` (the clone pulls submodules, including bundled
  FFmpeg).
- On macOS it builds with Apple clang (`/usr/bin/clang`) — build.sh normalizes a homebrew-LLVM
  environment.

## Launch

The agent brings it up with `launch(content_path=<game.iso|game.cso|game.pbp>, system="psp")` (the
MCP tool). Internally:

```sh
adapters/ppsspp/launch.sh <game.iso|game.cso|game.pbp> <EMUCAP_PORT> [EMUCAP_NAME]
```

This is a two-process launch (headless PPSSPP + the bridge), the same shape as DeSmuME NDS:

- `PPSSPPHeadless --debugger=<port> --graphics=software <content>`. The content is a **positional**
  boot argument — `-m`/`--mount` alone only mounts a *second* image on `umd1:` for ELF+CSO test
  harnesses and leaves the boot list empty. `--timeout` (a headless test-harness flag that aborts
  the run after N wall-clock seconds regardless of debugger activity) is never passed, so the run
  has no deadline.
- `--debugger` forces `startBreak=true` — **PPSSPP halts before boot**; `resume` to run.
- No display, no window, no GPU backend needed (`--graphics=software`) — the default headless path is
  truly headless, so no `caffeinate`/display-awake handling is required.
- No PSP firmware/BIOS is needed to boot a commercial ISO/CSO/PBP.

### HITL display mode (`display:true`)

`launch(content_path=<...>, system="psp", display=true)` opens a **real PPSSPP window a human sees
and plays** while the agent drives the same debugger WebSocket — this is emucap's human-in-the-loop
(HITL) core purpose. It launches the `PPSSPPSDL` GUI build instead of `PPSSPPHeadless`, with the same
positional-content boot and the same reserved `--debugger=<port>` handed to the bridge; the GUI
honors that exact port through upstream debugger configuration. Differences from headless:

- **A human plays with PPSSPP's own input.** The human uses PPSSPP's built-in keyboard/gamepad
  mapping (Developer/Controls settings inside the window) — keys go straight into the emulated PSP.
  The agent uses `tap` for timed input, including long holds and simultaneous buttons;
  human input uses the same PSP pad. On macOS, host keyboard input requires the window to be focused and the macOS **input
  source set to a Latin/English layout** — a Hangul/CJK IME intercepts keystrokes before PPSSPP sees
  them (same gotcha as the NDS HITL notes); the mouse/gamepad are unaffected.
- **The game boots running, not halted.** The GUI does not set `startBreak`, so the game runs
  immediately for the human — no initial `resume` needed. The agent attaches to a running core and
  can `pause`/`resume`/read/inject at will.
- **A GPU window (Vulkan/GL), not software.** `--graphics=software` is omitted so the window renders
  on the real backend. On macOS a `caffeinate -d -w <pid>` keeps the display awake for the window's
  lifetime (the window dies if the display sleeps — same as the Flycast/Mesen GUI adapters).
- **Same debugger surface.** The bridge connects to the GUI debugger through the same
  protocol. Hardware-renderer restore has the separate boundary described below.
- **Isolated per-session profile — never the user's real PPSSPP.** The launcher points the GUI at an
  emucap-owned launch-generation run dir via `HOME`/`XDG_CONFIG_HOME` and the fork's `EMUCAP_PPSSPP_MEMSTICK`
  (patch 0006). All config, saves, and control mappings the human sets in the window land in that
  disposable dir; the operator's real PPSSPP config/saves are never read or overwritten — verified
  live on macOS to beat a `NSUserDefaults`-configured custom memory stick. So the Developer/Controls
  settings above are per-session and do not persist into the user's own PPSSPP.

**ESC pause-menu gotcha (known follow-up).** When the human opens PPSSPP's own **ESC pause menu**, the
debugger WebSocket goes unresponsive — even a bare `version` request times out — until they close it.
The ESC menu calls `Core_Break(BreakReason::UIPause)` and pushes the pause screen, which backgrounds
the emulation screen so the core run loop stops being pumped; the WebSocket cannot service commands
in that state. So the agent **cannot observe a state the human froze with the ESC menu**. Workaround:
to inspect a paused state, the human should use the **game's own in-game pause** (or ask the agent to
`pause`, which uses `cpu.stepping` and keeps the WebSocket answering) rather than the emulator's ESC
menu; `resume` continues. This is PPSSPP-GUI-specific — the headless build has no pause menu, and NDS
`display:true` (a bare DeSmuME framebuffer window) has no host-level pause menu either, so neither
shares it. Keeping the debugger WebSocket answering read-only commands (`cpu.getAllRegs`/`memory.read`
/`memory.disasm`) while the ESC menu is open is a possible future fork change.

## System and content

- System name: `psp` (aliases `ppsspp`, `playstation-portable`). Content extensions: `.iso`,
  `.cso`, `.pbp`. `.iso` is shared with Saturn/PSX/PCE/MD/Dreamcast — a PSP GAME ISO9660 header
  disambiguates automatically; pass `system="psp"` explicitly if that fails.

## memory_types

`status.memory_types` is authoritative. The current profile exposes:

| memory_type | meaning |
|---|---|
| `main` | PSP user RAM, base `0x08800000` (`Core/MemMap.h: PSP_GetUserMemoryBase()`). `read_memory`/`write_memory`'s `address`/`start` offset is added to this base. |

`disassemble` and **exec** breakpoints take a raw absolute PSP address instead (no `memory_type` base
added) — e.g. straight from `get_state`'s `cpu.pc` (`memory_type` is ignored for an exec breakpoint).
**read/write** breakpoints resolve their `address`/`start` the same way `read_memory`/`write_memory`
do — a `memory_type` region offset added to the base — so a watchpoint lands where `read_memory`
reads.

## Buttons

`a`→cross(✕), `b`→circle(○), `x`→square(□), `y`→triangle(△), `l`→ltrigger, `r`→rtrigger, plus
`start`, `select`, and the d-pad `up`/`down`/`left`/`right` — mapped to PPSSPP's own PlayStation-style
button names. **Confirm/cancel is game-defined**: Japanese titles typically confirm with circle (`b`)
and cancel with cross (`a`), the opposite of the common Western layout — so a menu may look
unresponsive to `a` while `b` advances it. PPSSPP also has `home`/`screen`/`note`/`hold`/`wlan`/... and
an analog stick; this PSP profile does not advertise them. `tap` accepts axes on profiles
that advertise axis capabilities in full `status`.

## Tool availability

**Upstream debugger operations**: `read_memory`/`write_memory`
(`memory.read`/`memory.write`, base64 on the wire), `dump_memory` (streams each region — today
`main`, user RAM — via `memory.read` in 128 KiB chunks to `<name>.bin` + `regions.json`, plus a
`state.json` register snapshot, the same `.bin`/`regions.json`/`state.json` bundle the cross-ROM
diff loader consumes for every adapter), `find_pattern` (a bounded `main`-memory scan returning
region-relative offsets), `get_state` (`cpu.getAllRegs`'s GPR category
flattened to `cpu.<name>`), `disassemble` (`memory.disasm`), `set_breakpoint`/`clear_breakpoint`/
`list_breakpoints`/`clear_all_breakpoints` (`cpu.breakpoint.*` for exec, `memory.breakpoint.*` for
read/write, both with an optional `condition` expression), `step(unit="instructions")` (the MCP
maps this to the bridge's `step_instructions` wire method, which repeats `cpu.stepInto` because
PPSSPP has no step-count parameter), `pause`/`resume` (`cpu.stepping`/
`cpu.resume`), `poll_events` (draining PPSSPP's spontaneous `cpu.stepping` events), `set_input`/
`press_buttons` (`input.buttons.send`/`input.buttons.press`), `reset` (`game.reset`), and
`get_rom_info` (`game.status` for id/title + a locally computed sha1 of the `EMUCAP_CONTENT` image
— PPSSPP's WS API never exposes a content path or hash itself).

**Maintained native operations** (source patches extend upstream debugger commands):

The bridge retains at most 128 breakpoint IDs, counting duplicate logical registrations.
Clearing an ID releases its slot; a shared native breakpoint remains until its last bridge ID is
cleared. Native WebSocket CPU/memory additions also share a 128-entry limit checked on the CPU
thread. Replacing an existing native identity remains available at capacity. GUI-created entries
count toward this native admission limit; temporary stepping and the fixed GPR set are separate.

- `screenshot` — `emucap.screenshot`. Patch `0012-halted-output-readback.patch` reads the existing
  PSP display framebuffer on the emulation thread while preserving a CPU halt.
  Like upstream game screenshots, it crops to 480×272 at native scale, excluding host UI and
  postprocessing. Reading the host swapchain here could instead return a newly cleared UI frame. It does not resume, step, or schedule
  a VSYNC for a halted CPU. Running captures retain the next-VSYNC GPU halt path. Each read owns
  its copied buffer through encoding, including memory-backed display buffers; busy requests, changed stops, unavailable output, and timed-out
  reads fail rather than returning a previous result. GPU teardown/recreation invalidates pending reads. Queued timeout work is discarded; an
  already-started host read may finish into its private buffer, which is not returned or reused.
  Readback waits at most 5s; running capture may first wait another 5s for GE halt. The bridge
  uses a 12s response budget. The image is existing display-buffer content, not a newly presented frame at the stopped
  CPU instruction. Unbuffered hardware rendering that exposes only a host backbuffer is rejected.
- `save_state`/`load_state` — `savestate.save`/`savestate.load`. The fork's handler breaks the CPU
  into stepping if it is running and waits for native completion. A completed save restores
  the prior run/halt state; a load returns `state: frozen` from the EmuThread and stays stopped.
  GPU-stepping entry is rejected before enqueueing. A timed-out operation is not cancelled and
  does not automatically resume; its eventual effect remains unresolved until native completion.
- `step(unit="frames")` — `emucap.frameStep` advances exactly the requested number of emulated PSP
  VBlank-start boundaries and returns frozen. A breakpoint or other native stop returns
  `interrupted` with the completed count. The MCP `tap`, `hold_until`, `regression_run`, and
  `verify_determinism` composites are admitted on top of this exact clock and native savestate.
- `probe` — the Rust bridge halts first, then composes one native state load, exact frame advance,
  and frozen memory read without another client request interleaving. Halting first prevents the
  load acknowledgement from restoring a previously running CPU between restore and step. It uses
  the same fork commands as the preceding two items and does not require another host hook.

**Debugger data with an emucap contract**:
- `call_stack` — frozen-only `hle.backtrace`, bounded to 256 frames. PPSSPP derives the stack from
  MIPS code analysis and thread entry/stack bounds; optimized or unusual prologues can end the walk
  early, so `status.contracts` advertises `debug.call-stack="best_effort"` rather than an
  authoritative unwind.

**Profile boundaries (`status.methods` is authoritative)**:
- raw wire `run_frames` — exact frozen-terminal frame `step` is the supported time primitive.
- `watch_register`, `set_trace`/`get_trace`, and `break_on_reset` have no native hook in this
  profile. They are not advertised as future methods; callers discover only working operations.
- Structured breakpoint value-conditions (`value`/`value_mask`/`value_len`) — unsupported; use a
  raw `condition` expression (PPSSPP's own expression language) instead, or
  `pc_min`/`pc_max`, which the bridge compiles into one automatically.
- BP kind `nmi`/`irq`/`dma`, range exec breakpoints (`start` must equal `end`), and
  `auto_savestate`/`snapshot` breakpoint options.

## Batched memory and execution speed

Batch payloads are acquired by one native command into a private response buffer. The bridge
validates its full length before exposing any ranges. Native stop epochs remain stable across
repeated reads and pacing queries at the same halt. A pacing failure after an attempted mutation
terminates the control channel with the last verified policy. Follow the recovery action in
`status` before issuing further commands. It does not claim that the emulator process exited or
that guest progress was undone.


`read_memory_batch` reads up to 64 ranges (64 KiB) of `main` while the CPU is halted.
`debug.execution_speed` sets an integer 1–10000 percent of the PSP refresh or unlimited through
the native `emucap.pacing`; fast-forward reads back as unlimited, and a debugger, custom or analog limit or
netplay's forced rate as `custom`. Headless debugger sessions are paced (launch default 100
percent); frame waits sleep in 10 ms slices that end on a debugger break or a pacing change, and
a resume does not owe the halted time. `status.frame` is the VBlank clock, and a frame step that
reaches the native 240 s budget returns `reason: "host_deadline"`.

## Operational notes

- **Hardware-renderer restore**: the first captures after a displayed state load can differ
  because native states omit hardware framebuffer contents. Use software headless mode for
  workflows that need matching immediate and consecutive post-load captures.
- **Halt-on-start**: headless `--debugger` starts at the entry point; GUI launch normally runs
  and managed frozen launch pauses through the bridge. Native GUI debugging can select `-s`.
- **Stop provenance**: receipts retain PPSSPP's native reason, related address and breakpoint hit.
  `poll_events` uses those fields first; PC and hit-count matching remain compatibility fallbacks
  for older producers. Simultaneous fallback-only memory hits remain best-effort.
- **Ack-name mismatch**: `cpu.stepInto`/`stepOver`/`stepOut`/`runUntil`/`nextHLE` ack via a
  `cpu.stepping` event, not a reply of their own name — a naive client that blocks for a reply
  literally named `cpu.stepInto` hangs forever.
- **`press_buttons` needs a running emulator**: PPSSPP's timed press only auto-releases after
  enough emulated frames elapse, which never happens while the CPU is halted for the debugger —
  halted calls are rejected up front rather than hanging. `set_input` (a held state, not timed)
  works regardless of run/halt state.

### Pinned Linux desktop dependencies

Release builds prepare SDL3 and SDL3_ttf from `sdl.lock` with `build-sdl.sh PREFIX WORK`.
The helper verifies source archive hashes and installs into the supplied prefix. Pass that prefix
through `CMAKE_PREFIX_PATH`, its `lib/pkgconfig` through `PKG_CONFIG_PATH`, and its `lib` through
`LD_LIBRARY_PATH` when building/running locally. The release packager collects the shared libraries
and their installed license notices. A headless-only local build can still use
`EMUCAP_PPSSPP_BUILD_GUI=0` with the GUI requirement unset.

Linux GUI guest dialogs use Fontconfig fallback fonts. Install fonts for the game's language;
Debian/Ubuntu's `fonts-noto-cjk` supplies Japanese, Korean and Chinese glyphs. These are runtime
system fonts, separate from the SDL libraries collected into the native package.
