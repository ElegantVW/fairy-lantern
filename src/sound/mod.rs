//! GBA audio: DirectSound FIFOs + PSG → host speakers.
//!
//! Phase A design:
//! - Dual-rate FIFO pops from Timer 0/1 (SOUNDCNT_H bits 10/14).
//! - One output sample per FIFO sample-timer tick at the game rate;
//! - Host resamples to 48 kHz and opens `aplay`/`pw-cat` once. Underrun holds.


mod fifo;
mod host;
pub mod bios;
pub mod mixer;
mod psg;
mod regs;
mod resample;
mod wav;

pub use fifo::Fifo;
pub use host::HostAudio;
pub use regs::PsgRegs;

/// FIFO + mixer timing persisted in FAELST03 (host audio ring is not).
#[derive(Clone, Debug)]
pub struct SoundPlaySnap {
    pub fifo_a: Fifo,
    pub fifo_b: Fifo,
    pub dma_req_a: bool,
    pub dma_req_b: bool,
    pub stream_rate: u32,
    pub samples_out: u64,
    pub samples_from_fifo: u64,
    pub cycle_accum_out: u32,
    pub cycle_accum_psg: u32,
    pub cycle_a: u32,
    pub cycle_b: u32,
    pub cps_a: u32,
    pub cps_b: u32,
    pub cps_out: u32,
}

use mixer::Mixer;
use resample::{resample_stereo_to_host, Capture, SampleRing, HOST_RATE};

#[derive(Debug)]
pub struct Sound {
    mix: Mixer,
    ring: SampleRing,
    capture: Capture,
    host: Option<HostAudio>,
    backend: &'static str,
    /// Mirrors FIFO DMA request flags for bus/dma.
    pub dma_req_a: bool,
    pub dma_req_b: bool,
    pub stream_rate: u32,
    pub samples_out: u64,
    pub samples_from_fifo: u64,
}

impl Default for Sound {
    fn default() -> Self {
        Self::new()
    }
}

impl Sound {
    pub fn new() -> Self {
        Self {
            mix: Mixer::new(),
            ring: SampleRing::new(),
            capture: Capture::new(),
            host: None,
            backend: "none",
            dma_req_a: false,
            dma_req_b: false,
            stream_rate: 32768,
            samples_out: 0,
            samples_from_fifo: 0,
        }
    }

    pub fn samples_from_psg(&self) -> u64 {
        self.mix.psg.samples_from_psg
    }

    pub fn psg_trigger(&mut self, ch: u8, regs: &PsgRegs) {
        self.mix.psg.trigger(ch, regs, crate::sound::mixer::PSG_BASE_RATE);
    }

    pub fn psg_set_wave_byte(&mut self, offset: u8, val: u8) {
        self.mix.psg.set_wave_byte(offset, val);
    }

    pub fn backend_name(&self) -> &'static str {
        self.backend
    }

    pub fn peak_abs(&self) -> i16 {
        self.mix.peak_abs()
    }

    pub fn ring_len(&self) -> usize {
        self.ring.len()
    }

    pub fn ring_frames(&self) -> usize {
        self.ring.frames()
    }

    pub fn fifo_locked(&self) -> bool {
        self.mix.fifo_locked()
    }

    pub fn fifo_trace(&self) -> (&[i8], &[i8]) {
        (&self.mix.trace_a, &self.mix.trace_b)
    }

    /// Write A-only, B-only, and A=left/B=right 48 kHz WAVs for diagnosis.
    pub fn dump_fifo_traces(&self, dir: &std::path::Path) -> std::io::Result<()> {
        let (ta, tb) = self.fifo_trace();
        let rate = self.stream_rate.max(1);
        let scale = |s: i8| (i16::from(s)) << 7;
        let dual = |xs: &[i8]| {
            let mut o = Vec::with_capacity(xs.len() * 2);
            for &s in xs {
                let v = scale(s);
                o.push(v);
                o.push(v);
            }
            resample_stereo_to_host(&o, rate)
        };
        wav::write_wav_stereo(
            &dir.join("fairy-fifo-a.wav"),
            HOST_RATE,
            &dual(ta),
        )?;
        wav::write_wav_stereo(
            &dir.join("fairy-fifo-b.wav"),
            HOST_RATE,
            &dual(tb),
        )?;
        let n = ta.len().min(tb.len());
        let mut split = Vec::with_capacity(n * 2);
        for i in 0..n {
            split.push(scale(ta[i]));
            split.push(scale(tb[i]));
        }
        wav::write_wav_stereo(
            &dir.join("fairy-fifo-ab.wav"),
            HOST_RATE,
            &resample_stereo_to_host(&split, rate),
        )?;
        Ok(())
    }

    pub fn fifo_a_len(&self) -> usize {
        self.mix.fifo_a.len()
    }

    pub fn fifo_b_len(&self) -> usize {
        self.mix.fifo_b.len()
    }

    pub fn reset_fifo_a(&mut self) {
        self.mix.fifo_a.clear();
        self.dma_req_a = true;
    }

    pub fn reset_fifo_b(&mut self) {
        self.mix.fifo_b.clear();
        self.dma_req_b = true;
    }

    pub fn fifo_needs_dma(&self, which: u8) -> bool {
        match which {
            0 => self.mix.fifo_a.needs_dma(),
            _ => self.mix.fifo_b.needs_dma(),
        }
    }

    pub fn clear_dma_req(&mut self, which: u8) {
        if which == 0 {
            self.dma_req_a = false;
            self.mix.fifo_a.dma_req = false;
        } else {
            self.dma_req_b = false;
            self.mix.fifo_b.dma_req = false;
        }
    }

    pub fn start_host(&mut self) {
        if self.host.is_some() {
            return;
        }
        let h = HostAudio::start(self.ring.clone_handle());
        self.backend = h.backend();
        self.host = Some(h);
    }

    pub fn stop_host(&mut self) {
        if let Some(h) = self.host.take() {
            h.stop();
        }
        self.backend = "none";
    }

    pub fn dump_wav(&self, path: &std::path::Path) -> std::io::Result<()> {
        if crate::cpu::fairy_trace() {
            eprintln!(
                "MIXER fifo={} psg={} hold_a={} hold_b={} fa_len={} fb_len={} lastA={:02X} lastB={:02X} rate={}",
                self.samples_from_fifo,
                self.mix.psg.samples_from_psg,
                self.mix.fifo_a.hold_valid as u8,
                self.mix.fifo_b.hold_valid as u8,
                self.mix.fifo_a.len(),
                self.mix.fifo_b.len(),
                self.mix.fifo_a.hold as u8,
                self.mix.fifo_b.hold as u8,
                self.stream_rate,
            );
        }
        let samples: Vec<i16> = if !self.capture.is_empty() {
            self.capture.to_vec()
        } else {
            Vec::new()
        };
        // Always dump 48 kHz stereo so aplay/pw-cat don't choke on 13378 Hz.
        let rate = self.stream_rate.max(1);
        let host = resample_stereo_to_host(&samples, rate);
        wav::write_wav_stereo(path, HOST_RATE, &host)
    }

    pub fn push_fifo_a_word(&mut self, word: u32) {
        self.mix.fifo_a.push_word(word);
        if self.mix.fifo_a.len() >= 16 {
            self.dma_req_a = false;
        }
    }

    pub fn push_fifo_b_word(&mut self, word: u32) {
        self.mix.fifo_b.push_word(word);
        if self.mix.fifo_b.len() >= 16 {
            self.dma_req_b = false;
        }
    }

    pub fn push_fifo_a_half(&mut self, half: u16) {
        self.mix.fifo_a.push_half(half);
        if self.mix.fifo_a.len() >= 16 {
            self.dma_req_a = false;
        }
    }

    pub fn push_fifo_b_half(&mut self, half: u16) {
        self.mix.fifo_b.push_half(half);
        if self.mix.fifo_b.len() >= 16 {
            self.dma_req_b = false;
        }
    }

    /// Snapshot FIFO A/B + mixer timing for savestates (host ring is not saved).
    pub fn snapshot_play(&self) -> SoundPlaySnap {
        SoundPlaySnap {
            fifo_a: self.mix.fifo_a.clone(),
            fifo_b: self.mix.fifo_b.clone(),
            dma_req_a: self.dma_req_a,
            dma_req_b: self.dma_req_b,
            stream_rate: self.stream_rate,
            samples_out: self.samples_out,
            samples_from_fifo: self.samples_from_fifo,
            cycle_accum_out: self.mix.cycle_accum_out,
            cycle_accum_psg: self.mix.cycle_accum_psg,
            cycle_a: self.mix.cycle_a,
            cycle_b: self.mix.cycle_b,
            cps_a: self.mix.cps_a,
            cps_b: self.mix.cps_b,
            cps_out: self.mix.cps_out,
        }
    }

    pub fn restore_play(&mut self, s: SoundPlaySnap) {
        self.mix.fifo_a = s.fifo_a;
        self.mix.fifo_b = s.fifo_b;
        self.dma_req_a = s.dma_req_a;
        self.dma_req_b = s.dma_req_b;
        self.stream_rate = s.stream_rate;
        self.samples_out = s.samples_out;
        self.samples_from_fifo = s.samples_from_fifo;
        self.mix.cycle_accum_out = s.cycle_accum_out;
        self.mix.cycle_accum_psg = s.cycle_accum_psg;
        self.mix.cycle_a = s.cycle_a;
        self.mix.cycle_b = s.cycle_b;
        self.mix.cps_a = s.cps_a;
        self.mix.cps_b = s.cps_b;
        self.mix.cps_out = s.cps_out;
        self.mix.stream_rate = s.stream_rate;
        self.mix.samples_out = s.samples_out;
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
        let batch = self.mix.step(
            cycles,
            regs,
            timer_reload,
            timer0_ctrl,
            timer1_ctrl,
            overflows,
        );

        self.stream_rate = self.mix.stream_rate;
        self.samples_out = self.mix.samples_out;
        self.samples_from_fifo = self.mix.samples_from_fifo();
        self.dma_req_a = self.mix.fifo_a.dma_req;
        self.dma_req_b = self.mix.fifo_b.dma_req;

        if let Some(ref h) = self.host {
            // Do not advertise the default 32768 Hz until a FIFO timer has
            // locked; otherwise aplay opens, dies, and reopens at 13.4 kHz.
            if self.mix.fifo_locked() {
                h.set_game_rate(self.stream_rate);
            }
        }

        if !batch.is_empty() {
            self.capture.push_batch(&batch);
            self.ring.push_batch(&batch);
        }
    }
}

impl Drop for Sound {
    fn drop(&mut self) {
        self.stop_host();
    }
}
