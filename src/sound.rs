//! GBA audio: DirectSound FIFOs + PSG (square/wave/noise) → host speakers.
//!
//! Design: emit **one host sample per GBA audio tick** at the GBA sample rate
//! (typically ~13–33 kHz from Timer 0/1). Let ALSA/PipeWire resample.
//! Never hold/repeat a sample on underrun; feed silence so aplay does not loop.

use std::collections::VecDeque;
use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

/// Hardware FIFO = 8 words × 4 samples = 32 samples.
const FIFO_CAP: usize = 32;
const GBA_CLOCK: u32 = 16_777_216;
/// Fallback when timer is not programmed.
const DEFAULT_RATE: u32 = 32768;
/// Host player runs at this fixed rate; the ring is linearly resampled to it.
const HOST_RATE: u32 = 44100;
/// Live host ring: ~0.5 s at 48 kHz worst case.
const RING_CAP: usize = 48_000 / 2;
/// Rolling capture for WAV dump (not drained by host) — ~6 s @ 32 kHz.
const CAPTURE_CAP: usize = 32_768 * 6;

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
    /// Phase increment per host sample (16.16 fixed).
    phase_inc: u32,
    duty: u8,
    volume: u8,
    env_dir: bool, // true = increase
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
    volume_code: u8, // 0 mute, 1 full, 2 half, 3 quarter
    length: u16,
    length_en: bool,
    wave: [u8; 16],
    pos: u8,
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
            pos: 0,
        }
    }
}

#[derive(Debug, Clone)]
struct NoiseCh {
    enabled: bool,
    lfsr: u16,
    /// Countdown in host samples until next LFSR step.
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
struct Psg {
    sq1: SquareCh,
    sq2: SquareCh,
    wave: WaveCh,
    noise: NoiseCh,
    /// Frame sequencer sub-sample accum (for length @ 256 Hz).
    len_clock: u32,
    pub samples_from_psg: u64,
}

#[derive(Debug)]
pub struct Sound {
    fifo_a: VecDeque<i8>,
    fifo_b: VecDeque<i8>,
    psg: Psg,
    /// Per-FIFO sample clocks (A/B may use different timers — SOUNDCNT_H bit10/14).
    cycle_accum_a: u32,
    cycle_accum_b: u32,
    /// Host output clock (common mix rate).
    cycle_accum_out: u32,
    cps_a: u32,
    cps_b: u32,
    cps_out: u32,
    /// Current stream rate for host player (reopen if changes a lot).
    pub stream_rate: u32,
    /// Last popped sample; only used between dual-rate ticks — cleared on underrun/reset.
    hold_a: i8,
    hold_b: i8,
    hold_a_valid: bool,
    hold_b_valid: bool,
    last_mix: i16,
    pub samples_out: u64,
    pub samples_from_fifo: u64,
    peak_abs: i16,
    ring: Arc<Mutex<VecDeque<i16>>>,
    /// Rolling history of mixed samples for diagnostics (not drained by host).
    capture: VecDeque<i16>,
    host: Option<HostAudio>,
    backend: &'static str,
    /// Request DMA1/2 after consume (half-empty).
    pub dma_req_a: bool,
    pub dma_req_b: bool,
    /// Previous enable bits so we can drain FIFO when a channel is turned off.
    prev_en_a: bool,
    prev_en_b: bool,
    /// Selected sample timers currently running (FIFO must not clock when false).
    timer_on_a: bool,
    timer_on_b: bool,
}

struct HostAudio {
    join: Option<JoinHandle<()>>,
    stop: Arc<Mutex<bool>>,
    player_pid: Arc<Mutex<Option<u32>>>,
    rate: Arc<Mutex<u32>>,
}

impl std::fmt::Debug for HostAudio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostAudio").finish_non_exhaustive()
    }
}

impl Default for Sound {
    fn default() -> Self {
        Self::new()
    }
}

impl Sound {
    pub fn new() -> Self {
        let cps = GBA_CLOCK / DEFAULT_RATE;
        Self {
            fifo_a: VecDeque::with_capacity(FIFO_CAP + 4),
            fifo_b: VecDeque::with_capacity(FIFO_CAP + 4),
            psg: Psg::default(),
            cycle_accum_a: 0,
            cycle_accum_b: 0,
            cycle_accum_out: 0,
            cps_a: cps,
            cps_b: cps,
            cps_out: cps,
            stream_rate: DEFAULT_RATE,
            hold_a: 0,
            hold_b: 0,
            hold_a_valid: false,
            hold_b_valid: false,
            last_mix: 0,
            samples_out: 0,
            samples_from_fifo: 0,
            peak_abs: 0,
            ring: Arc::new(Mutex::new(VecDeque::with_capacity(RING_CAP))),
            capture: VecDeque::with_capacity(CAPTURE_CAP),
            host: None,
            backend: "none",
            dma_req_a: false,
            dma_req_b: false,
            prev_en_a: false,
            prev_en_b: false,
            timer_on_a: false,
            timer_on_b: false,
        }
    }

    pub fn samples_from_psg(&self) -> u64 {
        self.psg.samples_from_psg
    }

    /// Channel init (SOUND*_X bit 15 write). `ch`: 1..=4
    pub fn psg_trigger(&mut self, ch: u8, regs: &PsgRegs) {
        match ch {
            1 => trigger_square(&mut self.psg.sq1, regs.ch1_l, regs.ch1_h, regs.ch1_x, true),
            2 => trigger_square(&mut self.psg.sq2, 0, regs.ch2_l, regs.ch2_h, false),
            3 => {
                let w = &mut self.psg.wave;
                w.wave = regs.wave;
                w.enabled = regs.ch3_l & 0x80 != 0;
                w.volume_code = ((regs.ch3_h >> 5) & 3) as u8;
                w.length = 256u16.saturating_sub(regs.ch3_h & 0xFF);
                w.length_en = regs.ch3_x & (1 << 14) != 0;
                w.pos = 0;
                w.phase = 0;
                let n = regs.ch3_x & 0x7FF;
                let freq = if n < 2048 {
                    65536u32 / (2048u32 - n as u32).max(1)
                } else {
                    0
                };
                w.phase_inc = freq_to_inc(freq, self.stream_rate);
                if regs.ch3_l & 0x80 == 0 {
                    w.enabled = false;
                } else {
                    w.enabled = true;
                }
            }
            4 => trigger_noise(&mut self.psg.noise, regs.ch4_l, regs.ch4_h, self.stream_rate),
            _ => {}
        }
    }

    /// Keep wave RAM in sync when games write 0x04000090–9F.
    pub fn psg_set_wave_byte(&mut self, offset: u8, val: u8) {
        let i = (offset as usize) & 15;
        self.psg.wave.wave[i] = val;
    }

    pub fn backend_name(&self) -> &'static str {
        self.backend
    }
    pub fn peak_abs(&self) -> i16 {
        self.peak_abs
    }
    pub fn ring_len(&self) -> usize {
        self.ring.lock().map(|r| r.len()).unwrap_or(0)
    }
    pub fn fifo_a_len(&self) -> usize {
        self.fifo_a.len()
    }
    pub fn fifo_b_len(&self) -> usize {
        self.fifo_b.len()
    }

    pub fn reset_fifo_a(&mut self) {
        self.fifo_a.clear();
        self.hold_a_valid = false;
        self.hold_a = 0;
        self.dma_req_a = true;
    }
    pub fn reset_fifo_b(&mut self) {
        self.fifo_b.clear();
        self.hold_b_valid = false;
        self.hold_b = 0;
        self.dma_req_b = true;
    }

    pub fn fifo_needs_dma(&self, which: u8) -> bool {
        // Hardware requests when FIFO has ≤16 samples (room for half-FIFO).
        // Use strict `< 16` after a fill to 16 so a second DMA can complete to 32,
        // but never request when already full (would drop oldest + skip source).
        match which {
            0 => {
                let n = self.fifo_a.len();
                n < FIFO_CAP && (self.dma_req_a || n <= 16)
            }
            _ => {
                let n = self.fifo_b.len();
                n < FIFO_CAP && (self.dma_req_b || n <= 16)
            }
        }
    }

    pub fn clear_dma_req(&mut self, which: u8) {
        if which == 0 {
            self.dma_req_a = false;
        } else {
            self.dma_req_b = false;
        }
    }

    pub fn start_host(&mut self) {
        if self.host.is_some() {
            return;
        }
        let ring = Arc::clone(&self.ring);
        let stop = Arc::new(Mutex::new(false));
        let stop2 = Arc::clone(&stop);
        let player_pid = Arc::new(Mutex::new(None));
        let pid2 = Arc::clone(&player_pid);
        // 0 = not ready; host waits until the GBA sample timer programs a real rate.
        let rate = Arc::new(Mutex::new(0u32));
        let rate2 = Arc::clone(&rate);

        let (backend, spawn_fn): (&'static str, fn(u32) -> Option<Child>) = if which("aplay") {
            ("aplay", spawn_aplay)
        } else if which("pw-cat") {
            ("pw-cat", spawn_pw_cat)
        } else {
            ("none", |_| None)
        };

        let join = thread::Builder::new()
            .name("fairy-audio".into())
            .spawn(move || host_play_loop(ring, stop2, spawn_fn, pid2, rate2))
            .ok();

        if let Some(j) = join {
            self.host = Some(HostAudio {
                join: Some(j),
                stop,
                player_pid,
                rate,
            });
            self.backend = backend;
            if backend == "none" {
                eprintln!("  audio: FAILED — need aplay or pw-cat");
            } else {
                // Start quiet; rate locks in once the game programs the sample timer.
                eprintln!(
                    "  audio: {backend} (waits for GBA sample timer, then plays at game rate)"
                );
            }
        } else {
            self.backend = "none";
            eprintln!("  audio: FAILED — host thread");
        }
    }

    pub fn dump_wav(&self, path: &std::path::Path) -> std::io::Result<()> {
        if std::env::var_os("FAIRY_DMA_TRACE").is_some() {
            eprintln!(
                "MIXER fifo={} psg={} hold_a={} hold_b={} fa_len={} fb_len={} lastA={:04X} lastB={:04X} fb_head={:02X}{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
                self.samples_from_fifo,
                self.psg.samples_from_psg,
                self.hold_a_valid as u8,
                self.hold_b_valid as u8,
                self.fifo_a.len(),
                self.fifo_b.len(),
                self.hold_a as u16,
                self.hold_b as u16,
                self.fifo_b.get(0).unwrap_or(&0),
                self.fifo_b.get(1).unwrap_or(&0),
                self.fifo_b.get(2).unwrap_or(&0),
                self.fifo_b.get(3).unwrap_or(&0),
                self.fifo_b.get(4).unwrap_or(&0),
                self.fifo_b.get(5).unwrap_or(&0),
                self.fifo_b.get(6).unwrap_or(&0),
                self.fifo_b.get(7).unwrap_or(&0),
            );
        }
        // Prefer capture (not drained by host). Fall back to live ring.
        let samples: Vec<i16> = if !self.capture.is_empty() {
            self.capture.iter().copied().collect()
        } else {
            self.ring
                .lock()
                .map(|r| r.iter().copied().collect())
                .unwrap_or_default()
        };
        write_wav_mono(path, self.stream_rate.max(8000), &samples)
    }

    pub fn stop_host(&mut self) {
        if let Some(mut h) = self.host.take() {
            if let Ok(mut s) = h.stop.lock() {
                *s = true;
            }
            if let Ok(guard) = h.player_pid.lock() {
                if let Some(pid) = *guard {
                    let _ = Command::new("kill")
                        .args(["-9", &pid.to_string()])
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status();
                }
            }
            if let Some(j) = h.join.take() {
                let _ = j.join();
            }
        }
        self.backend = "none";
    }

    pub fn push_fifo_a_word(&mut self, word: u32) {
        push_word(&mut self.fifo_a, word);
        if self.fifo_a.len() > 16 {
            self.dma_req_a = false;
        }
    }
    pub fn push_fifo_b_word(&mut self, word: u32) {
        push_word(&mut self.fifo_b, word);
        if self.fifo_b.len() > 16 {
            self.dma_req_b = false;
        }
    }
    /// 16-bit write to FIFO_A_L/H: 2 samples (low byte first).
    pub fn push_fifo_a_half(&mut self, half: u16) {
        push_half(&mut self.fifo_a, half);
        if self.fifo_a.len() > 16 {
            self.dma_req_a = false;
        }
    }
    /// 16-bit write to FIFO_B_L/H: 2 samples (low byte first).
    pub fn push_fifo_b_half(&mut self, half: u16) {
        push_half(&mut self.fifo_b, half);
        if self.fifo_b.len() > 16 {
            self.dma_req_b = false;
        }
    }

    pub fn step(
        &mut self,
        cycles: u32,
        regs: &PsgRegs,
        timer_reload: [u16; 4],
        timer0_ctrl: u16,
        timer1_ctrl: u16,
    ) {
        self.update_rates(regs.sndh, timer_reload, timer0_ctrl, timer1_ctrl);

        // Live-update wave RAM + DAC power for ch3
        self.psg.wave.wave = regs.wave;
        if regs.ch3_l & 0x80 == 0 {
            self.psg.wave.enabled = false;
        }
        // PSG plays even when no FIFO timer is programmed (real hardware does not
        // gate the DAC on the sample timers). Publish the host rate once either a
        // FIFO timer or a live PSG channel exists.
        let psg_active = psg_has_signal(regs);
        if (self.timer_on_a || self.timer_on_b || psg_active) && self.stream_rate >= 8000 {
            if let Some(ref h) = self.host {
                if let Ok(mut r) = h.rate.lock() {
                    *r = self.stream_rate;
                }
            }
        }

        let en_a = (regs.sndh >> 8) & 3 != 0;
        let en_b = (regs.sndh >> 12) & 3 != 0;
        // Channel just disabled → drop residual samples (stops sticky SFX tails)
        if self.prev_en_a && !en_a {
            self.fifo_a.clear();
            self.hold_a_valid = false;
        }
        if self.prev_en_b && !en_b {
            self.fifo_b.clear();
            self.hold_b_valid = false;
        }
        self.prev_en_a = en_a;
        self.prev_en_b = en_b;

        // Hardware only clocks a FIFO when its selected timer is enabled.
        let clock_a = en_a && self.timer_on_a;
        let clock_b = en_b && self.timer_on_b;
        if !clock_a {
            self.hold_a_valid = false;
        }
        if !clock_b {
            self.hold_b_valid = false;
        }

        // No active sample clock and no PSG signal → nothing to emit this slice.
        if !self.timer_on_a && !self.timer_on_b && !psg_active {
            return;
        }

        let cps_a = self.cps_a.max(1);
        let cps_b = self.cps_b.max(1);
        let cps_out = self.cps_out.max(1);
        let master = regs.sndx & 0x80 != 0;

        // Interleave A/B pops with host output so dual-rate does not drop samples.
        let mut left = cycles;
        let mut batch = Vec::with_capacity(((cycles / cps_out) as usize) + 8);
        while left > 0 {
            let to_a = if clock_a {
                cps_a.saturating_sub(self.cycle_accum_a)
            } else {
                u32::MAX / 4
            };
            let to_b = if clock_b {
                cps_b.saturating_sub(self.cycle_accum_b)
            } else {
                u32::MAX / 4
            };
            let to_o = cps_out.saturating_sub(self.cycle_accum_out);
            let step = left.min(to_a).min(to_b).min(to_o).max(1);
            if clock_a {
                self.cycle_accum_a += step;
            }
            if clock_b {
                self.cycle_accum_b += step;
            }
            self.cycle_accum_out += step;
            left -= step;

            if clock_a && self.cycle_accum_a >= cps_a {
                self.cycle_accum_a -= cps_a;
                self.pop_fifo_a(true);
            }
            if clock_b && self.cycle_accum_b >= cps_b {
                self.cycle_accum_b -= cps_b;
                self.pop_fifo_b(true);
            }
            if self.cycle_accum_out >= cps_out {
                self.cycle_accum_out -= cps_out;
                let s = self.mix_output(regs, master, clock_a, clock_b);
                batch.push(s);
                self.samples_out = self.samples_out.wrapping_add(1);
            }
        }
        if !batch.is_empty() {
            for &s in &batch {
                if self.capture.len() >= CAPTURE_CAP {
                    self.capture.pop_front();
                }
                self.capture.push_back(s);
            }
            if let Ok(mut ring) = self.ring.lock() {
                for s in batch {
                    if ring.len() >= RING_CAP {
                        ring.pop_front();
                    }
                    ring.push_back(s);
                }
            }
        }
    }

    #[inline]
    fn pop_fifo_a(&mut self, en: bool) {
        if !en {
            self.hold_a_valid = false;
            return;
        }
        match self.fifo_a.pop_front() {
            Some(s) => {
                self.hold_a = s;
                self.hold_a_valid = true;
                self.samples_from_fifo = self.samples_from_fifo.wrapping_add(1);
            }
            None => {
                // Underrun → true silence (never keep last sample → no sticky SFX)
                self.hold_a = 0;
                self.hold_a_valid = false;
            }
        }
        if self.fifo_a.len() <= 16 {
            self.dma_req_a = true;
        }
    }

    #[inline]
    fn pop_fifo_b(&mut self, en: bool) {
        if !en {
            self.hold_b_valid = false;
            return;
        }
        match self.fifo_b.pop_front() {
            Some(s) => {
                self.hold_b = s;
                self.hold_b_valid = true;
                self.samples_from_fifo = self.samples_from_fifo.wrapping_add(1);
            }
            None => {
                self.hold_b = 0;
                self.hold_b_valid = false;
            }
        }
        if self.fifo_b.len() <= 16 {
            self.dma_req_b = true;
        }
    }

    /// Returns (cycles_per_sample, rate_hz, timer_running).
    fn timer_cps_rate(reload: u16, ctrl: u16) -> (u32, u32, bool) {
        if ctrl & 0x80 == 0 {
            // Timer off: FIFO must not be clocked (GBATEK).
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
        // Accept a wide but sane range (~4 kHz … 65 kHz)
        if cyc < 256 || cyc > 4096 {
            return (0, 0, false);
        }
        let r = (GBA_CLOCK / cyc).clamp(4000, 65536);
        (cyc, r, true)
    }

    fn update_rates(
        &mut self,
        sndh: u16,
        timer_reload: [u16; 4],
        timer0_ctrl: u16,
        timer1_ctrl: u16,
    ) {
        // Bit10 = FIFO A timer (0=T0,1=T1); bit14 = FIFO B timer
        let a_t1 = sndh & (1 << 10) != 0;
        let b_t1 = sndh & (1 << 14) != 0;
        let (cps_a, _, on_a) = Self::timer_cps_rate(
            timer_reload[if a_t1 { 1 } else { 0 }],
            if a_t1 { timer1_ctrl } else { timer0_ctrl },
        );
        let (cps_b, _, on_b) = Self::timer_cps_rate(
            timer_reload[if b_t1 { 1 } else { 0 }],
            if b_t1 { timer1_ctrl } else { timer0_ctrl },
        );
        self.timer_on_a = on_a;
        self.timer_on_b = on_b;
        if on_a {
            self.cps_a = cps_a;
        }
        if on_b {
            self.cps_b = cps_b;
        }
        // The mixer and PSG always run at the fixed DEFAULT_RATE. FIFO A/B are
        // clocked independently at their timer rates and held (DAC sample &
        // hold) between pops, so games mixing FIFO + PSG keep PSG pitch and
        // sub-clocks (length/envelope/sweep) exact even if a FIFO timer stops.
        self.cps_out = GBA_CLOCK / DEFAULT_RATE;
    }

    fn mix_output(&mut self, regs: &PsgRegs, master: bool, clock_a: bool, clock_b: bool) -> i16 {
        let sndh = regs.sndh;
        // SOUNDCNT_H bit2 = DMA A vol (0=50%, 1=100%); bit3 = DMA B vol (same).
        // (Bits 0–1 are PSG volume, not FIFO.)
        let vol_a = if sndh & (1 << 2) != 0 { 1i32 } else { 0 }; // shift amount
        let vol_b = if sndh & (1 << 3) != 0 { 1i32 } else { 0 };
        let psg_shift = match sndh & 3 {
            0 => 2, // 25%
            1 => 1, // 50%
            2 => 0, // 100%
            _ => 2, // 11 = 25% (GBATEK)
        };

        if !master {
            self.last_mix = 0;
            return 0;
        }

        let mut acc = 0i32;
        // DirectSound samples are signed 8-bit. Scale: 50% → <<6, 100% → <<7.
        if clock_a && self.hold_a_valid {
            acc += (self.hold_a as i32) << (6 + vol_a);
        }
        if clock_b && self.hold_b_valid {
            acc += (self.hold_b as i32) << (6 + vol_b);
        }

        let psg = self.psg_sample(regs);
        if psg != 0 {
            self.psg.samples_from_psg = self.psg.samples_from_psg.wrapping_add(1);
            acc += (psg as i32) >> psg_shift;
        }

        if acc == 0 {
            self.last_mix = 0;
            return 0;
        }

        let sample = soft_clip(acc);
        let p = sample.unsigned_abs() as i16;
        if p > self.peak_abs {
            self.peak_abs = p;
        }
        self.last_mix = sample;
        sample
    }

    fn psg_sample(&mut self, regs: &PsgRegs) -> i16 {
        let rate = self.stream_rate.max(8000);
        // Length clock ~256 Hz
        self.psg.len_clock = self.psg.len_clock.saturating_add(1);
        let len_ticks = rate / 256;
        if len_ticks > 0 && self.psg.len_clock >= len_ticks {
            self.psg.len_clock = 0;
            tick_length_sq(&mut self.psg.sq1);
            tick_length_sq(&mut self.psg.sq2);
            if self.psg.wave.length_en && self.psg.wave.enabled {
                if self.psg.wave.length > 0 {
                    self.psg.wave.length -= 1;
                } else {
                    self.psg.wave.enabled = false;
                }
            }
            tick_length_noise(&mut self.psg.noise);
        }

        // Refresh frequency from regs for free-running channels (no re-trigger)
        refresh_square_freq(&mut self.psg.sq1, regs.ch1_x, rate);
        refresh_square_freq(&mut self.psg.sq2, regs.ch2_h, rate);
        {
            let n = regs.ch3_x & 0x7FF;
            let freq = if n < 2048 {
                65536u32 / (2048u32 - n as u32).max(1)
            } else {
                0
            };
            self.psg.wave.phase_inc = freq_to_inc(freq, rate);
            self.psg.wave.volume_code = ((regs.ch3_h >> 5) & 3) as u8;
            self.psg.wave.length_en = regs.ch3_x & (1 << 14) != 0;
        }
        refresh_noise_period(&mut self.psg.noise, regs.ch4_h, rate);

        let sndl = regs.sndl;
        let mut mix = 0i32;
        // Channel enables: bits 8–11 right, 12–15 left (OR). If none set, mix all
        // active channels so games that only toggle SOUNDCNT_X still make sound.
        let any_pan = (sndl & 0xFF00) != 0;
        let en = |ch: u32| {
            !any_pan
                || (sndl & (1 << (8 + ch))) != 0
                || (sndl & (1 << (12 + ch))) != 0
        };

        if en(0) {
            mix += square_tick(&mut self.psg.sq1, rate) as i32;
        }
        if en(1) {
            mix += square_tick(&mut self.psg.sq2, rate) as i32;
        }
        if en(2) {
            mix += wave_tick(&mut self.psg.wave) as i32;
        }
        if en(3) {
            mix += noise_tick(&mut self.psg.noise, rate) as i32;
        }

        // Master PSG volumes (0–7 each side) — use max of L/R as mono gain
        let vol_r = (sndl & 7) as i32;
        let vol_l = ((sndl >> 4) & 7) as i32;
        let vol = vol_l.max(vol_r).max(1);
        // square/noise outputs are roughly ±(vol*512); scale by master 0–7
        ((mix * vol) / 7) as i16
    }
}

/// Snapshot of PSG-related IO for one mixer step.
#[derive(Clone, Copy, Debug, Default)]
pub struct PsgRegs {
    pub sndl: u16,
    pub sndh: u16,
    pub sndx: u16,
    pub ch1_l: u16,
    pub ch1_h: u16,
    pub ch1_x: u16,
    pub ch2_l: u16,
    pub ch2_h: u16,
    pub ch3_l: u16,
    pub ch3_h: u16,
    pub ch3_x: u16,
    pub ch4_l: u16,
    pub ch4_h: u16,
    pub wave: [u8; 16],
}

/// True when any PSG channel could currently produce sound (from registers
/// alone — used to keep the DAC running when no FIFO timer is programmed).
fn psg_has_signal(regs: &PsgRegs) -> bool {
    if regs.sndx & 0x80 == 0 {
        return false;
    }
    let any_pan = (regs.sndl & 0xFF00) != 0;
    let en = |ch: u32| {
        !any_pan || (regs.sndl & (1 << (8 + ch))) != 0 || (regs.sndl & (1 << (12 + ch))) != 0
    };
    (en(0) && (regs.ch1_h >> 12) & 0xF != 0)
        || (en(1) && (regs.ch2_h >> 12) & 0xF != 0)
        || (en(2) && regs.ch3_l & 0x80 != 0 && ((regs.ch3_h >> 5) & 3) != 0)
        || (en(3) && (regs.ch4_h >> 12) & 0xF != 0)
}

fn freq_to_inc(freq_hz: u32, stream_rate: u32) -> u32 {    if freq_hz == 0 || stream_rate == 0 {
        return 0;
    }
    // 16.16 fixed: phase wraps at 1<<19 (8 duty steps * 65536)
    // step per sample = freq * 8 / rate in duty-steps → use phase 0..1<<19
    ((freq_hz as u64 * 8 * 65536) / stream_rate as u64).min(u32::MAX as u64) as u32
}

fn square_hz(freq_raw: u16) -> u32 {
    let n = (freq_raw & 0x7FF) as u32;
    if n >= 2048 {
        return 0;
    }
    131072 / (2048 - n).max(1)
}

fn trigger_square(ch: &mut SquareCh, sweep: u16, env: u16, freq: u16, has_sweep: bool) {
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
    // phase_inc set on next sample via refresh
    ch.phase_inc = freq_to_inc(square_hz(ch.freq_raw), 32768);
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
    refresh_noise_period(ch, poly, rate);
    ch.timer = ch.period_samples;
}

fn refresh_square_freq(ch: &mut SquareCh, freq_reg: u16, rate: u32) {
    if !ch.enabled {
        return;
    }
    // Keep length enable live; frequency may be rewritten without re-trigger
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
    // f = 524288 / r / 2^(s+1)
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
    let step = rate / 64; // envelope clock 64 Hz
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
    let step = rate / 128; // sweep ~128 Hz
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
    // ± volume * 512 → roughly comparable to DirectSound << 7 path
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
    // 32 samples of 4-bit (2 per byte)
    let idx = ((ch.phase >> 16) & 31) as usize;
    let byte = ch.wave[idx / 2];
    let nibble = if idx & 1 == 0 { byte >> 4 } else { byte & 0xF };
    let shift = match ch.volume_code {
        1 => 0,
        2 => 1,
        3 => 2,
        _ => 4,
    };
    let s = (nibble as i16 - 8) << (9 - shift); // scale similar to square
    s
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

impl Drop for Sound {
    fn drop(&mut self) {
        self.stop_host();
    }
}

#[inline]
fn soft_clip(x: i32) -> i16 {
    // DirectSound ±128<<7 = ±16384; two channels ~±32k. Mild soft-clip only.
    let x = x.clamp(-48000, 48000);
    if x > 28000 {
        (28000 + (x - 28000) / 4).min(32767) as i16
    } else if x < -28000 {
        (-28000 + (x + 28000) / 4).max(-32768) as i16
    } else {
        x as i16
    }
}

fn push_word(fifo: &mut VecDeque<i8>, word: u32) {
    for b in word.to_le_bytes() {
        if fifo.len() >= FIFO_CAP {
            fifo.pop_front();
        }
        fifo.push_back(b as i8);
    }
}

fn push_half(fifo: &mut VecDeque<i8>, half: u16) {
    for b in half.to_le_bytes() {
        if fifo.len() >= FIFO_CAP {
            fifo.pop_front();
        }
        fifo.push_back(b as i8);
    }
}

fn which(bin: &str) -> bool {
    Command::new("which")
        .arg(bin)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn write_wav_mono(path: &std::path::Path, rate: u32, samples: &[i16]) -> std::io::Result<()> {
    use std::fs::File;
    let mut f = File::create(path)?;
    let data_len = (samples.len() * 2) as u32;
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_len).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&rate.to_le_bytes())?;
    f.write_all(&(rate * 2).to_le_bytes())?;
    f.write_all(&2u16.to_le_bytes())?;
    f.write_all(&16u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_len.to_le_bytes())?;
    for s in samples {
        f.write_all(&s.to_le_bytes())?;
    }
    Ok(())
}

fn spawn_aplay(rate: u32) -> Option<Child> {
    // Modest buffer; we keep the pipe fed (real samples or silence) so ALSA
    // never underruns into “repeat last period” territory.
    Command::new("aplay")
        .args([
            "-t",
            "raw",
            "-f",
            "S16_LE",
            "-c",
            "1",
            "-r",
            &rate.to_string(),
            "--buffer-time=80000",
            "--period-time=10000",
            "-q",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()
}

fn spawn_pw_cat(rate: u32) -> Option<Child> {
    Command::new("pw-cat")
        .args([
            "--playback",
            "--raw",
            "--format",
            "s16",
            "--rate",
            &rate.to_string(),
            "--channels",
            "1",
            "--latency",
            "40ms",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()
}

fn host_play_loop(
    ring: Arc<Mutex<VecDeque<i16>>>,
    stop: Arc<Mutex<bool>>,
    spawn: fn(u32) -> Option<Child>,
    player_pid: Arc<Mutex<Option<u32>>>,
    rate_shared: Arc<Mutex<u32>>,
) {
    // The player always runs at one fixed host rate; the game-rate samples in
    // the ring are linearly resampled here. A game rate change (or odd rate
    // like 13,378 Hz) never reopens the player, so no kill → click/gap.
    let mut child: Option<Child> = None;
    let mut stdin: Option<std::process::ChildStdin> = None;
    let mut fpos = 0f64; // fractional read position in game-rate samples
    let mut out_host: u64 = 0;
    let mut start = std::time::Instant::now();
    let mut primed = false;
    let mut buf = Vec::with_capacity(8192);

    loop {
        if stop.lock().map(|s| *s).unwrap_or(true) {
            break;
        }
        let want = rate_shared.lock().map(|r| *r).unwrap_or(0);

        // Not ready yet — wait for the game to program a real audio clock.
        if want < 8000 {
            thread::sleep(std::time::Duration::from_millis(5));
            continue;
        }

        if child.is_none() {
            child = spawn(HOST_RATE);
            if let Some(ref c) = child {
                *player_pid.lock().unwrap() = Some(c.id());
            } else {
                *player_pid.lock().unwrap() = None;
                thread::sleep(std::time::Duration::from_millis(20));
                continue;
            }
            stdin = child.as_mut().and_then(|c| c.stdin.take());
            fpos = 0.0;
            out_host = 0;
            start = std::time::Instant::now();
            primed = false;
            eprintln!("  audio: host open @ {HOST_RATE} Hz (resampling game audio)");
        }

        // Buffer a little real audio before starting the wall-clock stream
        // so the intro attack is not clipped by the player's first period.
        if !primed {
            let need = (want as usize / 20).max(256); // ~50 ms of game audio
            let have = ring.lock().map(|r| r.len()).unwrap_or(0);
            if have < need {
                thread::sleep(std::time::Duration::from_millis(2));
                continue;
            }
            primed = true;
            out_host = 0;
            start = std::time::Instant::now();
        }

        // Emit the wall-clock amount of host samples for this ~10 ms slice.
        let elapsed_ns = start.elapsed().as_nanos() as u64;
        let should_have = elapsed_ns.saturating_mul(HOST_RATE as u64) / 1_000_000_000;
        let behind = (should_have.saturating_sub(out_host)) as usize;
        let take = behind.min(HOST_RATE as usize / 100).max(0);

        let ratio = want as f64 / HOST_RATE as f64;
        buf.clear();
        if take > 0 {
            let mut ring = ring.lock().unwrap();
            for _ in 0..take {
                let i = fpos as usize;
                let t = (fpos - i as f64) as f32;
                match ring_read(&ring, i) {
                    Some(v0) => {
                        let s1 = ring_read(&ring, i + 1).unwrap_or(v0);
                        let s = if t > 0.0 && s1 != v0 {
                            (v0 as f32 + (s1 as f32 - v0 as f32) * t) as i16
                        } else {
                            v0
                        };
                        buf.extend_from_slice(&s.to_le_bytes());
                        fpos += ratio;
                    }
                    None => {
                        // Underrun — keep the wall clock, feed silence.
                        buf.extend_from_slice(&0i16.to_le_bytes());
                    }
                }
                out_host = out_host.wrapping_add(1);
                while fpos >= 1.0 {
                    ring.pop_front();
                    fpos -= 1.0;
                }
            }
        }

        if buf.is_empty() {
            thread::sleep(std::time::Duration::from_millis(1));
            continue;
        }

        if let Some(ref mut sin) = stdin {
            if sin.write_all(&buf).is_err() {
                drop(stdin.take());
                if let Some(mut c) = child.take() {
                    let _ = c.kill();
                    let _ = c.wait();
                }
                *player_pid.lock().unwrap() = None;
                if stop.lock().map(|s| *s).unwrap_or(true) {
                    break;
                }
            } else {
                let _ = sin.flush();
            }
        } else {
            thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    if let Some(mut c) = child {
        let _ = c.kill();
        let _ = c.wait();
    }
    *player_pid.lock().unwrap() = None;
}

/// Random access into a VecDeque via its two internal slices.
fn ring_read(ring: &VecDeque<i16>, i: usize) -> Option<i16> {
    let (a, b) = ring.as_slices();
    if i < a.len() {
        Some(a[i])
    } else {
        let j = i - a.len();
        if j < b.len() {
            Some(b[j])
        } else {
            None
        }
    }
}
