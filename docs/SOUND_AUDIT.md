# Sound-systems audit

**Date:** 2026-08-13  
**Tree:** independent `fairy-lantern` after fix waves 1–2  
**Scope:** DirectSound FIFOs, DMA refill, sample timer, mixer, PSG, m4a BIOS HLE, host pipe, capture/WAV.

### Sound fix wave (same day)

Landed after this audit; numbered items below stay as written history:

1. `stream_rate = GBA_CLOCK / cps_out`. Dual-rate emits at the faster tick; slower FIFO holds.
2. `timer_cps_rate` no longer rejects periods outside 256–4096.
3. PSG ticks on a 32768 Hz accumulator; CH3 uses `2097152/(2048−n)`; emit window averages ticks.
4. FIFO `dma_req` clears at `len >= 16`; `needs_dma` is `n < 16`.
5. `SoundDriverMain` / vsync / channel-clear are stubs (no fake IWRAM PCM).
6. Tests: dual-rate rate identity, slow timer, 16-sample no re-request, PCM fixture, wave Hz.

Cross-check: GBATEK sound chapter; `src/sound/*`; `bus::tick_sound`; `dma::on_fifo_request`; `play.rs` pacing; Liquid Crystal 400-frame headless run.

---

## Verdict

The path that Liquid Crystal actually uses — **ROM mixer → FIFO A/B → timer pop → mono mix → capture** — works. The old 3-tone drone was a boot hang, not a FIFO bug.

Everything the README implies beyond that is either unproven or wrong:

- “Dual-rate FIFO” is implemented as two pop clocks, but the **output rate advertisement disagrees with the emit period** when A and B differ. Host will underrun or race.
- PSG exists but is clocked and pitched incorrectly. LC intro did not use it (`from_psg=0`).
- m4a BIOS SWIs are a guessed stub, not MP2K. LC does not call them.

Treat this as a **single-rate DirectSound player for ROM-side mixers**, not a complete GBA APU.

---

## Signal path

```
ROM / IWRAM mixer  (LC: "Smsh" driver + IWRAM decoder @ 0x03002BD4)
        │
        │  DMA1 → FIFO A (0x040000A0)
        │  DMA2 → FIFO B (0x040000A4)
        │  special timing, 4 words (16 samples) per request
        ▼
  Fifo  32 × signed 8-bit
        │  pop when derived Timer0/1 period elapses
        ▼
  Mixer::step  →  mix_sample(A + B + PSG) → mono i16
        │
        ├─ SampleRing  →  host thread → aplay / pw-cat @ stream_rate
        └─ Capture     →  dump_wav (/tmp/fairy-lantern-audio.wav)
```

BIOS SWIs `0x1A`–`0x2B` (`sound/bios.rs`) sit **beside** this path. They try to write a PCM buffer in IWRAM for DMA to pick up. Liquid Crystal never enters them.

---

## What is solid

| Piece | Why |
|---|---|
| FIFO size / LE push | 32 samples; `push_word` is little-endian; unit-tested |
| Special DMA | DMA1/2, dest forced to FIFO A/B, 4 words, reload on enable 0→1, repeat |
| Half-empty refill | `tick_sound` calls `on_fifo_request` when `dma_req` is set — not HBlank-only |
| Underrun | `hold_valid=false`, output 0 — no sticky SFX loop |
| BPRE timer math | `(0x10000-reload)*prescale = 1254` → ~13379 Hz (`timer_rate_13378ish`) |
| SOUNDCNT_H | dest enable, 50/100% vol, timer select bits, write-1 reset 11/15 |
| SOUNDCNT_X bit 7 | master mute |
| Capture | rolling buffer; headless `run` always dumps a WAV |

---

## Liquid Crystal evidence (400 frames, release)

ROM: user-supplied BPRE, not in repo.

| Metric | Value |
|---|---|
| cycles | 112,356,068 (expected 112,358,400) |
| pc | `0x03002C38` (IWRAM stream decoder) |
| `unk_ops` / `swi_unk` | 0 / 0 |
| FIFO out / from_fifo | 89,766 / 178,442 |
| peak | 10944 |
| `stream_rate` | 13378 Hz |
| `from_psg` | **0** |

A leftover `/tmp/fairy-lantern-audio.wav` from this machine was **tagged 32768 Hz**, 164205 frames, peak 10944. The header rate is whatever `stream_rate` was at dump time; the capture is a flat i16 stream that may have been produced at a different `cps_out`. Do not use that file as a golden.

**Proved:** ROM mixer + one FIFO rate + capture.  
**Not proved:** dual-rate, PSG, m4a SWI, live `aplay` pipe.

---

## Findings

Severity: **P0** = wrong on any title that is not “one FIFO rate, ROM mixer” · **P1** = you will hear it · **P2** = engineering · **P3** = nit.

### P0

#### 1. `cps_out` and `stream_rate` disagree when A and B differ

`mixer.rs` `update_rates`:

```text
both on:  cps_out    = max(cps_a, cps_b)     // slower period
          stream_rate = max(rate_a, rate_b)   // faster Hz
```

The mixer emits one i16 per `cps_out` GBA cycles. The host opens `aplay -r stream_rate`. If A is 13.4 kHz and B is 32.8 kHz, you produce ~13.4 k samples/s and tell ALSA it is 32.8 kHz. The pipe underruns (silence fill in `host.rs`) or the song races.

Liquid Crystal hides this because both FIFOs share one timer. The README “dual-rate FIFO” claim is not met.

**Fix:** `stream_rate` must be `GBA_CLOCK / cps_out` (same period). Different A/B rates: emit at the **faster** tick, sample-and-hold the slower FIFO (hardware DAC). Do not advertise a rate you are not producing.

#### 2. `timer_cps_rate` rejects legal periods

```text
if cyc < 256 || cyc > 4096 { return (0, 0, false); }
```

That is roughly 4 kHz–65 kHz. Outside that window the mixer treats the timer as **off**, then `invalidate_hold()` every step → silence, while DMA can still fill the FIFO.

Hardware pops on every overflow, however slow or fast.

**Fix:** any enabled timer is a clock. Clamp only the host open rate (e.g. 4 kHz–48 kHz), not the pop logic.

#### 3. m4a BIOS HLE is not MP2K

`SoundDriverMain` assumes 64-byte channels, ignores frequency, mixes **one** PCM byte per channel per SWI, writes `min(buf_len, 1024)`. `SoundInfo` field offsets are a guess. `music_player_*` increment counters.

Titles that call SWI `0x1A`–`0x2B` get a fake IWRAM buffer. LC is safe only because its driver is in ROM.

**Fix:** stub the SWIs (no RAM writes) **or** implement real MP2K. The current middle is the worst of both.

### P1

#### 4. PSG clock vs pitch

- `Psg::trigger` computes `phase_inc` from `stream_rate` (~13378 on LC).
- Every `sample()` then `refresh_square_freq(..., 32768)`.
- Mixer calls `sample` once (or `psg_ticks.min(4)` times) per **output** sample, not at 32768 Hz.

Envelope (64 Hz), length (256 Hz), and sweep (128 Hz) are derived from the `sample()` rate argument, so they are also wrong. LC intro did not hit this.

#### 5. Wave frequency is 32× too slow

Code: `65536 / (2048-n)`.  
GBATEK CH3: `2097152 / (2048-n)`.  
Wave leads will sit more than two octaves low.

#### 6. Last-tick-wins + cap of 4

```text
for _ in 0..psg_ticks.min(4) { psg_s = self.psg.sample(...) }
```

Only the last tick is mixed. Extra ticks burn envelope/length. The cap starves PSG when `cps_out` is large.

#### 7. Hold cleared when the derived timer is “off”

`if !clock_a { fifo_a.invalidate_hold() }` on every `Mixer::step`. Hardware holds the last FIFO byte until the next overflow. Combined with finding 2, a rejected period mutes the DAC even though the FIFO has data. Dest-bit off is closer to hardware; timer-reject is not.

#### 8. DMA request stays hot at exactly 16 samples

`push_*` clears `dma_req` only if `len > 16`. A 4-word refill into an empty FIFO lands at 16 and leaves the request true → another special DMA every `tick_sound` until 32. Harmless without cycle-accurate DMA; not GBATEK (request on crossing half-empty).

#### 9. Forced mono

Host is `-c 1`. `SOUNDCNT_H` dest bits are enables, not pan. `SOUNDCNT_L` L/R is averaged. A-left / B-right contrast is lost.

#### 10. SoundBias unused

`PsgRegs.bias` is filled from `0x04000088` and never applied. Speakers are AC-coupled so this is usually inaudible. Mid-level should be 0x200 if anything expects DC.

#### 11. Host reopens the player on every rate change

`host_pipe_loop` kills `aplay` and respawns when `game_rate` changes. Flicker from finding 1, or from the default 32768 → 13378 transition at first FIFO lock, clicks. `game_rate` starts at 0 so the thread idles until the first valid mix.

#### 12. Ring drops oldest when full

`push_batch` pops front at `RING_CAP` (24 000 samples). Comment says it does not. A stall skips notes. Play-loop 80/200 ms watermarks try to keep the ring in range.

### P2

13. Cubic resampler (`resample_into`) is unused. Host is game-rate direct. `resample.rs` header still talks about resampling.  
14. Wave bank / 64-sample mode (`SOUND3CNT_L` bits 5–6) not implemented.  
15. `SOUNDCNT_X` bits 0–3 (channel-on) never updated; games that poll them see 0.  
16. FIFO clock uses reload×prescale only — cascade-as-sample-clock will drift.  
17. Headless `run` always writes `/tmp/fairy-lantern-audio.wav`. Play dumps at frame 300.  
18. No golden PCM test; no dual-rate unit test.

### P3

- `HostAudio::stop` runs `kill -TERM`.  
- Stop flag is `Mutex<bool>`, not atomic.  
- Backend chosen once via `which aplay` / `pw-cat`.  
- Capture is ~20 s then drops the start of long runs.

---

## Recommended fix order

1. Tie `stream_rate` to `cps_out`. Dual-rate: emit at the faster tick, hold the slower FIFO.  
2. Drop the 256–4096 reject; clamp host open rate only.  
3. Independent 32768 Hz PSG accumulator; CH3 `2097152/(2048-n)`; mix the sum of ticks.  
4. Clear `dma_req` when `len >= 16` after refill (or set only on the half-empty crossing).  
5. Stub or really implement m4a — do not write fake IWRAM.  
6. Dual-rate unit test + a tiny committed PCM fixture (not a commercial ROM).

---

## Related docs

- Historical LC hang (fixed): [SOUND_INVESTIGATION.md](SOUND_INVESTIGATION.md)  
- Whole-project audit: [AUDIT.md](AUDIT.md)
