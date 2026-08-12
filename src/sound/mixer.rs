//! Cycle-step mixer: one output sample per FIFO timer tick at game rate.

use super::fifo::Fifo;
use super::psg::Psg;
use super::regs::PsgRegs;

pub const GBA_CLOCK: u32 = 16_777_216;
const PSG_BASE_RATE: u32 = 32768;
const CPS_PSG: u32 = GBA_CLOCK / PSG_BASE_RATE;

#[derive(Debug)]
pub struct Mixer {
    pub fifo_a: Fifo,
    pub fifo_b: Fifo,
    pub psg: Psg,
    cycle_accum_out: u32,
    cps_out: u32,
    cycle_accum_psg: u32,
    /// Per-FIFO accumulators — pop when they reach cps.
    cycle_a: u32,
    cycle_b: u32,
    pub cps_a: u32,
    pub cps_b: u32,
    pub stream_rate: u32,
    pub samples_out: u64,
    peak_abs: i16,
    prev_en_a: bool,
    prev_en_b: bool,
    timer_on_a: bool,
    timer_on_b: bool,
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
    ) -> Vec<i16> {
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
        if !clock_a {
            self.fifo_a.invalidate_hold();
        }
        if !clock_b {
            self.fifo_b.invalidate_hold();
        }

        if !self.timer_on_a && !self.timer_on_b && !psg_active {
            return Vec::new();
        }

        let cps_out = self.cps_out.max(1);
        let master = regs.master_enable();
        let mut left = cycles;
        let mut batch = Vec::with_capacity(((cycles / cps_out) as usize) + 8);

        while left > 0 {
            let to_out = cps_out.saturating_sub(self.cycle_accum_out);
            let step = left.min(to_out).max(1);

            self.cycle_accum_out += step;
            left -= step;

            // Advance FIFO pop accumulators, keep remainder.
            if clock_a {
                self.cycle_a += step;
                while self.cycle_a >= self.cps_a.max(1) {
                    self.cycle_a -= self.cps_a.max(1);
                    self.fifo_a.pop_timer();
                }
            }
            if clock_b {
                self.cycle_b += step;
                while self.cycle_b >= self.cps_b.max(1) {
                    self.cycle_b -= self.cps_b.max(1);
                    self.fifo_b.pop_timer();
                }
            }

            if self.cycle_accum_out >= cps_out {
                self.cycle_accum_out -= cps_out;

                self.cycle_accum_psg += cps_out;
                let psg_ticks = self.cycle_accum_psg / CPS_PSG;
                self.cycle_accum_psg %= CPS_PSG;
                let mut psg_s = 0i16;
                for _ in 0..psg_ticks.min(4) {
                    psg_s = self.psg.sample(regs, PSG_BASE_RATE);
                }

                let s = mix_sample(
                    regs,
                    master,
                    clock_a && self.fifo_a.hold_valid,
                    self.fifo_a.hold,
                    clock_b && self.fifo_b.hold_valid,
                    self.fifo_b.hold,
                    psg_s,
                );
                let p = s.unsigned_abs() as i16;
                if p > self.peak_abs {
                    self.peak_abs = p;
                }
                batch.push(s);
                self.samples_out = self.samples_out.wrapping_add(1);
            }
        }
        batch
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
        if on_a && on_b {
            self.cps_out = cps_a.max(cps_b).max(128);
            self.stream_rate = rate_a.max(rate_b).max(8000);
        } else if on_a {
            self.cps_out = cps_a.max(128);
            self.stream_rate = rate_a.max(8000);
        } else if on_b {
            self.cps_out = cps_b.max(128);
            self.stream_rate = rate_b.max(8000);
        } else if regs.psg_has_signal() {
            self.cps_out = 512;
            self.stream_rate = 32768;
        }
    }

    pub fn audio_active(&self) -> bool {
        self.timer_on_a || self.timer_on_b
    }
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
) -> i16 {
    if !master {
        return 0;
    }
    let mut active = 0u32;
    let mut acc = 0i32;
    if en_a {
        active += 1;
        let sh = if regs.fifo_a_full_vol() { 7 } else { 6 };
        acc += (hold_a as i32) << sh;
    }
    if en_b {
        active += 1;
        let sh = if regs.fifo_b_full_vol() { 7 } else { 6 };
        acc += (hold_b as i32) << sh;
    }
    if psg != 0 {
        active += 1;
        acc += (psg as i32) >> regs.psg_volume_shift();
    }
    // Pre-attenuate when multiple sources active to prevent clipping
    if active > 1 {
        let scale = 2.0f32 / active as f32;
        acc = (acc as f32 * scale) as i32;
    }
    soft_clip(acc)
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
    let cyc = period.saturating_mul(presc);
    if cyc < 256 || cyc > 4096 {
        return (0, 0, false);
    }
    let r = (GBA_CLOCK / cyc).clamp(4000, 65536);
    (cyc, r, true)
}

#[inline]
fn soft_clip(x: i32) -> i16 {
    let x = x.clamp(-65536, 65536);
    if x > 31000 {
        (31000 + (x - 31000) / 6).min(32767) as i16
    } else if x < -31000 {
        (-31000 + (x + 31000) / 6).max(-32768) as i16
    } else {
        x as i16
    }
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
        regs.sndh = (1 << 8) | (1 << 2);
        let s = mix_sample(&regs, true, true, 0x7F, false, 0, 0);
        assert!(s.abs() > 10000, "s={s}");
    }

    #[test]
    fn held_sample_stable() {
        let regs = PsgRegs::default();
        let s0 = mix_sample(&regs, true, true, 64, false, 0, 0);
        let s1 = mix_sample(&regs, true, true, 64, false, 0, 0);
        assert_eq!(s0, s1, "held sample must be stable without HPF");
    }
}
