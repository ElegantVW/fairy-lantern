# AGENTS.md — fairy-lantern

GBA emulator in Rust. Canonical tree is this independent repo (`ElegantVW/fairy-lantern`).
The copy at `faeos/fairy-lantern` is stale and should not be edited.

## Status (2026-08-13)

v0.10, committed. Pokémon Liquid Crystal (BPRE) boots: intro decoder in IWRAM,
DirectSound FIFO → host at ~13.4 kHz, `unk_ops=0` on a 400-frame headless run.

Audio/boot hangs documented in `docs/SOUND_INVESTIGATION.md` are **fixed**:
timer prescale remainder + cascade, IRQ HLE / IntrWait, FIFO special DMA.

Known remaining holes (do not treat this as mGBA-class): [docs/AUDIT.md](docs/AUDIT.md).

## Build / run

```
cargo build --release
cargo test
./target/release/fairy-lantern test
FAIRY_DMA_TRACE=1 ./target/release/fairy \
  "/path/to/game.gba" --frames N 2> log
```

Audio capture: `/tmp/fairy-lantern-audio.wav`. Last frame: `/tmp/fairy-lantern-last.ppm`.

ROMs are user-supplied only. Do not commit `.gba` / `.sav`.

## Debug (`FAIRY_DMA_TRACE=1`)

CPU still has Liquid Crystal–specific PC traces (driver `0x081DC…`, mix window
`0x030062A0`). Prefer that env over adding more hardcoded PCs. `FAIRY_DEBUG=1`
dumps PPU regs on headless `run`.

## Savestates

`.flst` magic `FAELST04`: CPU (including FIQ/UND/ABT banks), IO, timers (frac),
halt, DMA internals, sound FIFOs. `FAELST03` / `FAELST02` still load. F5 / F7
in the play window.

Affine identity/PD caps stay on for Liquid Crystal. Set `FAIRY_ACCURATE_AFFINE=1`
to disable them.

## ROM loader

Bare `.gba` or `.zip` containing one. Images larger than 32 MiB are rejected;
zip declared size must match bytes extracted.
