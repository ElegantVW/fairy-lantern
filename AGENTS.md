# AGENTS.md — fairy-lantern

GBA emulator in Rust. Canonical tree is this independent repo (`ElegantVW/fairy-lantern`).
Vendor copy under `faeos/fairy-lantern` was removed — do not recreate it; plug in via
`./build.sh install` (see [faeOS docs/engines.md](https://github.com/ElegantVW/faeOS/blob/main/docs/engines.md)).

## Status (2026-08-17)

v0.11.0. Restore point for **listenable intro music**: tag `sacred/sound-working`
/ branch `checkpoint/sound-working` (`7816e1b`). The tree is ahead of that tag:
fight HP bars, Flash 128K erase, `FAELST05`, F5 shot+`.dbg.txt`, pacing recovery,
SWI→SVC, timer overflow counts, unused-IO open bus, HALTCNT Halt, 2-cycle IRQ delay,
Linux pad + turbo, pad LED probe, sound path polish.

LC (BPRE) boots to the overworld, plays intro/title through DirectSound A+B,
can finish an in-game FLASH save, and can fight (HUD, EXP). Affine identity/PD
hacks are **off** by default (hardware matrix). `FAIRY_AFFINE_COMPAT=1` restores
the old rewrite if a battle HUD/camera collapses.

This is **not** mGBA-class. Remaining holes: [docs/AUDIT.md](docs/AUDIT.md).
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
./build.sh install          # → ~/.local/lib/faeos/fairy + ~/bin/fairy launcher
cargo test
./target/release/fairy-lantern test
./target/release/fairy play /path/to/game.gba
./target/release/fairy run --frames 450 /path/to/game.gba
```

`~/bin/fairy` (from `./build.sh install` or `scripts/fairy`) prefers `$FAIRY_BIN`,
else rebuilds `~/fairy-lantern` when sources are newer, else uses
`~/.local/lib/faeos/fairy`.

Headless `run` always writes `/tmp/fairy-lantern-audio.wav` (48 kHz stereo)
and `/tmp/fairy-lantern-last.ppm`. FIFO traces: `/tmp/fairy-fifo-{a,b,ab}.wav`.

ROMs are user-supplied only. Do not commit `.gba` / `.sav`.

## Audio (current)

FIFO A/B pop on Timer0/1 overflow (~13379 Hz on BPRE/Emerald) and **hold**
between pops. GBATEK/mGBA then resample that hold to **32768 Hz PWM** with
SOUNDBIAS and a hard 10-bit clip — no DC blocker, no AGC. We emit at the FIFO
rate (the interpreter cannot feed 32k frames/s) and zero-order-hold to 48 kHz
via `pw-cat` (else `aplay`) at an **exact** `src/48k` ratio. Do not modulate
consume rate from ring depth (aliases against the 59.7 Hz play loop into a
~10 Hz intro tremolo). Do not high-pass the mix (an ~8 Hz HPF pumped dest-both
overworld BGM). Dest-both sums A+B per speaker; peaks fold instead of squaring.
FIFO underrun keeps the last DAC byte. Host underrun holds the last frame.
Prebuffer ~250 ms so a SoundVSync hitch does not empty the ring. The play loop
does not sprint extra GBA frames to feed the speaker.

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

## Input

Keyboard and a Linux joystick (`/dev/input/js0`–`js3`) OR into the same
10-bit GBA `KEYINPUT` mask (0 = pressed). No pad → keyboard only; a
vanished pad is not a hang.

| GBA | Keys | Pad |
|-----|------|-----|
| A | Z, Space, J | button 0/2 |
| B | X, K | button 1/3 |
| L / R | Q / E | button 4 / 5 |
| Start / Select | Enter / RightShift or Backspace | button 7/9 / 6/8 |
| D-pad | arrows or WASD | axis 0/1 or hat 6/7 (deadzone ~0.5) |
| Pause / state | P / F5 / F7 / F6 / F8 / Esc | hold L3 0.6s save · hold R3 0.6s load · F6 auto slot |
| Turbo on/off | C | pad X (west) |
| Turbo 2×/3×/4× | V | pad Y (north) |

`fairy pad --led` walks Xbox player-LED + RGB candidate USB reports (watch the rings).
`fairy pad --follow-wisp` polls `~/.config/wisp/current.json` but does not paint RGB until a report is proven.

Do not send analog into KEYINPUT; the GBA keypad is digital.

## Headless

`fairy run --frames N --load-state --save-state ROM` loads the default `.flst`,
runs N frames, writes state + shot + `.dbg.txt`. Always dumps
`/tmp/fairy-lantern-audio.wav` and `/tmp/fairy-lantern-last.ppm`.

## ROM loader

Bare `.gba` or `.zip` containing one. Images larger than 32 MiB are rejected;
zip declared size must match bytes extracted.

## Do not

- Vendor this tree back into `faeos/fairy-lantern` (use engines.md install).
- Commit carts, saves, prebuilt ELFs, or `/tmp` captures.
- Re-introduce IWRAM writes from BIOS `SoundDriverMain`.
- Treat README “boots LC” as a claim that every GBA title works.
