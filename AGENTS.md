# AGENTS.md — fairy-lantern

GBA emulator in Rust. Canonical tree is this independent repo (`ElegantVW/fairy-lantern`).
The copy at `faeos/fairy-lantern` is stale and should not be edited.

## Status (2026-08-13)

v0.10.0. Restore point for **listenable intro music**: tag `sacred/sound-working`
/ branch `checkpoint/sound-working` (`7816e1b`). The working tree is **ahead** of
that tag: fight HP bars, Flash 128K erase, `FAELST05`, F5 shot+`.dbg.txt`,
pacing recovery, SWI→SVC, timer overflow counts, CI, unused-IO open bus,
HALTCNT Halt, 2-cycle IRQ delay.

LC (BPRE) boots to the overworld, plays intro/title through DirectSound A+B,
can finish an in-game FLASH save, and can fight (HUD, EXP). Affine identity/PD
hacks are **off** by default (hardware matrix). `FAIRY_AFFINE_COMPAT=1` restores
the old rewrite if a battle HUD/camera collapses.

This is **not** a commercial GBA emulator and **not** mGBA-class. Remaining
holes: [docs/AUDIT.md](docs/AUDIT.md).
Sound path: [docs/SOUND_AUDIT.md](docs/SOUND_AUDIT.md).
Old boot-hang write-up: [docs/SOUND_INVESTIGATION.md](docs/SOUND_INVESTIGATION.md).

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
| `FAIRY_AFFINE_COMPAT=1` | restore LC affine identity/PD rewrites (off by default) |

`fairy tone` goes through the same ring + resampler as a ROM (`--direct` skips it).

## Savestates

`.flst` magic `FAELST05`: CPU (FIQ/UND/ABT), IO, timers (frac), halt, DMA,
FIFOs, Flash/EEPROM FSM, RTC GPIO. `FAELST04`–`02` still load (those machines
start idle). F5 also writes `stem.ppm` and `stem.dbg.txt` (host RSS/CPU,
OAM, pals, sound). Copies: `/tmp/fairy-lantern-state.ppm` and `.dbg.txt`.

Affine identity/PD rewrites are **off**. If a **battle** HUD/camera collapses,
try `FAIRY_AFFINE_COMPAT=1`.

## Input (now / later)

**Now:** keyboard only. `play::poll_keys` ORs minifb keys into the 10-bit
GBA `KEYINPUT` mask (0 = pressed):

| GBA | Keys |
|-----|------|
| A | Z, Space, J |
| B | X, K |
| L / R | Q / E |
| Start / Select | Enter / RightShift or Backspace |
| D-pad | arrows or WASD |
| Pause / state | P / F5 / F7 / F8 / Esc |

minifb 0.27 has **no** gamepad API. Controllers do nothing today.

**Later (do not start until the LC campaign gate is boring):** map a host
gamepad onto the **same** `KEYINPUT` bits. That is a commercial advantage
(couch play) but it is not a current objective.

When we do it:

- Keep keyboard working; OR pads into the same mask (do not replace keys).
- Prefer `gilrs` (cross-platform) or Linux `evdev`; do not block the 59.73 Hz
  loop on device enumerate.
- Standard layout: face A/B, Start/Select, shoulders L/R, d-pad. Left stick
  → d-pad with a deadzone (~0.4). Ignore gyro/touch/rumble for v1.
- Hotplug: if the pad vanishes, fall back to keyboard without a hang.
- Do not send analog into KEYINPUT; the GBA keypad is digital.
- Test: LC overworld walk + fight menu with pad only, then keyboard only.

## Headless

`fairy run --frames N --load-state --save-state ROM` loads the default `.flst`,
runs N frames, writes state + shot + `.dbg.txt`. Always dumps
`/tmp/fairy-lantern-audio.wav` and `/tmp/fairy-lantern-last.ppm`.

## ROM loader

Bare `.gba` or `.zip` containing one. Images larger than 32 MiB are rejected;
zip declared size must match bytes extracted.

## Do not

- Edit `faeos/fairy-lantern` (stale vendor copy).
- Commit carts, saves, or `/tmp` captures.
- Re-introduce IWRAM writes from BIOS `SoundDriverMain`.
- Treat README “boots LC” as a claim that every GBA title works.
