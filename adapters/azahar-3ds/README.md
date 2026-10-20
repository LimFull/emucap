# Azahar Nintendo 3DS adapter

This adapter accepts one profile, `3ds` (aliases `n3ds`, `nintendo-3ds`, `azahar`), and infers it
from `.3ds`, `.cci`, `.cxi` and `.3dsx` content. Titles must be decrypted; encrypted dumps and CIA
installers are rejected. It is maintained in this fork only.

Build the pinned Azahar libretro core and the direct host:

```sh
adapters/azahar-3ds/build.sh
cargo build --locked --release --bin emucap-azahar
```

`build.sh` verifies the pinned Azahar unified source release, applies `patches/` to a fresh
extract, builds the libretro core with only the software renderer, and writes
`work/emucap-azahar-build.json`. Launch admits a core only when that identity matches
`upstream.lock` and the core digest. The build needs CMake, Ninja and a C++20 toolchain; the
source release carries its other dependencies.

## Architecture

`emucap-azahar` loads the core with `dlopen` and owns the only `retro_run` loop, the pattern of
the NP2kai backend. One `retro_run` ends at the next presented VBlank, and every request is
serviced between frames, so memory, registers and state are read while no guest code runs.
Patch `0001` adds a small C ABI to the core for application memory, registers and the title ID.
Patch `0002` adds the debugger: the core always runs the dyncom interpreter, which consults the
emucap breakpoint state before every instruction and on every load and store. A hit or a finished
step ends `retro_run` at that instruction boundary, and the remaining cores catch up on the next
run. In 3D scenes the software renderer dominates frame time, so the interpreter costs about 15%
there; 2D scenes still run above real time.
The launch generation receives a read-only copy of the core and its own system and save
directories; a user's Azahar or RetroArch profile is never read.

No BIOS or console system files are required for titles that boot with Azahar's HLE services.

## Supported surface

- launch (headless only), pause/resume, exact frame step and run-frames, reset;
- `virtual` memory: the application's 32-bit virtual address space. Reads and writes reject any
  range containing an unmapped page; writes invalidate JIT code for the range.
  `read_memory_batch` reads up to the shared batch limits at one frozen boundary;
- `get_state`: r0-r15 and CPSR of the application core (core 0) at the frame boundary;
- buttons `a b x y l r zl zr start select up down left right` and the Circle Pad as
  `circle_up/down/left/right` at full deflection, held with `set_input`, pulsed by `tap`;
- touch on the 320x240 bottom screen through `hold_touch`, `release_touch` and
  `pulse_touch_while_running`;
- frozen screenshots of the top screen above the centered bottom screen (400x480);
- frozen save/load through `retro_serialize`, bound by a sidecar to the exact title, core, patch
  set, frontend build, OS and architecture. A screenshot after load needs one frame step;
- host pacing through `debug.execution_speed`;
- exec, read, write and access breakpoints on `virtual` ranges, pausing or recording only. Exec
  hits stop before the instruction; data hits stop after the accessing instruction and report
  the address, size, value and access-time registers. Pausing hits can capture up to 16
  `virtual:address:length` snapshots (16 KiB) at the frozen boundary. Points match only the
  title's process; DMA, GPU and HLE service copies into its memory are not observed. Value and
  PC conditions are rejected;
- exact instruction steps with `step(unit="instructions")`, executing the instruction under a
  breakpoint on resume;
- ARM and Thumb disassembly from the dynarmic decoder; 32-bit Thumb is decoded only for BL/BLX;
- a best-effort call stack of PC and the link register; 3DS code rarely keeps frame pointers.

A breakpoint stop can land mid-frame. The next frame advance completes that frame, and save/load
at such a stop restores the exact instruction boundary.

Not exposed: audio output, display, the HOME button, the C-Stick, gyroscope and microphone.

## Verification

macOS ARM64 with Digimon World Re:Digitize Decode (Japan, decrypted `.3ds`): launch from
`launch_plan` inference, exact 120-frame steps at about real time, Start and A input through
`tap`, code and heap reads, an unmapped-read rejection, application registers, a heap write and
batch read, and touch at exact bottom-screen pixels. Save/load restored an identical screenshot
and heap one frame later, and replaying 301 frames from the state reproduced the same screenshot.
The debugger stopped on an exec breakpoint in game code and re-hit it after a step and resume,
stopped a stack store whose disassembly, address and value matched the event, recorded 233
non-pausing hits over 30 frames, and captured snapshots. Save/load at a mid-frame breakpoint
restored registers, an identical five-instruction step trace and identical heap 30 frames later.
Other titles, Windows and Linux remain unverified.

Azahar's contribution policy prohibits AI-generated submissions. The patch here is for this
adapter only and must not be submitted to Azahar.
