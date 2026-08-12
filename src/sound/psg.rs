//! GBA PSG: two squares, wave, noise — clocked at the mixer sample rate.

use super::regs::PsgRegs;

/// Duty patterns (8 steps), GBATEK / DMG-compatible.
const DUTY: [[i8; 8]; 4] = [
    [0, 0, 0, 0, 0, 0, 0, 1], // 12.5%
    [1, 0, 0, 0, 0, 0, 0, 1], // 25%
    [1, 0, 0, 0, 0, 1, 1, 1], // 50%
    [0, 1, 1, 1, 1, 1, 1, 0], // 75%
];

#[derive(Debug, Clone)]
struct SquareCh {
    enabled: bool,
    phase: u32,
    phase_inc: u32,
    duty: u8,
    volume: u8,
    env_dir: bool,
    env_period: u8,
    env_timer: u8,
    env_clock: u32,
    length: u16,
    length_en: bool,
    sweep_period: u8,
    sweep_neg: bool,
    sweep_shift: u8,
    sweep_timer: u8,
    sweep_clock: u32,
    freq_raw: u16,
}

impl Default for SquareCh {
    fn default() -> Self {
        Self {
            enabled: false,
            phase: 0,
            phase_inc: 0,
            duty: 2,
            volume: 0,
            env_dir: false,
            env_period: 0,
            env_timer: 0,
            env_clock: 0,
            length: 0,
            length_en: false,
            sweep_period: 0,
            sweep_neg: false,
            sweep_shift: 0,
            sweep_timer: 0,
            sweep_clock: 0,
            freq_raw: 0,
        }
    }
}

#[derive(Debug, Clone)]
struct WaveCh {
    enabled: bool,
    phase: u32,
    phase_inc: u32,
    volume_code: u8,
    length: u16,
    length_en: bool,
    wave: [u8; 16],
}

impl Default for WaveCh {
    fn default() -> Self {
        Self {
            enabled: false,
            phase: 0,
            phase_inc: 0,
            volume_code: 0,
            length: 0,
            length_en: false,
            wave: [0; 16],
        }
    }
}

#[derive(Debug, Clone)]
struct NoiseCh {
    enabled: bool,
    lfsr: u16,
    timer: u32,
    period_samples: u32,
    volume: u8,
    env_dir: bool,
    env_period: u8,
    env_timer: u8,
    env_clock: u32,
    length: u16,
    length_en: bool,
    width7: bool,
}

impl Default for NoiseCh {
    fn default() -> Self {
        Self {
            enabled: false,
            lfsr: 0x7FFF,
            timer: 0,
            period_samples: 1,
            volume: 0,
            env_dir: false,
            env_period: 0,
            env_timer: 0,
            env_clock: 0,
            length: 0,
            length_en: false,
            width7: false,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct Psg {
    sq1: SquareCh,
    sq2: SquareCh,
    wave: WaveCh,
    noise: NoiseCh,
    len_clock: u32,
    pub samples_from_psg: u64,
}

impl Psg {
    pub fn trigger(&mut self, ch: u8, regs: &PsgRegs, rate: u32) {
        match ch {
            1 => trigger_square(&mut self.sq1, regs.ch1_l, regs.ch1_h, regs.ch1_x, true, rate),
            2 => trigger_square(&mut self.sq2, 0, regs.ch2_l, regs.ch2_h, false, rate),
            3 => {
                let w = &mut self.wave;
                w.wave = regs.wave;
                w.enabled = regs.ch3_l & 0x80 != 0;
                w.volume_code = ((regs.ch3_h >> 5) & 3) as u8;
                w.length = 256u16.saturating_sub(regs.ch3_h & 0xFF);
                w.length_en = regs.ch3_x & (1 << 14) != 0;
                w.phase = 0;
                let n = regs.ch3_x & 0x7FF;
                let freq = if n < 2048 {
                    65536u32 / (2048u32 - n as u32).max(1)
                } else {
                    0
                };
                w.phase_inc = freq_to_inc(freq, rate);
            }
            4 => trigger_noise(&mut self.noise, regs.ch4_l, regs.ch4_h, rate),
            _ => {}
        }
    }

    pub fn set_wave_byte(&mut self, offset: u8, val: u8) {
        self.wave.wave[(offset as usize) & 15] = val;
    }

    /// Produce one mono PSG sample at `rate` Hz. Returns scaled i16 contribution.
    pub fn sample(&mut self, regs: &PsgRegs, rate: u32) -> i16 {
        let rate = rate.max(8000);
        self.wave.wave = regs.wave;
        if regs.ch3_l & 0x80 == 0 {
            self.wave.enabled = false;
        }

        // Length clock ~256 Hz
        self.len_clock = self.len_clock.saturating_add(1);
        let len_ticks = rate / 256;
        if len_ticks > 0 && self.len_clock >= len_ticks {
            self.len_clock = 0;
            tick_length_sq(&mut self.sq1);
            tick_length_sq(&mut self.sq2);
            if self.wave.length_en && self.wave.enabled {
                if self.wave.length > 0 {
                    self.wave.length -= 1;
                } else {
                    self.wave.enabled = false;
                }
            }
            tick_length_noise(&mut self.noise);
        }

        refresh_square_freq(&mut self.sq1, regs.ch1_x, rate);
        refresh_square_freq(&mut self.sq2, regs.ch2_h, rate);
        {
            let n = regs.ch3_x & 0x7FF;
            let freq = if n < 2048 {
                65536u32 / (2048u32 - n as u32).max(1)
            } else {
                0
            };
            self.wave.phase_inc = freq_to_inc(freq, rate);
            self.wave.volume_code = ((regs.ch3_h >> 5) & 3) as u8;
            self.wave.length_en = regs.ch3_x & (1 << 14) != 0;
        }
        refresh_noise_period(&mut self.noise, regs.ch4_h, rate);

        let sndl = regs.sndl;
        let mut mix = 0i32;
        let any_pan = (sndl & 0xFF00) != 0;
        let en = |ch: u32| {
            !any_pan || (sndl & (1 << (8 + ch))) != 0 || (sndl & (1 << (12 + ch))) != 0
        };

        if en(0) {
            mix += square_tick(&mut self.sq1, rate) as i32;
        }
        if en(1) {
            mix += square_tick(&mut self.sq2, rate) as i32;
        }
        if en(2) {
            mix += wave_tick(&mut self.wave) as i32;
        }
        if en(3) {
            mix += noise_tick(&mut self.noise, rate) as i32;
        }

        if mix != 0 {
            self.samples_from_psg = self.samples_from_psg.wrapping_add(1);
        }

        let vol_r = (sndl & 7) as i32;
        let vol_l = ((sndl >> 4) & 7) as i32;
        // Average L/R master volumes (honest mono) rather than max-only.
        let vol = if vol_l == 0 && vol_r == 0 {
            7
        } else if vol_l == 0 {
            vol_r
        } else if vol_r == 0 {
            vol_l
        } else {
            (vol_l + vol_r) / 2
        };
        ((mix * vol) / 7) as i16
    }
}

fn freq_to_inc(freq_hz: u32, stream_rate: u32) -> u32 {
    if freq_hz == 0 || stream_rate == 0 {
        return 0;
    }
    ((freq_hz as u64 * 8 * 65536) / stream_rate as u64).min(u32::MAX as u64) as u32
}

fn square_hz(freq_raw: u16) -> u32 {
    let n = (freq_raw & 0x7FF) as u32;
    if n >= 2048 {
        return 0;
    }
    131072 / (2048 - n).max(1)
}

fn trigger_square(ch: &mut SquareCh, sweep: u16, env: u16, freq: u16, has_sweep: bool, rate: u32) {
    ch.enabled = true;
    ch.duty = ((env >> 6) & 3) as u8;
    ch.volume = ((env >> 12) & 0xF) as u8;
    ch.env_dir = env & (1 << 11) != 0;
    ch.env_period = (env & 7) as u8;
    ch.env_timer = ch.env_period;
    ch.env_clock = 0;
    ch.length = 64u16.saturating_sub((env >> 8) & 0x3F);
    ch.length_en = freq & (1 << 14) != 0;
    ch.freq_raw = freq & 0x7FF;
    ch.phase = 0;
    if has_sweep {
        ch.sweep_period = ((sweep >> 4) & 7) as u8;
        ch.sweep_neg = sweep & (1 << 3) != 0;
        ch.sweep_shift = (sweep & 7) as u8;
        ch.sweep_timer = ch.sweep_period;
        ch.sweep_clock = 0;
    }
    ch.phase_inc = freq_to_inc(square_hz(ch.freq_raw), rate.max(8000));
}

fn trigger_noise(ch: &mut NoiseCh, env: u16, poly: u16, rate: u32) {
    ch.enabled = true;
    ch.volume = ((env >> 12) & 0xF) as u8;
    ch.env_dir = env & (1 << 11) != 0;
    ch.env_period = (env & 7) as u8;
    ch.env_timer = ch.env_period;
    ch.env_clock = 0;
    ch.length = 64u16.saturating_sub((env >> 8) & 0x3F);
    ch.length_en = poly & (1 << 14) != 0;
    ch.width7 = poly & (1 << 3) != 0;
    ch.lfsr = 0x7FFF;
    refresh_noise_period(ch, poly, rate.max(8000));
    ch.timer = ch.period_samples;
}

fn refresh_square_freq(ch: &mut SquareCh, freq_reg: u16, rate: u32) {
    if !ch.enabled {
        return;
    }
    ch.length_en = freq_reg & (1 << 14) != 0;
    ch.freq_raw = freq_reg & 0x7FF;
    ch.phase_inc = freq_to_inc(square_hz(ch.freq_raw), rate);
}

fn refresh_noise_period(ch: &mut NoiseCh, poly: u16, rate: u32) {
    if !ch.enabled {
        return;
    }
    ch.width7 = poly & (1 << 3) != 0;
    let shift = ((poly >> 4) & 0xF) as u32;
    let div_code = (poly & 7) as u32;
    let r = if div_code == 0 { 8 } else { div_code * 16 };
    let denom = r.saturating_mul(1u32 << (shift + 1).min(23)).max(1);
    let freq = 524288u32 / denom;
    ch.period_samples = (rate / freq.max(1)).max(1);
}

fn tick_length_sq(ch: &mut SquareCh) {
    if ch.length_en && ch.enabled {
        if ch.length > 0 {
            ch.length -= 1;
        } else {
            ch.enabled = false;
        }
    }
}

fn tick_length_noise(ch: &mut NoiseCh) {
    if ch.length_en && ch.enabled {
        if ch.length > 0 {
            ch.length -= 1;
        } else {
            ch.enabled = false;
        }
    }
}

fn tick_envelope_sq(ch: &mut SquareCh, rate: u32) {
    if ch.env_period == 0 || !ch.enabled {
        return;
    }
    let step = rate / 64;
    if step == 0 {
        return;
    }
    ch.env_clock = ch.env_clock.saturating_add(1);
    if ch.env_clock < step {
        return;
    }
    ch.env_clock = 0;
    if ch.env_timer > 0 {
        ch.env_timer -= 1;
        return;
    }
    ch.env_timer = ch.env_period;
    if ch.env_dir {
        if ch.volume < 15 {
            ch.volume += 1;
        }
    } else if ch.volume > 0 {
        ch.volume -= 1;
    }
}

fn tick_envelope_noise(ch: &mut NoiseCh, rate: u32) {
    if ch.env_period == 0 || !ch.enabled {
        return;
    }
    let step = rate / 64;
    if step == 0 {
        return;
    }
    ch.env_clock = ch.env_clock.saturating_add(1);
    if ch.env_clock < step {
        return;
    }
    ch.env_clock = 0;
    if ch.env_timer > 0 {
        ch.env_timer -= 1;
        return;
    }
    ch.env_timer = ch.env_period;
    if ch.env_dir {
        if ch.volume < 15 {
            ch.volume += 1;
        }
    } else if ch.volume > 0 {
        ch.volume -= 1;
    }
}

fn tick_sweep(ch: &mut SquareCh, rate: u32) {
    if ch.sweep_period == 0 || ch.sweep_shift == 0 || !ch.enabled {
        return;
    }
    let step = rate / 128;
    if step == 0 {
        return;
    }
    ch.sweep_clock = ch.sweep_clock.saturating_add(1);
    if ch.sweep_clock < step {
        return;
    }
    ch.sweep_clock = 0;
    if ch.sweep_timer > 0 {
        ch.sweep_timer -= 1;
        return;
    }
    ch.sweep_timer = ch.sweep_period;
    let f = ch.freq_raw as i32;
    let delta = f >> ch.sweep_shift;
    let new_f = if ch.sweep_neg { f - delta } else { f + delta };
    if new_f < 0 || new_f > 2047 {
        ch.enabled = false;
    } else {
        ch.freq_raw = new_f as u16;
    }
}

fn square_tick(ch: &mut SquareCh, rate: u32) -> i16 {
    if !ch.enabled || ch.volume == 0 {
        return 0;
    }
    tick_envelope_sq(ch, rate);
    tick_sweep(ch, rate);
    ch.phase = ch.phase.wrapping_add(ch.phase_inc);
    let step = ((ch.phase >> 16) & 7) as usize;
    let hi = DUTY[ch.duty as usize & 3][step];
    if hi != 0 {
        (ch.volume as i16) * 512
    } else {
        -((ch.volume as i16) * 512)
    }
}

fn wave_tick(ch: &mut WaveCh) -> i16 {
    if !ch.enabled || ch.volume_code == 0 {
        return 0;
    }
    ch.phase = ch.phase.wrapping_add(ch.phase_inc);
    let idx = ((ch.phase >> 16) & 31) as usize;
    let byte = ch.wave[idx / 2];
    let nibble = if idx & 1 == 0 { byte >> 4 } else { byte & 0xF };
    let shift = match ch.volume_code {
        1 => 0,
        2 => 1,
        3 => 2,
        _ => 4,
    };
    (nibble as i16 - 8) << (9 - shift)
}

fn noise_tick(ch: &mut NoiseCh, rate: u32) -> i16 {
    if !ch.enabled || ch.volume == 0 {
        return 0;
    }
    tick_envelope_noise(ch, rate);
    if ch.timer > 0 {
        ch.timer -= 1;
    }
    if ch.timer == 0 {
        ch.timer = ch.period_samples.max(1);
        let bit = (ch.lfsr ^ (ch.lfsr >> 1)) & 1;
        ch.lfsr >>= 1;
        if bit != 0 {
            if ch.width7 {
                ch.lfsr |= 0x40;
            } else {
                ch.lfsr |= 0x4000;
            }
        }
    }
    let hi = ch.lfsr & 1;
    if hi != 0 {
        (ch.volume as i16) * 512
    } else {
        -((ch.volume as i16) * 512)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silent_when_disabled() {
        let mut p = Psg::default();
        let regs = PsgRegs::default();
        assert_eq!(p.sample(&regs, 32768), 0);
    }
}
