# AGENTS.md — fairy-lantern

GBA emulator in Rust. Canonical tree is this independent repo (`ElegantVW/fairy-lantern`).
The copy at `faeos/fairy-lantern` is stale and should not be edited.

## Status (2026-08-13, `sacred/sound-working`)

v0.10.0, commit `7816e1b`. Tag `sacred/sound-working` and branch `checkpoint/sound-working`
are the restore point where Pokémon Liquid Crystal **intro music is listenable**.

LC (BPRE) boots to the overworld: title art, dialogue, walking camera, DirectSound
FIFO A+B → host. Headless 450-frame run: `unk_ops=0`, `swi_unk=0`, IWRAM mixer
alive, A/B mix buffers stay in a healthy 8-bit range (no rail).

This is **not** mGBA-class. Remaining holes: [docs/AUDIT.md](docs/AUDIT.md).
Sound path detail: [docs/SOUND_AUDIT.md](docs/SOUND_AUDIT.md).
The old boot-hang write-up is history: [docs/SOUND_INVESTIGATION.md](docs/SOUND_INVESTIGATION.md).

To restore this sound state later:

```
git checkout sacred/sound-working
# or
git checkout checkpoint/sound-working
```

## Build / run

```
cargo build --release
cargo test
./target/release/fairy-lantern test
./target/release/fairy play /path/to/game.gba
./target/release/fairy run --frames 450 /path/to/game.gba
```

`~/bin/fairy` (from `./build.sh install` or `scripts/fairy`) rebuilds
`~/fairy-lantern` when sources are newer, then execs `target/release/fairy`.

Headless `run` always writes `/tmp/fairy-lantern-audio.wav` (48 kHz stereo)
and `/tmp/fairy-lantern-last.ppm`. FIFO traces: `/tmp/fairy-fifo-{a,b,ab}.wav`.

ROMs are user-supplied only. Do not commit `.gba` / `.sav`.

## Audio (current)

Hardware PWM rate is **32768 Hz**; FIFO A/B pop on Timer0/1 overflow (~13379 Hz
on BPRE) and **hold** between pops. Host resamples 32768 → 48 kHz stereo via
`pw-cat` (else `aplay`). Underrun inserts silence (does not hold the last
period). Dest-both sums FIFO A+B per speaker the way LC programs SOUNDCNT_H.

LC’s ROM mixer (“Smsh”) writes stereo into IWRAM: right at `0x030062A0`, left
at `+1584`. DMA1/2 special refill those FIFOs. BIOS m4a SWIs are stubs and
must not invent PCM; LC does not call them.

The intro-star wash was ARM addressing mode 2: `STRB [Rn, Rm]` used the
data-processing immediate decoder, so reverb wrote `r5+6` instead of
`r5+1584` and the left buffer accumulated. Fixed; covered by
`strb_reg_offset_uses_rm_not_imm`.

| Env | Effect |
|-----|--------|
| `FAIRY_DS=a` / `b` / default | play FIFO A only, B only, or both |
| `FAIRY_AUDIO=sine` | replace mix with 440 Hz (pipe/video test) |
| `FAIRY_MIX_STAT=1` | every 25 frames: A/B stats, 12 SoundChannels, DMA src |
| `FAIRY_DUMP_IWRAM=1` | hexdump mix window at end of `run` |
| `FAIRY_DMA_TRACE=1` | DMA / FIFO / selected IWRAM traces |
| `FAIRY_DEBUG=1` | PPU regs on headless `run` |
| `FAIRY_ACCURATE_AFFINE=1` | disable LC affine identity/PD hacks |

`fairy tone` goes through the same ring + resampler as a ROM (`--direct` skips it).

## Savestates

`.flst` magic `FAELST04`: CPU (including FIQ/UND/ABT banks), IO, timers (frac),
halt, DMA internals, sound FIFOs. `FAELST03` / `FAELST02` still load. F5 / F7
in the play window.

Affine identity/PD caps stay on for Liquid Crystal unless
`FAIRY_ACCURATE_AFFINE=1`. Try that flag if a **battle** HUD/camera looks wrong.

## ROM loader

Bare `.gba` or `.zip` containing one. Images larger than 32 MiB are rejected;
zip declared size must match bytes extracted.

## Do not

- Edit `faeos/fairy-lantern` (stale vendor copy).
- Commit carts, saves, or `/tmp` captures.
- Re-introduce IWRAM writes from BIOS `SoundDriverMain`.
- Treat README “boots LC” as a claim that every GBA title works.
