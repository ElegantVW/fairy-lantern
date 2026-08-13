# Fairy Lantern — GBA emulator from scratch

Light a fable; play a pocket world. A from-scratch Game Boy Advance emulator in
Rust: own ARM7TDMI core, bus, PPU, and sound path. No mGBA, no libretro cores.

> Independent repo — was part of the faeOS monorepo (`faeos/fairy-lantern`).
> Accuracy notes and known holes: [docs/AUDIT.md](docs/AUDIT.md).
> Restore the listenable-audio snapshot: `git checkout sacred/sound-working`.

## Status

v0.10.0 — Pokémon Liquid Crystal (BPRE) boots to the overworld with title art,
dialogue, walking camera, and **listenable intro/title music**. DirectSound
FIFOs A+B are mixed as the game programs them; the host plays 32768 Hz PWM
held samples resampled to 48 kHz stereo.

This is a working from-scratch interpreter for the titles that have been
walked through. It is not a completeness claim. Open holes:
[docs/AUDIT.md](docs/AUDIT.md), sound path: [docs/SOUND_AUDIT.md](docs/SOUND_AUDIT.md).
Operator notes: [AGENTS.md](AGENTS.md).

## Play

```
fairy play game.gba
```

| Key | Action |
|-----|--------|
| Arrows / WASD | D-pad |
| Z / Space | A |
| X | B |
| Enter | Start |
| P / F5 / F7 / Esc | Pause / savestate / load / quit |

ROMs are user-supplied only (`.gba` / `.zip` containing a `.gba`).

| Env | Effect |
|-----|--------|
| `FAIRY_DS=a` / `b` | FIFO A only or B only (default is both) |
| `FAIRY_AUDIO=sine` | 440 Hz instead of the game mix |
| `FAIRY_ACCURATE_AFFINE=1` | disable LC affine identity/PD hacks |

## Core surface

- ARM + Thumb interpreter (subset; unknown encodings are silent NOPs)
- IRQ banking (USR/SYS, IRQ, SVC, FIQ, UND, ABT) + BIOS IRQ HLE + IntrWait/Halt
- DMA immediate / VBlank / HBlank / FIFO special (DMA1/2). No DMA3 video capture.
- Timers with prescale remainder + cascade
- BIOS SWI: memory, decompress (LZ/RL/Huff), Div, ArcTan, AffineSet,
  SoundBias; m4a SWIs are stubs (no fake IWRAM PCM)
- Sound FIFO A/B + dest bits + 50/100% + SOUNDBIAS clip; host `pw-cat`/`aplay`
  at 48 kHz stereo; silence on underrun
- PPU Mode 0–5, priority composite, alpha + brightness, WIN0/1 + OBJ window,
  mosaic BG/OBJ, affine OBJ
- FLASH1M / FLASH / SRAM battery + savestates (`FAELST04`) + EEPROM bit-bang
- Keypad IRQ (KEYCNT), cartridge GPIO RTC (SIIRTC), GBA frame pacing (~59.73 Hz)
- Sequential vs N-cycle ROM fetch waitstates; data waitstates on LDR/STR;
  open-bus on unmapped reads

## Build

```
cargo build --release        # binaries: fairy-lantern, fairy
./build.sh install           # copy wrapper + bins into ~/bin
```

Debug instrumentation: `FAIRY_DMA_TRACE=1`, `FAIRY_MIX_STAT=1` — see
[AGENTS.md](AGENTS.md).

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
