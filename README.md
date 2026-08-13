# Fairy Lantern — GBA emulator from scratch

Light a fable; play a pocket world. A from-scratch Game Boy Advance emulator in
Rust: own ARM7TDMI core, bus, PPU, and sound path. No mGBA, no libretro cores.

> Independent repo — was part of the faeOS monorepo (`faeos/fairy-lantern`).
> Accuracy notes and known holes: [docs/AUDIT.md](docs/AUDIT.md).

## Status

v0.10 — dual-rate FIFO (no sticky SFX), OBJ mosaic, semi-OBJ blend, battle UI.
Commercial-class boot verified: Pokémon Liquid Crystal boots to the overworld
with title art, dialogue, walking camera, and a working DirectSound FIFO → host
audio path (~13.4 kHz for BPRE). See [AGENTS.md](AGENTS.md) for the deep dive
and [docs/SOUND_INVESTIGATION.md](docs/SOUND_INVESTIGATION.md).

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

## Core surface

- ARM + Thumb interpreter (commercial-class subset)
- IRQ banking + BIOS IRQ HLE + IntrWait/Halt
- DMA imm / VBlank / HBlank / FIFO special
- Timers with prescale remainder + cascade
- BIOS SWI: memory, decompress (LZ/RL/Huff), Div, ArcTan, AffineSet,
  SoundBias, m4a sound-driver family
- Sound FIFO A/B sinks + DirectSound → host audio (`aplay`/`pw-cat`),
  1:1 sample path (dual-rate: emit at the faster FIFO, hold the slower),
  silence on underrun
- PPU Mode 0–5, priority composite, alpha + brightness, WIN0/1 + OBJ window,
  mosaic BG, affine OBJ
- FLASH1M / FLASH / SRAM battery + savestates + EEPROM bit-bang
- Keypad IRQ (KEYCNT), cartridge GPIO RTC (SIIRTC), GBA frame pacing (~59.73 Hz)
- Approximate fetch waitstates, open-bus on unmapped reads

## Build

```
cargo build --release        # binaries: fairy-lantern, fairy
./build.sh install           # copy into ~/bin (faeOS layout)
```

Debug instrumentation is gated behind `FAIRY_DMA_TRACE=1` — see
[AGENTS.md](AGENTS.md) for addresses and run conventions.

## Tests

```
cargo test
fairy-lantern test          # same self-tests as a CLI smoke check
```

Unit tests cover sound FIFO/mixer, ARM `MSR #imm`, timer cascade, FIFO DMA,
savestate round-trip, and the 32 MiB ROM cap. Commercial-ROM checks are still
headless `run --frames` (see [AGENTS.md](AGENTS.md)).

## License

MIT.
