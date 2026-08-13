//! DirectSound FIFO A/B — 32 signed 8-bit samples each.

use std::collections::VecDeque;

/// Hardware FIFO = 8 words × 4 samples = 32 samples.
pub const FIFO_CAP: usize = 32;

#[derive(Debug, Clone)]
pub struct Fifo {
    samples: VecDeque<i8>,
    /// Latched sample (DAC sample-and-hold between pops).
    pub hold: i8,
    pub hold_valid: bool,
    pub dma_req: bool,
    pub samples_consumed: u64,
}

impl Default for Fifo {
    fn default() -> Self {
        Self::new()
    }
}

impl Fifo {
    pub fn new() -> Self {
        Self {
            samples: VecDeque::with_capacity(FIFO_CAP + 4),
            hold: 0,
            hold_valid: false,
            dma_req: false,
            samples_consumed: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn clear(&mut self) {
        self.samples.clear();
        self.hold = 0;
        self.hold_valid = false;
        self.dma_req = true;
    }

    pub fn push_word(&mut self, word: u32) {
        for b in word.to_le_bytes() {
            if self.samples.len() >= FIFO_CAP {
                self.samples.pop_front();
            }
            self.samples.push_back(b as i8);
        }
        if self.samples.len() >= 16 {
            self.dma_req = false;
        }
    }

    pub fn push_half(&mut self, half: u16) {
        for b in half.to_le_bytes() {
            if self.samples.len() >= FIFO_CAP {
                self.samples.pop_front();
            }
            self.samples.push_back(b as i8);
        }
        if self.samples.len() >= 16 {
            self.dma_req = false;
        }
    }

    /// Pop one sample when the selected timer ticks. Underrun → silence (no sticky hold).
    pub fn pop_timer(&mut self) {
        match self.samples.pop_front() {
            Some(s) => {
                self.hold = s;
                self.hold_valid = true;
                self.samples_consumed = self.samples_consumed.wrapping_add(1);
            }
            None => {
                self.hold = 0;
                self.hold_valid = false;
            }
        }
        if self.samples.len() < 16 {
            self.dma_req = true;
        }
    }

    pub fn invalidate_hold(&mut self) {
        self.hold_valid = false;
        self.hold = 0;
    }

    /// Request when below half-full. A 16-sample refill does not immediately
    /// re-request (avoids a DMA every tick_sound while sitting on 16).
    pub fn needs_dma(&self) -> bool {
        let n = self.samples.len();
        n < FIFO_CAP && (self.dma_req || n < 16)
    }

    pub fn peek_head(&self, i: usize) -> i8 {
        self.samples.get(i).copied().unwrap_or(0)
    }

    pub fn samples_vec(&self) -> Vec<i8> {
        self.samples.iter().copied().collect()
    }

    pub fn restore(
        &mut self,
        samples: &[i8],
        hold: i8,
        hold_valid: bool,
        dma_req: bool,
        samples_consumed: u64,
    ) {
        self.samples.clear();
        for &s in samples.iter().take(FIFO_CAP) {
            self.samples.push_back(s);
        }
        self.hold = hold;
        self.hold_valid = hold_valid;
        self.dma_req = dma_req;
        self.samples_consumed = samples_consumed;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_word_le_order() {
        let mut f = Fifo::new();
        f.push_word(0x0403_0201);
        assert_eq!(f.len(), 4);
        assert_eq!(f.peek_head(0), 0x01);
        assert_eq!(f.peek_head(1), 0x02);
        assert_eq!(f.peek_head(2), 0x03);
        assert_eq!(f.peek_head(3), 0x04);
    }

    #[test]
    fn underrun_silence() {
        let mut f = Fifo::new();
        f.push_half(0x7F7F);
        f.pop_timer();
        assert!(f.hold_valid);
        f.pop_timer();
        f.pop_timer();
        assert!(!f.hold_valid);
        assert_eq!(f.hold, 0);
    }

    #[test]
    fn dma_req_half_empty() {
        let mut f = Fifo::new();
        for _ in 0..8 {
            f.push_word(0);
        }
        assert_eq!(f.len(), 32);
        assert!(!f.needs_dma());
        for _ in 0..16 {
            f.pop_timer();
        }
        assert!(!f.needs_dma(), "exactly 16 must not re-request");
        f.pop_timer();
        assert!(f.needs_dma());
    }

    #[test]
    fn sixteen_sample_refill_does_not_rerequest() {
        let mut f = Fifo::new();
        for _ in 0..4 {
            f.push_word(0);
        }
        assert_eq!(f.len(), 16);
        assert!(!f.needs_dma());
    }
}
