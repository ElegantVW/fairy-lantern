# Sound-systems audit

**Original date:** 2026-08-13  
**Current as of:** 2026-08-13, `7816e1b` / tag `sacred/sound-working`  
**Tree:** independent `fairy-lantern`  
**Scope:** DirectSound FIFOs, DMA refill, sample timer, mixer, PSG, m4a BIOS HLE, host pipe, capture/WAV.

Numbered findings below are the first-pass write-up. Many are done. Use the
status table and the current path, not the old P0 labels.

---

## Verdict (current)

Liquid Crystal’s path — **ROM “Smsh” mixer → IWRAM pcmBuffer → DMA1/2 FIFO →
timer pop → dest-both mix → 32768 Hz hold → 48 kHz `pw-cat`** — is listenable.
The 3-tone drone was a boot hang. The looping star / later mush was ARM
`STRB [Rn, Rm]` writing reverb to `r5+6` instead of the left buffer at `+1584`.

This is a working **ROM-mixer DirectSound player**, not a complete GBA APU:

- PSG exists (32768 Hz accum, CH3 formula) but LC intro never used it (`from_psg=0`).
- BIOS m4a SWIs are stubs (no fake IWRAM). LC does not call them.
- Wave 64-sample bank and `SOUNDCNT_X` bits 0–3 are implemented.

---

## Signal path (current)

```
ROM / IWRAM mixer  (LC: "Smsh" @ SoundInfo 0x03005F50, mix @ 0x030028E1)
        │  12 × 64-byte SoundChannel @ 0x03005FA0
        │  pcmBuffer right 0x030062A0, left +1584 (PCM_DMA_BUF_SIZE)
        │
        │  DMA1 → FIFO A (0x040000A0)   SAD = 0x030062A0
        │  DMA2 → FIFO B (0x040000A4)   SAD = 0x030068D0
        │  special timing, 4 words (16 samples) per half-empty
        │  m4aSoundVSync rewinds SAD every pcmDmaPeriod (7) VBlanks
        ▼
  Fifo  32 × signed 8-bit
        │  pop on Timer0/1 overflow (~13379 Hz on BPRE)
        ▼
  Mixer::step
        │  dest bits enable A/B per speaker (LC dest-both = mono sum)
        │  50% = sample<<1, 100% = sample<<2; SOUNDBIAS 0x200, clip 0..0x3FF
        │  emit at 32768 Hz, holding last FIFO byte between pops
        │
        ├─ SampleRing → pw-cat (else aplay) @ 48 kHz stereo
        │    prebuffer ~125 ms; underrun = silence (no hold-last)
        └─ Capture → /tmp/fairy-lantern-audio.wav (48 kHz stereo)
```

BIOS SWIs `0x1A`–`0x2B` sit **beside** this path and must not write IWRAM.
Liquid Crystal never enters them.

`FAIRY_DS=a|b` isolates one FIFO. `FAIRY_AUDIO=sine` replaces the mix.
`FAIRY_MIX_STAT=1` dumps A/B stats + 12 channels + DMA src every 25 frames.

---

## What is solid

| Piece | Why |
|---|---|
| FIFO size / LE push | 32 samples; `push_word` little-endian; unit-tested |
| Special DMA | DMA1/2, dest forced to FIFO A/B, 4 words, reload SAD on enable 0→1, repeat |
| Half-empty refill | `tick_sound` → `on_fifo_request` when `len < 16` |
| Underrun | `hold_valid=false` → 0; host also inserts silence |
| BPRE timer math | `(0x10000-reload)*prescale = 1254` → ~13379 Hz |
| SOUNDCNT_H | dest enable, 50/100%, timer select, write-1 reset 11/15 |
| SOUNDCNT_X bit 7 | master mute; write-0 stops all PSG |
| SOUNDCNT_X bits 0–3 | live PSG-on flags (read-only) |
| Wave banks | SOUND3CNT_L bit5=64-sample, bit6=CPU bank |
| SOUNDBIAS | `bias_out`: add 0x200, clip 10-bit, scale to i16 |
| Addressing mode 2 | `STRB [r5, r6]` writes `r5+r6` (reverb left buffer) |
| Host | device opened once; 48 kHz stereo; no rate-change reopen |
| Capture | headless `run` always dumps a WAV |

---

## Liquid Crystal evidence (`sacred/sound-working`, 450 frames)

ROM: user-supplied BPRE, not in repo.

| Metric | Value |
|---|---|
| cycles | 126,400,555 (expected 126,403,200) |
| pc | `0x03002C50` (IWRAM mixer) |
| `unk_ops` / `swi_unk` | 0 / 0 |
| ident / period / rev / ch | `Smsh`, pcmDmaPeriod=7, reverb=50, 12 channels |
| A peak / mean / rail | 30–57 / 6–12 / 0 |
| B peak / mean / rail | 15–71 / 6–13 / 0 (was ±128 before the STRB fix) |
| host emit | 32768 Hz hold, dump 48 kHz stereo, peak ~7200, no i16 rail |
| `from_psg` | **0** |

**Proved:** ROM mixer + both FIFOs + dest-both + live `pw-cat` + intro music.  
**Not proved:** PSG, BIOS m4a SWIs, dual-timer A/B at different rates, cries/fights.

Do not use pre-checkpoint `/tmp/fairy-lantern-audio.wav` files as goldens.

---

## Finding status

| # | Topic | Status |
|---|---|---|
| 1 | `cps_out` vs `stream_rate` when A≠B | **Superseded** — emit at 32768 Hz PWM; FIFO pops stay on the timer |
| 2 | `timer_cps_rate` rejects 256–4096 | **Fixed** — any enabled timer clocks the FIFO |
| 3 | m4a HLE writes fake IWRAM | **Stubbed** — no RAM writes |
| 4 | PSG clock vs pitch | **Partial** — 32768 Hz accum + CH3 formula in tree; LC unused |
| 5 | Wave freq 32× slow | **Fixed** in code (`2097152/(2048−n)`); unproven in-game |
| 6 | Last-tick-wins + cap of 4 | **Fixed** (PWM-rate emit; PSG still unproven) |
| 7 | Hold cleared when timer “off” | **Fixed** — dest-off clears; timer-off holds |
| 8 | DMA req hot at exactly 16 | **Fixed** (`len >= 16` clears) |
| 9 | Forced mono / dest bits | **Fixed** — dest bits are enables; LC dest-both sums A+B |
| 10 | SoundBias unused | **Fixed** (`bias_out`) |
| 11 | Host reopens on rate change | **Fixed** — open once at 48 kHz |
| 12 | Ring drops oldest | **Changed** — drop newest if full; underrun = silence |
| 13 | Cubic resampler unused | **Open** (linear 32768→48k is what the host uses) |
| 14 | Wave bank / 64-sample mode | **Fixed** |
| 15 | `SOUNDCNT_X` bits 0–3 | **Fixed** |
| 16 | FIFO clock ignores cascade | **Open** |
| 17 | Headless always dumps `/tmp` WAV | **Open** (debug default) |
| 18 | No golden PCM / dual-rate test | **Partial** — mixer unit tests exist; no committed WAV golden |

**Also fixed (not in the original list):** ARM `STRB [Rn, Rm]` / reverb left
buffer. Without it, B railed and leaked into A. Test:
`cpu::tests::strb_reg_offset_uses_rm_not_imm`.

### Sound fix waves (historical)

1. Tie emit period to a real clock; hold the slower FIFO.
2. Drop the 256–4096 reject.
3. PSG 32768 Hz accum; CH3 formula.
4. `dma_req` at `len >= 16`.
5. Stub m4a SWIs (no fake PCM).
6. Emit at hardware PWM 32768 Hz.
7. `pw-cat`, prebuffer, silence on underrun; `fairy tone` through the ring.
8. Addressing mode 2; dest-both A+B.

---

## Findings (original text)

Severity labels below are from the first pass. Check **Finding status** before
treating a P0 as open. The “mono mix” / 13378 Hz host / fake-IWRAM descriptions
are stale.

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

## Recommended fix order (current)

1–5 and the STRB/reverb fix are done. Next:

1. Play LC **fights** (cries, layered SFX). If it rails again, dump `FAIRY_MIX_STAT=1` before changing the mixer.
2. PPU goldens / PCM fixture if a fight or cry sounds wrong.
3. A tiny committed PCM fixture (not a commercial ROM). Dual-timer A/B at different rates if a title needs it.
4. Leave BIOS m4a as stubs unless a game actually calls SWI `0x1A`–`0x2B`.

---

## Related docs

- Historical LC hang (fixed): [SOUND_INVESTIGATION.md](SOUND_INVESTIGATION.md)  
- Whole-project audit: [AUDIT.md](AUDIT.md)
