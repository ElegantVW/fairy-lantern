# Fairy Lantern — GBA emulator from scratch

Light a fable; play a pocket world. A from-scratch Game Boy Advance emulator in
Rust: own ARM7TDMI core, bus, PPU, and sound path. No mGBA, no libretro cores.

Source-only repo (no prebuilt programs or ROMs in git). Independent of the
faeOS monorepo — plug in with one install command (see below).

Accuracy notes: [docs/AUDIT.md](docs/AUDIT.md).  
Listenable-audio restore point: `git checkout sacred/sound-working`.

## Status

v0.11.0 — Pokémon Liquid Crystal (BPRE) boots, plays intro/title music, can
**save in-game (FLASH 128K)**, and can fight (HP/EXP HUD). DirectSound FIFOs
A+B are mixed as the game programs them; host is FIFO-rate hold → 48 kHz stereo.
Linux joystick + turbo; optional pad LED probe (`fairy pad --led`).

This is a working from-scratch interpreter for the titles that have been
walked through. It is not a completeness claim. Open holes:
[docs/AUDIT.md](docs/AUDIT.md), sound path: [docs/SOUND_AUDIT.md](docs/SOUND_AUDIT.md).
Operator notes: [AGENTS.md](AGENTS.md).

## Build & install (plug-and-play with faeOS)

```bash
git clone git@github.com:ElegantVW/fairy-lantern.git ~/fairy-lantern
cd ~/fairy-lantern && ./build.sh install
```

| What | Where |
|------|--------|
| Real binary | `~/.local/lib/faeos/fairy` (+ `fairy-lantern`) |
| Public command | `~/bin/fairy` (thin launcher; auto-rebuilds if sources are newer) |

With [faeOS](https://github.com/ElegantVW/faeOS) installed first, the same
command names light up with no path edits. Full contract:
[faeOS docs/engines.md](https://github.com/ElegantVW/faeOS/blob/main/docs/engines.md).

```bash
cargo build --release        # tree only: target/release/fairy
cargo test
```

Host audio: PipeWire `pw-cat` (preferred) or ALSA `aplay`.

## Play

```
fairy play game.gba
```

| Key | Action |
|-----|--------|
| Arrows / WASD | D-pad |
| Z / Space / J | A |
| X / K | B |
| Q / E | L / R |
| Enter / RightShift | Start / Select |
| P / F5 / F7 / F6 / F8 / Esc | Pause / save / load / load autosave / OAM / quit |
| C / V · pad X / Y | Turbo on-off / cycle 2×–4× |
| hold L3 / hold R3 | Save / load (0.6s). L2/R2/M2 do **not** savestate |

Input is keyboard plus an optional Linux joystick (`/dev/input/js*`), both
OR-ed into the same 10-bit `KEYINPUT` mask. See [AGENTS.md](AGENTS.md) § Input.

ROMs are user-supplied only (`.gba` / `.zip` containing a `.gba`).

| Env | Effect |
|-----|--------|
| `FAIRY_BIN` | Force engine path (skip discovery) |
| `FAIRY_LANTERN_ROOT` | Source tree (default `~/fairy-lantern`) |
| `FAIRY_DS=a` / `b` | FIFO A only or B only (default is both) |
| `FAIRY_AUDIO=sine` | 440 Hz instead of the game mix |
| `FAIRY_AFFINE_COMPAT=1` | restore LC affine identity/PD rewrites (off by default) |

Debug: `FAIRY_DMA_TRACE=1`, `FAIRY_MIX_STAT=1` — see [AGENTS.md](AGENTS.md).

## Core surface

- ARM + Thumb interpreter (subset; unknown encodings are silent NOPs)
- IRQ banking (USR/SYS, IRQ, SVC, FIQ, UND, ABT) + BIOS IRQ HLE + IntrWait/Halt
- DMA immediate / VBlank / HBlank / FIFO special (DMA1/2) + DMA3 video capture
  (one line per HBlank, VCOUNT 2–161)
- Timers with prescale remainder + cascade
- BIOS SWI enters SVC then HLE (Div, Sqrt, decompress, AffineSet, …);
  m4a SWIs stay stubs (no fake IWRAM PCM)
- Sound FIFO A/B + dest bits + 50/100% + SOUNDBIAS fold; host `pw-cat`/`aplay`
  at 48 kHz stereo; underrun holds the last frame
- PPU Mode 0–5, priority composite, alpha + brightness, WIN0/1 + OBJ window,
  mosaic BG/OBJ, affine OBJ
- FLASH1M / FLASH / SRAM battery + savestates (`FAELST05`) + EEPROM bit-bang;
  untagged carts stay `None` until the first `0x0E` write (then SRAM 64K)
- Keypad IRQ (KEYCNT), cartridge GPIO RTC (SIIRTC), GBA frame pacing (~59.73 Hz)
- Sequential vs N-cycle ROM fetch waitstates; data waitstates on LDR/STR;
  open-bus on unmapped reads and unused/write-only IO; HALTCNT Halt;
  ~2-cycle IRQ delay after IME/IE/IF; 8-bit PAL/OAM ignored, BG VRAM
  duplicates the byte; DISPSTAT live flags are read-only

## Tests

```
cargo test
fairy-lantern test          # CLI smoke check
```

Unit tests cover FIFO/mixer, ARM `MSR #imm`, `STRB [Rn, Rm]`, timer cascade,
FIFO DMA, savestate round-trip, and the 32 MiB ROM cap. Commercial-ROM checks
are headless `run --frames` on a user-supplied cart.

## License

MIT.
