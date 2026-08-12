//! GBA audio: DirectSound FIFOs + PSG → host speakers.
//!
//! Phase A design:
//! - Dual-rate FIFO pops from Timer 0/1 (SOUNDCNT_H bits 10/14).
//! - One output sample per FIFO sample-timer tick at the game rate;
//! - Host opens at 48 kHz and resamples from game rate; underrun → silence (no sticky SFX).


mod fifo;
mod host;
pub mod bios;
pub mod mixer;
mod psg;
mod regs;
mod resample;
mod wav;

pub use regs::PsgRegs;

use host::HostAudio;
use mixer::Mixer;
use resample::{Capture, SampleRing};

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
        self.mix.psg.trigger(ch, regs, self.stream_rate.max(8000));
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
        if std::env::var_os("FAIRY_DMA_TRACE").is_some() {
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
            // Live ring holds mix-rate samples.
            Vec::new()
        };
        // Capture rate matches the WAV header.
        wav::write_wav_mono(path, self.stream_rate.max(8000), &samples)
    }

    pub fn push_fifo_a_word(&mut self, word: u32) {
        self.mix.fifo_a.push_word(word);
        if self.mix.fifo_a.len() > 16 {
            self.dma_req_a = false;
        }
    }

    pub fn push_fifo_b_word(&mut self, word: u32) {
        self.mix.fifo_b.push_word(word);
        if self.mix.fifo_b.len() > 16 {
            self.dma_req_b = false;
        }
    }

    pub fn push_fifo_a_half(&mut self, half: u16) {
        self.mix.fifo_a.push_half(half);
        if self.mix.fifo_a.len() > 16 {
            self.dma_req_a = false;
        }
    }

    pub fn push_fifo_b_half(&mut self, half: u16) {
        self.mix.fifo_b.push_half(half);
        if self.mix.fifo_b.len() > 16 {
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
        let batch = self
            .mix
            .step(cycles, regs, timer_reload, timer0_ctrl, timer1_ctrl);

        self.stream_rate = self.mix.stream_rate;
        self.samples_out = self.mix.samples_out;
        self.samples_from_fifo = self.mix.samples_from_fifo();
        self.dma_req_a = self.mix.fifo_a.dma_req;
        self.dma_req_b = self.mix.fifo_b.dma_req;

        if let Some(ref h) = self.host {
            h.set_game_rate(self.stream_rate);
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
