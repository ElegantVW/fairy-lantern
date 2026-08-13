# Sound investigation — Pokemon Liquid Crystal (v3.3.00512)

**Status (2026-08-13): FIXED and committed.** The hang below was timer
readback + IRQ/IntrWait + FIFO special DMA. Headless 400-frame run now reaches
the IWRAM stream decoder (`0x03002C38`) with FIFO audio at ~13.4 kHz.

This file is the historical write-up of the drone/hang. Do not treat the
“ROOT CAUSE IDENTIFIED (game hangs)” diagnosis as current. See `AGENTS.md`
and `docs/AUDIT.md` for present status. For the APU/FIFO/host
path as it exists now, see [SOUND_AUDIT.md](SOUND_AUDIT.md).

---

Status (original): ROOT CAUSE IDENTIFIED (game hangs in boot init; music never started).
Full detail for anyone resuming this thread.

## Symptom

The ROM plays a static 3-tone drone (E6/A6/C7, 7-frame periodicity, ~1 Hz
wobble) instead of intro/title music. The driver and DMA/FIFO path are alive;
the music data path is never driven.

## Anatomy of the sound system (verified by disassembly + trace)

### Driver pipeline (runs every frame)
```
game timer0 IRQ (0x0800080C)
  ├─ bl 0x081DC9D4  (re-arm: pos=sentinel, counter reload 7→0)   [0x08000816]
  └─ IE|=4, IME|=4
driver main 0x081DC020 (called by IRQ context, via thunk 0x081DC45A)
  ├─ gate [0x03005F50] == 0x68736D53  →  pos = sentinel+1
  ├─ call [0x03005F70] = 0x081DCA20   (song processor, runs all 6 songs
  │    chained via [song+0x38]; per song: gate [song+0x34]==sentinel →
  │    claim, then body: [song+4] >= 0 ? update : bail)
  ├─ call [0x03005F78] = 0x081DDC6C   (voice-track/PSG updater)
  └─ bx 0x030028E1                     (mix, IWRAM code; renders window
       0x030062A0 → DMA → FIFO A/B)
```

- Sentinel `0x68736D53` = "Smsh". Gate semantics (byte-accurate disasm of
  `0x081DCA20`): `[song+0x34]==sentinel` → **process** (jump over `bx lr`),
  write sentinel+1 at `0x081DCA2C`, re-arm sentinel at tail `0x081DCC6A`.
- 6 song structs: `0x03006FB0`, `0x03006F70`, `0x030073D0`, `0x03007380`,
  `0x03007340`, `0x03007300`. Fields: `+4` = run state (`0x80000000` =
  stopped, negative → processor bails), `+0x34` = sentinel gate,
  `+0x38` = next-song chain.
- Melody tracks for songs 0/1 live at `0x03007220`.
- SoundInfo = `0x03005F50`; 8 voice tracks at `0x03005FA0 + 0x50*i`
  (i=1..8 → 0x03005FA0, 5FF0, 6040, 6090, 60E0, 6130, 6180, 61D0).

### Boot sound init (0x081DD034)
```
zero SoundInfo (0x081E3B64, evt0)  → init fns 0x081DD4A0 / 0x081DD35C
command routine 0x081DD63C (r0=0x0094CC00 boot command, evt2,
                           lr=0x081DD059) → routine A 0x081DD728
                          + post-init 0x081DD598 (sets [0x03005F50+8/10/14/18],
                          routine B, spin on 0x9F)
six song-starts 0x081DD7E0 (evt2, lr=0x081DD073 ×4 and lr=0x081DD0A3 ×2)
   → each song created with [song+4] = 0x80000000 (STOPPED)
[SoundInfo+0x28] = 0x081DDC6D (evt2, pc=0x081DD3D2) → real target
   0x081DDC6C voice/PSG updater (runs every frame)
```

### Dead code / red herrings (proven never to run)
- `0x081DE638` "song-starter": no references to `0x081DE639`, SST trace never
  fires. `[SoundInfo+0x28]` never points to it (final value 0x081DDC6D).
- Start-helper `0x081DD000` (`[song+4] &= 0x7FFFFFFF`): zero thumb-bl callers,
  zero pointer refs in ROM, never observed at pc. Reachable only through
  game flow that never executes.
- Command routine `0x081DD63C/640`: callers `0x081DD054` (boot) and
  `0x081DD926` (game song API). Only the boot call ever executed.

### SONGWR proof (trace of all writes to the 6 songs' +4 and +0x34)
Only two writers exist: boot zero-init (evt0, pc=0x081E3B64) and song-start
(evt2, pc=0x081DD812) — the +4 stays `0x80000000` for all 400+ frames.

## The actual bug (emulator fidelity)

The game never reaches the title: it hangs in three boot-time waits.

1. **TM timer delay** — `0x081E34FC` spin (pool base `0x04000100`):
   `strh 0x83,[r4+2]` (TMxCNT_H enable+prescaler), then
   `ldrh r0,[r4] ; cmp r0,#0x1f ; bls` — waits for the live counter to
   exceed 0x1f (~2M cycles on real HW ≈ 7 frames). Observed spinning from
   evt28 through evt196; the timer readback never advances in the emulator.
2. **Flag wait** — `0x080008AA`: `ldrh r1,[0x0300010C] ; ands r0,r3,#1 ;
   beq` — waits for bit 0 of `0x0300010C`. Never set (evt224 → end of run).
3. **Flash/save-chip detect** — `0x08AF0000..0x08AF0400` (`0x08AF00A0..D8`
   inner loop): writes `0x4/0x5` to ROM scratch `0x080000C4/C6/C8` and does
   timing-dependent readback (real hardware detects flash via waitstates).
   Cycles forever; also executes the chip-ID read (`0x08AF00E8`) and parse
   (`0x08AF0186`) paths. Globals: `0x03005504/05` (detect results),
   `0x03005547`, `0x03005537`.

`dbg_evt` counts FIFO refills only (`src/dma.rs:133`); its freeze at 588 in
the frame trace = sound stopped being fed while the game spun.

Because the game never leaves boot, the song API (`0x081DD926` → `0x081DD63C`)
never fires, `[song+4]` is never cleared, the processor bails every frame,
and the mix renders only the static voice-track bytes left by the boot
zero-init (track 1 at `0x03005FA0` holds `52 00 62 1F 64 E3 ...`) — the drone.

## Fix order

1. `src/timers.rs`: TMxCNT_L readback must return the live count
   (`0x04000100`). Verify against the `0x081E34FC` spin completing.
2. Find what should set `[0x0300010C] & 1` (VBlank/IF mirror, keypad,
   or bios_intr_wait) — see `src/irq.rs`.
3. `src/bus.rs`: ROM writes (0x08000000 region) must be no-ops, not mutating
   the ROM image; reads must be 1-cycle-ish so the flash probe terminates.
4. After the title is reached: expect CMD/SST/SONGWR activity from the song
   API; music should start and the melody tracks `0x03007220` get written.
5. Strip debug arms (FRAME/DRV/CMD/SST/WINDOW/REARM/SPROC/DISP/MIXTRKW/
   SONGWR/S28WR/BUFWR/SLOTWR) once verified.

## Instrumentation map (where the arms live)

| Trace | Where | Triggers |
|---|---|---|
| FRAME pc | src/cpu/mod.rs:172 | every insn while evt%28==0 (log hog) |
| DRV (9 entries) | src/cpu/mod.rs:184 | 0x081DC9D4, 0x081DD728, 0x081DD7A4, 0x081DD7E0, 0x081DCA20, 0x081DD858, 0x081DD93C, 0x081DD97C, 0x081DD000 |
| CMD | src/cpu/mod.rs:197 | 0x081DD640 |
| SST | src/cpu/mod.rs:210 | 0x081DE638/0x081DE6DE |
| WINDOW/TRACK/MIXTRK | src/cpu/mod.rs:222 | 0x081DC088 (every 200th) |
| REARM | src/cpu/mod.rs:267 | 0x081DC9D4 (h<4 or h%700==0) |
| SPROC | src/cpu/mod.rs:284 | 0x081DCA20 (h<6 or c%1000==0) |
| DISP | src/cpu/mod.rs:304 | 0x081DC088 (first 8) |
| SLOTWR | src/bus.rs:316 | write8 0x030028E0..0x030028EC |
| MIXTRKW | src/bus.rs:324 | write8 0x03005FA0..0x03006228 |
| BUFWR | src/bus.rs:337 | write8 0x030062A0..0x03006F00 |
| SONGWR/S28WR | src/bus.rs:682 | write32 6-song +4/+0x34 fields, 0x03005F78 |
| fifo refill | src/dma.rs:119/134 | FIFO DMA refills (evt++ at :133) |
| WINDIFF | src/emu.rs:196 | frames 200..260 window diffs |

## Recent trace logs
`/tmp/opencode/cmd.log` (CMD once at evt2), `sst.log` (empty), `s28.log`,
`songw2.log` (SONGWR proof), `frames.log` (FRAME pc dump — stuck loops),
`start.log` (DRV over 150 frames — start-helper never fires).