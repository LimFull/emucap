# MAME Taito F2 adapter

This adapter accepts one explicit profile, `taitof2` (aliases `taito_f2`, `taito-f2`), for the
games in MAME's `taito/taito_f2.cpp` driver. A `.zip` is never auto-inferred as Taito F2.

Build the dedicated pinned MAME subset and the shared 68000 bridge:

```sh
adapters/mame-taitof2/build.sh
cargo build --locked --release --bin emucap-mame-neogeo-bridge
```

The build owns `adapters/mame-taitof2/work` and does not reuse or replace the PC-98 or Neo Geo
subsets. It applies the [shared MAME patch stack](../mame-pc98/README.md) and reuses the Neo Geo
68000 bridge and Lua plugin with the `taitof2` profile. `EMUCAP_TAITOF2_MAME_BIN` selects another
maintained MAME binary; a stock MAME lacks the required native hooks. No release package is
published for this adapter yet.

## What the user must provide

Taito F2 boards have no BIOS. The game set is a MAME `.zip` whose stem is the driver name, for
example `pulirula.zip`; it is never copied into this repository. A clone such as `pulirulaj.zip`
also needs its parent set in the same directory, which is the only rompath the launcher uses. Check
a set against the pinned build before launching:

```sh
adapters/mame-taitof2/work/mame -rompath /path/to/roms -verifyroms pulirula
```

## Supported surface

The MCP launch path uses an emucap-owned MAME home, is headless by default and opens a window only
for `launch(display:true)`. It provides:

- machine-global pause/resume selected through the 68000 main CPU, 68000 state, instruction step,
  exact frame step and run-frames;
- one memory type, `program`: the complete 24-bit 68000 program space (16 MiB) read through MAME's
  side-effect-free debugger view. Work RAM moves between games (for example `0x100000`,
  `0x200000`, or `0x300000` for PuLiRuLa), so the adapter exposes absolute addresses instead of
  guessing one RAM window. Writes go through the debugger write path; writes to I/O registers have
  their hardware side effects;
- pausing exec/read/write breakpoints with public IDs, hit-time registers and bounded snapshots;
  exec breakpoints accept `memory_type` `cpu` or `program`. Also one-shot event polling,
  disassembly, a best-effort native call stack and `read_memory_batch`;
- frozen-frame PNG capture, `debug.execution_speed` and native MAME save/load at a settled scheduler
  boundary, with the same contract as [Neo Geo](../mame-neogeo/README.md);
- player-one `button1`..`button4`, `start`, `coin`, `service` and directions on port 0, named after
  MAME's `P1 Button N`. `status.input_buttons.available` lists the inputs the running game has.

Taito F2 renders palette-indexed frames. Patch `0020` extends native raster-state preservation to
indexed screens and registers the Taito F2 screen bounds, so a load restores the presented frame
without advancing guest time. The driver's screens have no VBLANK period; owned frame steps count the
boundary whose notifier is running, so one requested frame stops after exactly one counter increment.

Not exposed: the Z80 sound CPU, player-two input, dials or paddles (Cameltry, Drift Out) and the
Mahjong Quest keyboard.

## Verification

macOS ARM64 with MAME 0.289 and PuLiRuLa (`pulirula`): headless launch, exact 60-frame steps,
coin and start input from the attract mode into gameplay, native save/load with an identical
post-load screenshot and work RAM, an identical screenshot after replaying 360 frames from that
state, program-space writes, a work-RAM write watchpoint, an exec breakpoint on the IRQ5 handler,
disassembly and reset. Other games, Windows and Linux remain unverified.
