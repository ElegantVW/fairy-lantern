# AGENTS.md — fairy-lantern

GBA emulator in Rust. Target ROM for audio work:
`/tmp/opencode/Pokemon Liquid Crystal (v3.3.00512).gba`
(originally reported: static 3-tone drone instead of intro music).

## Status: RESOLVED (uncommitted working tree)

The game now boots and plays normally. Verified at 4000 frames:
- Boot completes ~frame 197 (~3.3 s); silence during boot.
- Intro (custom "Smsh" driver) plays rich, evolving audio + rendered scene;
  brief silent gap ~frames 339-377 (scene/phase change).
- By ~frame 3749 the song system is fully active: song processor
  `0x081DCA2C` writes the 6 song structs (`Tmsh`/`Smsh` sentinels),
  `0x081DD97C` song-update runs, `0x081DD63C` command routine called,
  audio at the end = dense music spectrum (not a drone).
- FIFO audio path works: DMA1/2 (special timing) refill 4 words at a time
  from the mix windows `0x030062A0` (ch1) and `0x030068D0` (ch2).

Root cause of the old drone: game hung in boot-time waits — timer delay
`0x081E34FC` (TM readback never advanced), flag spin `0x080008AA`
(`[0x0300010C]&1` never set), flash-detect. Fixed by the UNCOMMITTED diffs:
- `src/timers.rs`: per-channel prescale `frac` accumulator + true
  reload→overflow cascade (`tick_channel`, `cascade_next`, multi-overflow cap).
- `src/irq.rs`: proper IRQ entry (SPSR/LR bank, LR=PC+4), BIOS IRQ HLE
  (stack frame + handler via `0x03007FFC`, epilogue `BIOS_IRQ_RETURN`),
  IntrWait mirror at `0x03007FF8` latched in `raise()`.
- `src/dma.rs`: `on_fifo_request` — 4-word refills into the sound FIFO
  (DMA1/2 special timing), correct count handling.
- `src/battery.rs`, `src/bios_hle.rs`, `src/cpu/*`, `src/bus.rs`,
  `src/savestate.rs`, `src/ppu/*` also modified (still uncommitted).

Persisted flash save: `~/.local/share/faeos/fairy-lantern/roms/Pokemon
Liquid Crystal (v3.3.00512).sav` (128 KB, initialized Aug 10).

## Build / run (always)

```
cargo build --release
FAIRY_DMA_TRACE=1 ./target/release/fairy "/tmp/opencode/Pokemon Liquid Crystal (v3.3.00512).gba" --frames N 2> log
```

Audio goes to a wav file (rolling ~1 s capture); `--help`/`src/main.rs` for
all CLI options. Last frame rendered to `/tmp/fairy-lantern-last.ppm`.

## Debug instrumentation (all gated on env FAIRY_DMA_TRACE=1)

- `src/cpu/mod.rs:172` FRAME: per-instruction pc dump while `dbg_evt % 28 == 0`
  (pc reveals stuck loops).
- `src/cpu/mod.rs:184` DRV: entry-counter + first-6/every-100th print of the 9
  driver entry points; `:203` CMD (0x081DD640), `:216` SST (0x081DE638/6DE),
  `:228` WINDOW/TRACK/MIXTRK (0x081DC088), `:273` REARM (0x081DC9D4),
  `:290` SPROC (0x081DCA20), `:310` DISP; `:232` DEC (0x03002BD4 — IWRAM
  sample decoder, copied from ROM `0x081DC398`; intro audio stream).
- `src/bus.rs` SLOTWR (0x030028E0), MIXTRKW (voice tracks 0x03005FA0..),
  BUFWR (mix window 0x030062A0), SONGWR (6 songs' +4/+0x34), S28WR,
  FIFOW (0x040000A0 writes).
- `src/dma.rs:119/134` fifo refills; `src/dma.rs:133` `dbg_evt += 1`
  (evt counter = FIFO refills only; frozen evt = sound stopped feeding).
- `src/emu.rs` WINDIFF frame-window diffs; FAIRY_DUMP_IWRAM dumps the mix
  window `0x030062A0..0x030068C0` at end of run.
- The RUN trace (`src/cpu/mod.rs`) prints pc-run transitions for
  `evt 8380..8780` — used to find the IWRAM decode loop at
  `0x03002BEC..0x03002C54` (fixed-point delta decoder, ~224 samples/call).

## Sound driver facts (Pokemon Liquid Crystal)

- SoundInfo = `0x03005F50`; pos = `[0x03005F50]`, processor = `[0x03005F70]`,
  second fn = `[0x03005F78]`, voice tracks at `0x03005FA0 + 0x50*i`.
- Sentinel `0x68736D53` ("Smsh"); gate = `[song+0x34]`.
- Driver main `0x081DC020`; song processor `0x081DCA20` (runs all 6 songs
  chained via `[song+0x38]`); re-arm `0x081DC9D4` (game timer0 IRQ handler
  `0x0800080C`); song update `0x081DD97C`; start-helper `0x081DD000`
  (`[song+4] &= 0x7FFFFFFF`); command routine `0x081DD63C/640`;
  boot sound init `0x081DD034`; song-start `0x081DD7E0`;
  voice/PSG updater `0x081DDC6C`; mix code in IWRAM `bx 0x030028E1`;
  mix windows `0x030062A0` (ch1) / `0x030068D0` (ch2); 6 song structs
  `0x03006FB0 6F70 73D0 7380 7340 7300`.
- Intro audio = custom streaming decoder copied to IWRAM `0x03002BD4`
  (ROM `0x081DC398`): ARM fixed-point delta/LUT decoder, fed from ROM
  stream `0x08509F58+`, writes 224 samples/call into the mix window;
  called per-frame from the driver `0x081DC069` region.

## Remaining / next

- Commit the uncommitted fixes (19 files, ~3.5k insertions) once confirmed.
- Consider committing/keeping the debug arms gated behind FAIRY_DMA_TRACE.
- The game waits for input at the title — headless runs stop there;
  no further sound work expected.
