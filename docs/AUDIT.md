# Fairy Lantern audit

**Original date:** 2026-08-13  
**Current as of:** 2026-08-14 (tree ahead of `f1fc40d` / `sacred/sound-working`)  
**Tree:** independent repo (`ElegantVW/fairy-lantern`)  
**Scope:** static review of `src/`, docs, git/hygiene, tests, and Pokémon Liquid Crystal. Sound follow-up: [SOUND_AUDIT.md](SOUND_AUDIT.md).

Numbered findings below are the **original write-up**. Many are done. Use the status table, not the old P0 labels.

## Verdict (current)

A from-scratch interpreter that boots Liquid Crystal (BPRE) to the overworld,
plays intro/title through real DirectSound FIFOs, can finish an in-game FLASH
save, and can fight (HP/EXP HUD). CPU, IRQ, timers, FIFO DMA, and the ARM
`STRB [Rn, Rm]` addressing-mode-2 bug were the load-bearing fixes.

It is **not** a commercial GBA emulator and **not** mGBA-class. Unknown opcodes
still NOP (first 8 log), PSG is unproven on LC, affine identity/PD hacks stay
default-on, there are no PPU goldens, the host is Linux `pw-cat`, and only one
title has been walked. SWI enters SVC for HLE; untagged carts do not invent SRAM.

Use it for titles that have been walked through. Do not read “v0.10” as a
completeness claim.

## Snapshot (current)

| Item | State |
|---|---|
| Version | `Cargo.toml` 0.10.0 |
| Restore | tag `sacred/sound-working`, branch `checkpoint/sound-working` (`7816e1b`) |
| `cargo test --bin fairy` | 99 unit tests passed |
| CI | `rust-toolchain.toml` + `.github/workflows/test.yml` |
| `faeos/fairy-lantern` | stale — do not edit |
| Docs | this file + [AGENTS.md](../AGENTS.md) + [SOUND_AUDIT.md](SOUND_AUDIT.md) |

### Liquid Crystal (after `sacred/sound-working`)

ROM: user-supplied BPRE, not in repo.

| | 400-frame (pre-audio-fix, historical) | 450-frame (sound-working) |
|---|---|---|
| cycles | 112,356,068 / expected 112,358,400 | 126,400,555 / expected 126,403,200 |
| pc | `0x03002C38` | `0x03002C50` (same IWRAM mixer) |
| `unk_ops` / `swi_unk` | 0 / 0 | 0 / 0 |
| A mix buffer | healthy 8-bit | peak ~30–57, mean ~6–12, rail 0 |
| B mix buffer | railed ±128 (reverb never wrote it) | peak ~15–71, mean ~6–13, rail 0 |
| host emit | 13378 Hz advertised | 32768 Hz PWM hold → 48 kHz stereo |
| PSG | 0 | 0 |
| intro | song buried under star wash | listenable; star is a one-shot |

Affine identity/PD hacks remain **on** by default. Fight HP bars work (`a3958ac`).

## Finding status

| # | Topic | Status |
|---|---|---|
| 1 | Savestate timer-ctrl from old IO | **Fixed** (`FAELST03`+) |
| 2 | Savestate not a full machine | **Partial** — `FAELST05` adds Flash/EEPROM FSM + RTC GPIO; host audio ring still omitted |
| 3 | ARM `MSR #imm` never decodes | **Fixed** (mask `0x0FB0_F000` + test) |
| 4 | Zip / ROM no size cap | **Fixed** (32 MiB + zip size match) |
| 5 | Unknown opcodes silent NOP | **Partial** — still NOP; first 8 log `pc`+opcode |
| 6 | BIOS always readable | **Fixed** — open-bus unless `exec_pc` is in BIOS |
| 7 | Duplicate SOUNDCNT `write16` arms | **Fixed** (clippy clean on that path) |
| 8 | Only USR/IRQ/SVC banked | **Fixed** (FIQ/UND/ABT) |
| 9 | `LDM/STM ^` user-bank ignored | **Fixed** |
| 10 | SWI never enters SVC | **Fixed** — enter SVC + I + ARM, HLE, restore SPSR (SoftReset may stay out) |
| 11 | SoftReset / Sqrt / RegisterRamReset | **Fixed** — SoftReset honors `03007FFA` (ROM vs EWRAM) and wipes 200h; integer Sqrt; IWRAM keeps last 0x200; bit7 clears IO not CPU |
| 12 | HBlank at end of 1232-cycle line | **Fixed** (`HBLANK_CYCLE = 1006`) |
| 13 | Affine identity / PD caps | **Gated** — still default-on; `FAIRY_ACCURATE_AFFINE=1` disables |
| 14 | DMA3 video capture | **Fixed** — special DMA3 one line per HBlank, VCOUNT 2..=161, off at 162 |
| 15 | Timer overflow storms capped at 16 | **Fixed** — all overflows counted; cascade closed-form; IF raised once |
| 16 | m4a BIOS HLE guess / fake PCM | **Stubbed** — no IWRAM writes; LC uses ROM mixer |
| 17 | Waitstates averaged | **Partial** — WS0/1/2 N+S, SRAM WAITCNT; cart data forces next fetch N; prefetch is an 8-halfword stand-in |
| 18 | VRAM `% 96K` | **Fixed** — 128K window, upper 32K mirrors OBJ |
| 19 | Unknown save → 64K SRAM | **Fixed** — untagged cart is `SaveType::None` (no SRAM poke) |
| 20 | Flash IDs / erase approximate | **Partial** — AMD 80+AA/55+10/30 erase works; IDs follow SDK tag (Sanyo/Macronix/SST/Panasonic). Atmel protocol not implemented (never reported) |
| 21 | `FAIRY_DMA_TRACE` env on hot path | **Fixed** (`fairy_trace()` OnceLock) |
| 22 | LC PCs hardcoded in `cpu/mod.rs` | **Partial** — `cfg(debug)` + `FAIRY_DMA_TRACE`; gone from release step |
| 23 | Docs contradict each other | **Fixed** (this refresh) |
| 24 | Almost no regression tests | **Partial** — 99 unit tests; still no PPU goldens / committed PCM fixture |
| 25 | No CI / toolchain pin | **Partial** — `rust-toolchain.toml` + `.github/workflows/test.yml` |
| 26 | `faeos/fairy-lantern` will rot | **Open** — treat as dead; do not edit |
| 27 | Host is `aplay`/`pw-cat` | **Partial** — `pw-cat` 48 kHz stereo, no reopen; still Linux-only |
| 28 | Headless always writes `/tmp` WAV+PPM | **Open** (intentional for debug) |
| 29 | TUI needs Spellbook on PATH | **Open** |
| 30 | SPARK assembler `panic!` | **Fixed** — unencodable imm encodes `#0` instead of aborting |

Plus (not in the original list):
- ARM addressing mode 2 register offset — **Fixed** (`strb_reg_offset_uses_rm_not_imm`).
- Same-priority OBJ order — **Fixed** (low OAM index in front; Gen3 HP bars).
- Flash AMD erase 80+AA/55+10/30 — **Fixed**.
- Host catch-up spiral (up to 6 frames, never recovered) — **Fixed** (1 extra if ring starving).
- OBJ scanline cycle budget (~1210) — **Fixed** (OAM 0→127; leftovers dropped).
- Unused IO / write-only FIFO+HALTCNT — **Fixed** (open bus, not `io[]` zeros).
- HALTCNT (`4000301h`) — **Fixed** (byte/halfword write Halts; POSTFLG is bit0).
- IRQ 2-cycle delay — **Fixed** (Halt 64-cycle slices still take immediately).
- IE / IME unused bits — **Fixed** (IE `0x3FFF`, IME bit 0).
- Thumb `LDMIA` writeback when `Rb` is in the list — **Fixed** (loaded value wins).
- 8-bit PAL/OAM writes ignored; BG VRAM 8-bit duplicates the byte; OBJ ignored.
- `write16`/`write32` to video mem no longer split into two `write8`s.
- DISPSTAT bits 0–2 (v/h/vc flags) are read-only on write; WAITCNT bit15 stays 0.
- ARM `LDM` base-in-list keeps the loaded value; `STM` stores the old base if Rn is first.
- Thumb `LDRH [rb, ro]` unaligned is 32-bit ROR 8 (was a raw halfword).
- `CpuFastSet` rounds the word count up to a multiple of 8 (GBATEK).
- BIOS is 16K only (`00004000+` is open bus, not a mirror).
- DMA0 SAD 27-bit / no Game Pak; DMA1–3 SAD 28-bit; DMA0–2 DAD 27-bit.
- IF unused bits 14–15 stay 0.
- SPARK assembler no longer `panic!`s on an unencodable immediate.
- MSR CPSR does not write T (ARM7TDMI); USR cannot MSR the control field.
- HLE boot leaves POSTFLG=1; SoftReset zeros IRQ/SVC LR+SPSR.
- Data-processing shift-by-register is +1 I-cycle.
- Modes 3–5 honor WININ/OUT BG2 bits; WININ/WINOUT/BLDCNT/TMxCNT unused bits masked.
- Gamepad / `KEYINPUT` from a host controller — **Deferred**. Keyboard only
  (`play::poll_keys`). minifb has no pad API. When LC’s campaign gate is done,
  map gilrs/evdev onto the same 10 bits; keep keys; no analog into KEYINPUT.
  Notes: [AGENTS.md](../AGENTS.md) § Input.

### Fix waves (same day, historical)

1. ARM `MSR #imm` mask `0x0FB0_F000`.
2. Savestate `FAELST03` then `FAELST04`: IO before timer shadows; frac, halt, DMA, FIFOs.
3. ROM/zip cap 32 MiB.
4. Tests: MSR, timer cascade, FIFO DMA, savestate, ROM cap, `STRB [Rn,Rm]`.
5. FIQ/UND/ABT banks; `LDM/STM ^`.
6. Sequential fetch waitstates; data waitstates.
7. Affine hacks gated.
8. `fairy_trace()` cached.
9. Clippy `-D warnings` clean (noisy lints allowed in `Cargo.toml`).
10. Addressing mode 2; dest-both A+B; 32768 Hz hold; `pw-cat` host.

---

## Findings (original text)

Severity labels below are from the first pass. Check the **Finding status** table
before treating a P0 as open.

### P0

#### 1. Savestate load rebuilds timer ctrl from the *old* IO block

`src/savestate.rs` `load()`:

```101:109:src/savestate.rs
    // Rebuild the timer ctrl shadow from the restored IO block.
    for i in 0..4 {
        emu.bus.timer_ctrl_prev[i] = emu.bus.read16(0x0400_0102 + i as u32 * 4);
    }
    emu.ppu.line = read_u16(&mut f)?;
    emu.ppu.line_cycles = read_u32(&mut f)?;
    emu.bus.ewram = read_blob(&mut f)?;
    emu.bus.iwram = read_blob(&mut f)?;
    emu.bus.io = read_blob(&mut f)?;
```

The comment says “restored IO block”; the read happens **before** `bus.io` is replaced. After F7, `timer_ctrl_prev` is the previous session’s enable/prescale (or power-on zeros). Timers then tick with the wrong control while the snapshot’s counters/reloads are applied.

**Fix:** restore IO (and `timers.frac`) first, then rebuild shadows.

#### 2. Savestate is not a machine snapshot

`FAELST02` omits DMA internal SAD/DAD/count/`active`, sound FIFOs + mixer accumulators + host ring, EEPROM serial FSM, `halt_wait` / `intr_wait_mask`, RTC GPIO, flash `cmd_step`/`mode` (bank+data only). Mid-song or mid-DMA F7 is undefined. Bump magic to `FAELST03` when this is filled in.

#### 3. ARM `MSR #imm` never decodes

```105:106:src/cpu/arm.rs
    // MSR (imm): xxxx00110_R10_field_1111_rot_imm
    if (op & 0x0DB0_F000) == 0x0320_F000 {
```

Clippy `bad_bit_mask`: bit 25 is set in `0x0320_F000` but not in the mask `0x0DB0_F000`, so the compare is **impossible**. The instruction falls through to data-processing as `TEQ`/`CMN` with `S=0` and `Rd=15` — a complete no-op. `MSR` register form still works.

Correct mask is `0x0FB0_F000` (include bit 25). Games that `MSR CPSR_c, #imm` to change mode or IRQ disable are silently ignored. LC boot did not hit this (0 unknown ops); other titles will.

#### 4. Zip / ROM load has no size cap

`src/cart.rs` `read_to_end`s the largest `.gba` in a zip with no 32 MiB GBA ceiling and no check that uncompressed size matches bytes read. A malicious zip can OOM the process.

#### 5. Unknown ARM/Thumb opcodes are silent NOPs

`cpu/arm.rs` / `cpu/thumb.rs` increment `unknown_ops` and return 1 cycle. Headless `run` prints the counter; the play window does not. A commercial game that wanders into an undecoded encoding desyncs with no trap.

#### 6. BIOS region is always readable

`bus.rs` comments that BIOS is open-bus unless executing from BIOS. Implementation always returns `bios[addr]`. Harmless with the empty HLE image. With `--bios`, ROM code can dump the BIOS — not GBA behavior, and a legal footgun if a dump is ever shipped.

#### 7. Duplicate `write16` match arms

`0x0400_0082` and `0x0400_0084` appear twice (`src/bus.rs` ~547 and ~598). Clippy `unreachable_patterns` + `match_overlapping_arm`. Second pair is dead. The IO switch is getting too large to audit by eye.

### P1

#### 8. Only USR/SYS + IRQ + SVC are banked

FIQ / UND / ABT share USR R13/R14. `MSR` into those modes corrupts USR stacks. GBA rarely uses FIQ; it is still a real bank.

#### 9. `LDM/STM` S-bit user-bank (`^` without PC) is ignored

`ldm_stm` uses `S` only for exception return when PC is in the list (`let _ = s`). User-bank STM from IRQ/SVC is a no-op on the banked registers.

#### 10. SWI never enters SVC

`bios_hle::dispatch` runs in the caller’s mode. Fine for HLE. Wrong if a game inspects CPSR/`SPSR_svc` around a SWI, or if a real BIOS is loaded (vector `0x08` is never taken).

#### 11. SoftReset / Sqrt / RegisterRamReset are stubs

- SoftReset always jumps to `0x08000000` (ignores `0x03007FFA` and the RAM wipe).
- Sqrt is `(v as f64).sqrt() as u32`.
- `RegisterRamReset` bit 1 fills all IWRAM, including the IRQ handler at `0x03007FFC`.

#### 12. HBlank is “end of 1232-cycle line,” not cycle 1006

Visible dots are 960 cycles. HBlank DMA/IRQ fire after the whole line. Mid-scanline HDMA-style effects will glitch.

#### 13. Affine identity fallback and PD caps

`Bus::ensure_affine_identity` on DISPCNT mode 1/2, plus `ppu/mod.rs` forcing `PD = 0x100` when `|PD| > 0x400` or PA/PB look degenerate. These are Liquid Crystal / battle-HUD patches. Games that *intend* a zero matrix will be rewritten.

#### 14. DMA3 special (video capture) is missing

Special timing only implements FIFO on DMA1/2. Display/video-capture DMA will not run.

#### 15. Timer overflow storms are capped at 16

`timers.rs` `extra.min(16)` on IRQ and cascade. Combined with Halt stepping 64 cycles at a time, short-period timers can drop overflows. (`Timers::on_write_reload` immediately sets `counter = reload`, which is also wrong — but clippy reports it **unused**; the live path is MMIO enable 0→1.)

#### 16. m4a BIOS HLE is a guess

`sound/bios.rs` `SoundDriverMain` assumes 64-byte channel structs, mixes **one** PCM byte per channel per SWI, ignores frequency, writes a 1024-byte cap. `music_player_*` are mostly counters. LC uses a ROM “Smsh” driver + FIFO, so this path was not exercised in the 400-frame run. Games that actually call SWI `0x1A`–`0x2B` will get wrong or silent audio.

#### 17. Waitstates are averaged, not N/S sequential

`fetch_waitstates` reports `(N+S)/2 - 1`. No 16- vs 32-bit distinction, no data-access waits on LDR/STR, no prefetch buffer. Cycle-sensitive intros (the original LC hang class) will keep appearing.

#### 18. VRAM addressing uses `% 96K`

Real GBA VRAM has specific unused-region mirrors (64K+32K), not a flat modulus.

#### 19. Unknown save type defaults to 64K SRAM

`battery::detect` falls back to SRAM so casual homebrew can save. An EEPROM/Flash game without an SDK string can corrupt the cart protocol and the `.sav`. `SaveType::Eeprom512` is never constructed (always 8K + later auto-narrow).

#### 20. Flash IDs / erase are approximate

Hardcoded Sanyo/SST IDs; erase-prep does not require a full second unlock; raw writes in mode 0 are allowed “for homebrew.”

### P2

#### 21. `FAIRY_DMA_TRACE` env lookup on the hot path

CPU caches the flag (`OnceLock`). `bus.rs` IWRAM writes, DMA CNT writes, FIFO refills, and several IO writes still call `std::env::var_os`. That is a syscall-shaped tax on stores to IWRAM.

#### 22. ~200 lines of Liquid Crystal PCs in `cpu/mod.rs`

Hardcoded `0x081DC…` / IWRAM mix-window dumps on the per-instruction path (gated, but still in `main`). Belongs in `src/debug/` or `cfg`.

#### 23. Docs contradict each other

- `AGENTS.md`: “RESOLVED (uncommitted)”
- `docs/SOUND_INVESTIGATION.md`: “ROOT CAUSE IDENTIFIED (game hangs)”
- README: v0.10 working audio
- `Cargo.toml`: 0.1.0
- `faeos/docs/plans/fairy-lantern.md`: still `cd ~/faeos/fairy-lantern`, and lists “m4a silent” next to host audio

#### 24. Almost no regression tests

Self-tests: 2 ALU ops, SPARK 3-frame smoke, mode-3 pixel, SRAM/EEPROM detect, ARM `LDR [PC,#0]`, Thumb BL. Missing: flags, LDM writeback, IRQ entry/return, timer cascade, FIFO DMA 4-word refill, PPU golden frames, zip loader, savestate round-trip (which would have caught finding 1). `tests/` is empty.

#### 25. No CI, no clippy/fmt gate, no rust-toolchain

#### 26. Monorepo copy will silently rot

Independent repo is canonical. `faeos/fairy-lantern` still has `sound.rs` (1309 lines) vs `src/sound/`. `build.sh install` from the independent tree writes `~/bin`; faeOS docs still point at the old path.

#### 27. Host audio is `aplay` / `pw-cat` + `kill -TERM`

Works here. Not portable. `sound/mod.rs` still says “host opens at 48 kHz and resamples”; `host.rs` pipes game rate and lets the device resample.

#### 28. Headless `run` always writes `/tmp/fairy-lantern-audio.wav` and a PPM

Fine for debugging; surprising as a default, and a shared-`/tmp` leak of whatever the ROM played.

#### 29. TUI depends on `spellbook` on PATH

The independent GitHub repo cannot pick a new ROM from the home screen without faeOS Spellbook.

#### 30. `fable.rs` `panic!`s on an unencodable ARM immediate when assembling SPARK

Internal assembler miss should be a build-time assert or `Result`, not a runtime panic.

### P3

- Thumb `LDMIA` writeback when `Rb` is in the list — **Fixed** (loaded value kept).
- ARM `LDR` to PC — **Fixed** (ARMv4, bit 0 ignored; `ldr_pc_stays_arm_on_v4`).
- `play.rs:145` `if frame_n % 30 == 0 || status.is_empty()` — both branches call `rtc.clock_string()` (clippy `if_same_then_else`).
- `arm.rs:339` leftover no-effect tuple in ROR-by-multiple-of-32; the following lines are correct.
- `.gitignore` correctly ignores `*.gba` / `roms/*`. `faeos/fairy-lantern/roms/hello.gba` exists only in the monorepo.

---

## What is in good shape

Do not bury this. The project earned LC boot with real work.

- ARM data-processing, barrel shifter (RRX, PC+12 `Rm`), long multiply, SWP, halfword/signed transfers, LDM empty-list quirk, Thumb formats 1–7 plus hi-reg/BX/BL pair — a real subset, not a toy.
- IRQ HLE (stack frame + `0x03007FFC` + `BIOS_IRQ_RETURN`) and IntrWait mirror at `0x03007FF8` are the right shape.
- FIFO special DMA: 4 words, fixed dest `0x040000A0/A4`, reload-on-enable-0→1 only — matches GBATEK and is why LC audio works.
- ARM addressing mode 2 (`STRB [Rn, Rm]`) — m4a reverb actually writes the left pcmBuffer.
- Timer prescale remainder + cascade was the documented LC boot hang; the current code is the fix.
- PPU has a real compositor (priority, WIN0/1 + OBJ window, mosaic BG/OBJ, semi-OBJ blend, modes 0–5).
- IF is write-1-to-clear; KEYCNT edge IRQ exists; GPIO RTC detect exists; zip-of-gba loader skips `__MACOSX`.
- Dual bins (`fairy` / `fairy-lantern`), XDG data dir, battery next to ROM, 59.73 Hz pacing with audio watermarks.

---

## Clippy (evidence)

`cargo clippy --all-targets --offline -- -D warnings` fails. Unique issues that are bugs, not style:

| Lint | Where | Meaning |
|---|---|---|
| `clippy::bad_bit_mask` | `cpu/arm.rs:106` | `MSR #imm` never matches (finding 3) |
| `unreachable_patterns` | `bus.rs:598`, `bus.rs:610` | duplicate SOUNDCNT arms (finding 7) |
| `clippy::match_overlapping_arm` | `bus.rs:547` | same |
| `clippy::no_effect` | `cpu/arm.rs:339` | leftover ROR statement |
| `clippy::if_same_then_else` | `play.rs:145` | dead title-clock branch |

Also unused: `Eeprom512`, `Timers::on_write_reload`, `Emu::from_path`, `Cpu::{reg,set_reg}`, sample-ring resampler (`cubic_interp` / `resample_into`) — leftover from the “host opens at 48 kHz” design.

---

## Recommended fix order (current)

Done this week: sound checkpoint, HP bars, integer Sqrt, BIOS open-bus, unk_op
log, VRAM mirrors, DMA3 capture, Flash AMD erase, unused-IO open bus, IRQ
delay, HALTCNT, CI.

Next:

1. Play LC to Violet / Sprout / Falkner. Named bugs only (drain, fade, cry, `unk_op`).
2. `FAIRY_ACCURATE_AFFINE=1` only after they approve — do not flip the default.
3. PPU golden frames + a tiny committed PCM fixture.
4. Unknown opcodes: stop silent-NOP (log in the play window; UND later).
5. Second owned ROM when they have one. Do not sync `faeos/fairy-lantern`.

---

## Method

- Read `src/` (CPU, bus, DMA, timers, IRQ, PPU, sound, savestate, cart, battery, play, TUI).
- Compared claims in `README.md`, `AGENTS.md`, `docs/SOUND_INVESTIGATION.md`, `faeos/docs/plans/fairy-lantern.md`.
- `diff -rq` vs `faeos/fairy-lantern`.
- `cargo test --offline`, `cargo clippy --all-targets --offline`, `./target/release/fairy-lantern test`.
- Headless `run --frames 200` and `--frames 400` on Liquid Crystal (user-supplied ROM; not copied into the repo).
