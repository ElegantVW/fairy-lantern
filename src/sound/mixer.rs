//! Cycle-step mixer: one output sample per FIFO timer tick at game rate.

use super::fifo::Fifo;
use super::psg::Psg;
use super::regs::PsgRegs;

pub const GBA_CLOCK: u32 = 16_777_216;
pub const PSG_BASE_RATE: u32 = 32768;
const CPS_PSG: u32 = GBA_CLOCK / PSG_BASE_RATE;

#[derive(Debug)]
pub struct Mixer {
    pub fifo_a: Fifo,
    pub fifo_b: Fifo,
    pub psg: Psg,
    pub(super) cycle_accum_out: u32,
    pub(super) cps_out: u32,
    pub(super) cycle_accum_psg: u32,
    /// Per-FIFO accumulators — pop when they reach cps.
    pub(super) cycle_a: u32,
    pub(super) cycle_b: u32,
    pub cps_a: u32,
    pub cps_b: u32,
    pub stream_rate: u32,
    pub samples_out: u64,
    peak_abs: i16,
    prev_en_a: bool,
    prev_en_b: bool,
    timer_on_a: bool,
    timer_on_b: bool,
    /// Once a FIFO sample-timer has run, keep that rate (do not bounce to 32768).
    fifo_locked: bool,
    /// Raw FIFO holds at the sample clock (for A/B diagnostics).
    pub trace_a: Vec<i8>,
    pub trace_b: Vec<i8>,
    scratch: Vec<i16>,
}

impl Default for Mixer {
    fn default() -> Self {
        Self::new()
    }
}

impl Mixer {
    pub fn new() -> Self {
        Self {
            fifo_a: Fifo::new(),
            fifo_b: Fifo::new(),
            psg: Psg::default(),
            cycle_accum_out: 0,
            cps_out: 512,
            cycle_accum_psg: 0,
            cycle_a: 0,
            cycle_b: 0,
            cps_a: 512,
            cps_b: 512,
            stream_rate: 32768,
            samples_out: 0,
            peak_abs: 0,
            prev_en_a: false,
            prev_en_b: false,
            timer_on_a: false,
            timer_on_b: false,
            fifo_locked: false,
            trace_a: Vec::new(),
            trace_b: Vec::new(),
            scratch: Vec::with_capacity(64),
        }
    }

    pub fn peak_abs(&self) -> i16 {
        self.peak_abs
    }

    pub fn samples_from_fifo(&self) -> u64 {
        self.fifo_a
            .samples_consumed
            .wrapping_add(self.fifo_b.samples_consumed)
    }

    pub fn step(
        &mut self,
        cycles: u32,
        regs: &PsgRegs,
        timer_reload: [u16; 4],
        timer0_ctrl: u16,
        timer1_ctrl: u16,
        overflows: [u32; 4],
    ) {
        self.scratch.clear();
        self.update_rates(regs, timer_reload, timer0_ctrl, timer1_ctrl);

        let psg_active = regs.psg_has_signal();
        let en_a = regs.fifo_a_enable();
        let en_b = regs.fifo_b_enable();

        if self.prev_en_a && !en_a {
            self.fifo_a.clear();
            self.fifo_a.invalidate_hold();
        }
        if self.prev_en_b && !en_b {
            self.fifo_b.clear();
            self.fifo_b.invalidate_hold();
        }
        self.prev_en_a = en_a;
        self.prev_en_b = en_b;

        let clock_a = en_a && self.timer_on_a;
        let clock_b = en_b && self.timer_on_b;
        if !en_a {
            self.fifo_a.invalidate_hold();
        }
        if !en_b {
            self.fifo_b.invalidate_hold();
        }

        let ov_a = if clock_a {
            if regs.fifo_a_timer1() {
                overflows[1]
            } else {
                overflows[0]
            }
        } else {
            0
        };
        let ov_b = if clock_b {
            if regs.fifo_b_timer1() {
                overflows[1]
            } else {
                overflows[0]
            }
        } else {
            0
        };
        // Emit at the faster FIFO. A slice can report more pops on the
        // slower timer at start-up; max() does not drop those bytes.
        let ov_out = ov_a.max(ov_b);

        if !self.timer_on_a && !self.timer_on_b && !psg_active {
            return;
        }

        let master = regs.master_enable();

        // Tick PSG for this slice once; fold into each emitted sample.
        self.cycle_accum_psg += cycles;
        let mut psg_s = 0i16;
        let mut psg_n = 0u32;
        while self.cycle_accum_psg >= CPS_PSG {
            self.cycle_accum_psg -= CPS_PSG;
            psg_s = self.psg.sample(regs, PSG_BASE_RATE);
            psg_n += 1;
        }
        if psg_n == 0 {
            psg_s = 0;
        }

        // One host sample per FIFO timer tick (~13.4 kHz on BPRE/Emerald).
        // Advertising 32768 Hz while the interpreter could not feed it
        // starved pw-cat (silence holes = tremolo) and the play loop
        // sprinted extra frames to refill (felt "crushed").
        if ov_out > 0 && (clock_a || clock_b) {
            let ov_out = ov_out.min(4096);
            for i in 0..ov_out {
                let pa = ((i + 1) * ov_a) / ov_out - (i * ov_a) / ov_out;
                let pb = ((i + 1) * ov_b) / ov_out - (i * ov_b) / ov_out;
                for _ in 0..pa {
                    self.fifo_a.pop_timer();
                }
                for _ in 0..pb {
                    self.fifo_b.pop_timer();
                }
                if self.trace_a.len() < 300_000 {
                    self.trace_a.push(self.fifo_a.hold);
                    self.trace_b.push(self.fifo_b.hold);
                }
                let (l, r) = if audio_sine() {
                    sine_pair(self.samples_out, self.stream_rate.max(1))
                } else {
                    let use_a = en_a && self.fifo_a.hold_valid && ds_want_a();
                    let use_b = en_b && self.fifo_b.hold_valid && ds_want_b();
                    mix_sample(
                        regs,
                        master,
                        use_a,
                        self.fifo_a.hold,
                        use_b,
                        self.fifo_b.hold,
                        psg_s,
                    )
                };
                let p = l.unsigned_abs().max(r.unsigned_abs()) as i16;
                if p > self.peak_abs {
                    self.peak_abs = p;
                }
                self.scratch.push(l);
                self.scratch.push(r);
                self.samples_out = self.samples_out.wrapping_add(1);
            }
            return;
        }

        // PSG-only (no FIFO clock this slice): emit at 32768 Hz.
        if psg_active && !clock_a && !clock_b {
            let cps_out = self.cps_out.max(1);
            let mut left = cycles;
            while left > 0 {
                let to_out = cps_out.saturating_sub(self.cycle_accum_out);
                let step = left.min(to_out).max(1);
                self.cycle_accum_out += step;
                left -= step;
                if self.cycle_accum_out >= cps_out {
                    self.cycle_accum_out -= cps_out;
                    let (l, r) = mix_sample(regs, master, false, 0, false, 0, psg_s);
                    self.scratch.push(l);
                    self.scratch.push(r);
                    self.samples_out = self.samples_out.wrapping_add(1);
                }
            }
        }
    }

    pub fn last_batch(&self) -> &[i16] {
        &self.scratch
    }

    fn update_rates(
        &mut self,
        regs: &PsgRegs,
        timer_reload: [u16; 4],
        timer0_ctrl: u16,
        timer1_ctrl: u16,
    ) {
        let (cps_a, rate_a, on_a) = timer_cps_rate(
            timer_reload[if regs.fifo_a_timer1() { 1 } else { 0 }],
            if regs.fifo_a_timer1() {
                timer1_ctrl
            } else {
                timer0_ctrl
            },
        );
        let (cps_b, rate_b, on_b) = timer_cps_rate(
            timer_reload[if regs.fifo_b_timer1() { 1 } else { 0 }],
            if regs.fifo_b_timer1() {
                timer1_ctrl
            } else {
                timer0_ctrl
            },
        );
        self.timer_on_a = on_a;
        self.timer_on_b = on_b;
        if on_a {
            self.cps_a = cps_a;
        }
        if on_b {
            self.cps_b = cps_b;
        }
        // Emit at the faster FIFO tick (smaller period). The slower FIFO
        // sample-and-holds. stream_rate is exactly GBA_CLOCK / cps_out.
        let tick = if on_a && on_b {
            self.fifo_locked = true;
            cps_a.min(cps_b)
        } else if on_a {
            self.fifo_locked = true;
            cps_a
        } else if on_b {
            self.fifo_locked = true;
            cps_b
        } else if self.fifo_locked {
            return;
        } else if regs.psg_has_signal() {
            CPS_PSG
        } else {
            return;
        };
        let _ = (rate_a, rate_b);
        self.cps_out = tick;
        self.stream_rate = (GBA_CLOCK / tick).max(1);
    }

    pub fn audio_active(&self) -> bool {
        self.timer_on_a || self.timer_on_b
    }

    pub fn fifo_locked(&self) -> bool {
        self.fifo_locked
    }
}

fn ds_mode() -> u8 {
    static M: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *M.get_or_init(|| {
        match std::env::var("FAIRY_DS")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "a" | "a-only" => 1,
            "b" | "b-only" => 2,
            _ => 3,
        }
    })
}

fn ds_want_a() -> bool {
    ds_mode() & 1 != 0
}

fn ds_want_b() -> bool {
    ds_mode() & 2 != 0
}

fn audio_sine() -> bool {
    static S: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *S.get_or_init(|| {
        matches!(
            std::env::var("FAIRY_AUDIO").unwrap_or_default().to_ascii_lowercase().as_str(),
            "sine" | "tone" | "beep"
        )
    })
}

fn sine_pair(n: u64, rate: u32) -> (i16, i16) {
    let t = n as f64 / f64::from(rate.max(1));
    let s = (t * 440.0 * std::f64::consts::TAU).sin() * 9000.0;
    let v = s as i16;
    (v, v)
}

#[inline]
fn mix_sample(
    regs: &PsgRegs,
    master: bool,
    en_a: bool,
    hold_a: i8,
    en_b: bool,
    hold_b: i8,
    psg: i16,
) -> (i16, i16) {
    if !master {
        return (0, 0);
    }
    // GBATEK / mGBA: each FIFO spans ±0x200 at 100% (sample<<2), ±0x100 at 50%.
    // Dest bits are enables, not a stereo hint — A and B add per speaker.
    let mut left = 0i32;
    let mut right = 0i32;
    if en_a {
        let v = fifo_amp(hold_a, regs.fifo_a_full_vol());
        if regs.fifo_a_left() {
            left += v;
        }
        if regs.fifo_a_right() {
            right += v;
        }
    }
    if en_b {
        let v = fifo_amp(hold_b, regs.fifo_b_full_vol());
        if regs.fifo_b_left() {
            left += v;
        }
        if regs.fifo_b_right() {
            right += v;
        }
    }
    if psg != 0 {
        let p = (psg as i32) >> regs.psg_volume_shift();
        left += p;
        right += p;
    }
    (bias_out(left), bias_out(right))
}

#[inline]
fn fifo_amp(hold: i8, full: bool) -> i32 {
    let s = i32::from(hold);
    if full {
        s << 2
    } else {
        s << 1
    }
}

/// Add SOUNDBIAS (0x200), fold dest-both peaks instead of squaring at 10-bit.
#[inline]
fn bias_out(sample: i32) -> i16 {
    let s = sample + 0x200;
    let folded = if s > 0x3FF {
        0x3FF - (s - 0x3FF) / 4
    } else if s < 0 {
        s / 4
    } else {
        s
    };
    let folded = folded.clamp(0, 0x3FF);
    ((folded - 0x200) * 40) as i16
}

/// Slow DC blocker. Hardware / mGBA do not do this on the FIFO mix —
/// an HPF at the FIFO rate (~8 Hz) pumped dest-both BGM (Rustboro).
#[cfg(test)]
#[inline]
fn dc_block(dc: &mut i32, x: i16) -> i16 {
    let x = i32::from(x);
    *dc += (x - *dc) / 256;
    (x - *dc) as i16
}

pub fn timer_cps_rate(reload: u16, ctrl: u16) -> (u32, u32, bool) {
    if ctrl & 0x80 == 0 {
        return (0, 0, false);
    }
    let period = 0x1_0000u32.saturating_sub(reload as u32).max(1);
    let presc = match ctrl & 3 {
        0 => 1u32,
        1 => 64,
        2 => 256,
        _ => 1024,
    };
    let cyc = period.saturating_mul(presc).max(1);
    let r = (GBA_CLOCK / cyc).max(1);
    (cyc, r, true)
}

/// Old AGC: fast attack / slow release. A footstep peak pulled the
/// envelope above CEIL and ducked BGM for tens of ms — tremolo.
#[cfg(test)]
fn limit_sample_pumps(env: &mut i32, x: i16) -> i16 {
    let y = i32::from(x);
    let a = y.abs();
    if a > *env {
        *env = a;
    } else {
        *env = (*env * 255) / 256;
    }
    const CEIL: i32 = 10_000;
    let y = if *env > CEIL { y * CEIL / *env } else { y };
    y.clamp(-32767, 32767) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_rate_13378ish() {
        let reload = (0x10000u32 - 1254) as u16;
        let (cps, rate, on) = timer_cps_rate(reload, 0x80);
        assert!(on);
        assert_eq!(cps, 1254);
        assert!((13000..14000).contains(&rate), "rate={rate}");
    }

    #[test]
    fn ds_full_vol_scaling() {
        let mut regs = PsgRegs::default();
        regs.sndx = 0x80;
        regs.sndh = (1 << 8) | (1 << 9) | (1 << 2); // A L+R, full vol
        let (l, r) = mix_sample(&regs, true, true, 0x7F, false, 0, 0);
        assert_eq!(l, r);
        assert!(l.abs() > 10000, "l={l}");
    }

    #[test]
    fn both_fifos_lr_stay_apart() {
        let mut regs = PsgRegs::default();
        regs.sndx = 0x80;
        // A left, B right — must not fold into one speaker
        regs.sndh = (1 << 9) | (1 << 2) | (1 << 12) | (1 << 3);
        let (l, r) = mix_sample(&regs, true, true, 0x40, true, 0x20, 0);
        assert!(l.abs() > 0 && r.abs() > 0, "l={l} r={r}");
        assert_ne!(l, r, "left and right are different streams");
    }

    #[test]
    fn dest_both_sums_a_and_b() {
        let mut regs = PsgRegs::default();
        regs.sndx = 0x80;
        regs.sndh = (3 << 8) | (3 << 12) | (1 << 2) | (1 << 3);
        let a_only = mix_sample(&regs, true, true, 0x10, false, 0, 0);
        let dest_both = mix_sample(&regs, true, true, 0x10, true, 0x10, 0);
        assert_ne!(dest_both, a_only, "dest-both must add FIFO B (mp2k left) into the speakers");
        assert!(dest_both.0.abs() > a_only.0.abs());
    }

    #[test]
    fn agc_ducks_quiet_after_peak() {
        let mut env = 0i32;
        let _ = limit_sample_pumps(&mut env, 20_000);
        let ducked = limit_sample_pumps(&mut env, 4_000);
        assert!(
            ducked.abs() < 4_000,
            "legacy AGC must duck the follow-up ({ducked})"
        );
    }

    #[test]
    fn held_sample_stable() {
        let regs = PsgRegs::default();
        let s0 = mix_sample(&regs, true, true, 64, false, 0, 0);
        let s1 = mix_sample(&regs, true, true, 64, false, 0, 0);
        assert_eq!(s0, s1, "held sample must be stable without HPF");
    }

    #[test]
    fn slow_timer_still_clocks() {
        let reload = (0x10000u32 - 8000) as u16; // 8000-cycle period, ~2 kHz
        let (cps, _rate, on) = timer_cps_rate(reload, 0x80);
        assert!(on);
        assert_eq!(cps, 8000);
    }

    #[test]
    fn dual_rate_stream_matches_emit_period() {
        let mut mix = Mixer::new();
        let mut regs = PsgRegs::default();
        regs.sndx = 0x80;
        // A: Timer0 dest L+R; B: Timer1 dest L+R
        regs.sndh = (3 << 8) | (3 << 12) | (1 << 14);
        let reload = [
            (0x10000u32 - 1254) as u16,
            (0x10000u32 - 512) as u16,
            0,
            0,
        ];
        for _ in 0..8 {
            mix.fifo_a.push_word(0x1010_1010);
            mix.fifo_b.push_word(0x2020_2020);
        }
        mix.step(512 * 8, &regs, reload, 0x80, 0x80, [3, 8, 0, 0]);
        let out = mix.last_batch();
        assert!(!out.is_empty());
        assert_eq!(mix.cps_out, 512, "emit at the faster FIFO");
        assert_eq!(mix.stream_rate, GBA_CLOCK / mix.cps_out);
        assert_eq!(mix.stream_rate, 32768);
        assert_eq!(out.len(), 16, "8 stereo frames");
    }

    #[test]
    fn fifo_rate_stays_after_timer_off() {
        let mut mix = Mixer::new();
        let mut regs = PsgRegs::default();
        regs.sndx = 0x80;
        regs.sndh = 1 << 8;
        let reload = [(0x10000u32 - 1254) as u16, 0, 0, 0];
        mix.fifo_a.push_word(0x1010_1010);
        mix.step(1254 * 4, &regs, reload, 0x80, 0, [4, 0, 0, 0]);
        assert!(mix.fifo_locked());
        let locked = mix.stream_rate;
        assert_eq!(locked, GBA_CLOCK / 1254, "output follows the FIFO timer");
        assert!((13_000..14_000).contains(&locked), "locked={locked}");
        // Timers disabled; PSG master still on — must not bounce to 32768.
        mix.step(1254 * 4, &regs, reload, 0, 0, [0, 0, 0, 0]);
        assert_eq!(mix.stream_rate, locked);
    }

    #[test]
    fn fifo_pcm_fixture_scales() {
        let mut mix = Mixer::new();
        let mut regs = PsgRegs::default();
        regs.sndx = 0x80;
        regs.sndh = (1 << 8) | (1 << 9) | (1 << 2); // A L+R, full vol
        mix.fifo_a.push_word(u32::from_le_bytes([0x10, 0x20, 0x30, 0x40]));
        let reload = [(0x10000u32 - 512) as u16, 0, 0, 0];
        mix.step(512 * 4, &regs, reload, 0x80, 0, [4, 0, 0, 0]);
        let out = mix.last_batch();
        assert_eq!(out.len(), 8);
        let expect = |s: i8| mix_sample(&regs, true, true, s, false, 0, 0).0;
        // Each FIFO byte must stay ordered and nonzero.
        assert!(out[0] != 0 && out[2] != 0 && out[4] != 0 && out[6] != 0);
        assert!(expect(0x10) != 0);
        assert!(out[6].abs() >= out[0].abs());
    }

    #[test]
    fn no_silent_emit_between_fifo_overflows() {
        let mut mix = Mixer::new();
        let mut regs = PsgRegs::default();
        regs.sndx = 0x80;
        regs.sndh = (1 << 8) | (1 << 9) | (1 << 2);
        let reload = [(0x10000u32 - 1254) as u16, 0, 0, 0];
        mix.fifo_a.push_word(u32::from_le_bytes([0x10, 0x20, 0x30, 0x40]));
        // One overflow → one stereo frame at the FIFO rate.
        mix.step(512, &regs, reload, 0x80, 0, [1, 0, 0, 0]);
        let hit = mix.last_batch().to_vec();
        assert_eq!(hit.len(), 2);
        assert_ne!(hit[0], 0, "first FIFO byte must be audible");
        // No overflow this slice: emit nothing (host ZOH holds the last frame).
        mix.step(512, &regs, reload, 0x80, 0, [0, 0, 0, 0]);
        assert!(
            mix.last_batch().is_empty(),
            "do not invent PWM samples between pops"
        );
        mix.step(512, &regs, reload, 0x80, 0, [1, 0, 0, 0]);
        let hit2 = mix.last_batch();
        assert_eq!(hit2.len(), 2);
        assert_ne!(hit2[0], 0);
    }

    #[test]
    fn dest_both_soft_folds_instead_of_pwm_rail() {
        let mut regs = PsgRegs::default();
        regs.sndx = 0x80;
        // A+B both speakers at 100% — hardware would square at 10-bit.
        regs.sndh = (3 << 8) | (3 << 12) | (1 << 2) | (1 << 3);
        let (l, r) = mix_sample(&regs, true, true, 0x7F, true, 0x7F, 0);
        assert_eq!(l, r);
        // Hard 10-bit clip then *40 is 20440. Fold must sit well below that.
        assert!(l.abs() < 18_000, "dest-both peak should fold, got {l}");
        assert!(l.abs() > 8_000, "still audible, got {l}");
    }

    #[test]
    fn dc_block_does_not_duck_after_peak() {
        let mut dc = 0i32;
        let _ = dc_block(&mut dc, 20_000);
        let follow = dc_block(&mut dc, 4_000);
        assert!(
            follow.abs() > 3_500,
            "DC block must not AGC-duck the next note ({follow})"
        );
    }
}
