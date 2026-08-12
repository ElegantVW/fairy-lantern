//! SOUNDCNT / PSG register snapshots.

/// Snapshot of PSG-related IO for one mixer step.
#[derive(Clone, Copy, Debug, Default)]
pub struct PsgRegs {
    pub sndl: u16,
    pub sndh: u16,
    pub sndx: u16,
    /// SOUNDBIAS (0x04000088) — bits 0–9 bias level, typically 0x200.
    pub bias: u16,
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

impl PsgRegs {
    #[inline]
    pub fn master_enable(&self) -> bool {
        self.sndx & 0x80 != 0
    }

    /// FIFO A destination enables (bits 8–9): L/R. Nonzero = channel on.
    #[inline]
    pub fn fifo_a_enable(&self) -> bool {
        (self.sndh >> 8) & 3 != 0
    }

    /// FIFO B destination enables (bits 12–13).
    #[inline]
    pub fn fifo_b_enable(&self) -> bool {
        (self.sndh >> 12) & 3 != 0
    }

    /// SOUNDCNT_H bit10: 0 = Timer0 clocks FIFO A, 1 = Timer1.
    #[inline]
    pub fn fifo_a_timer1(&self) -> bool {
        self.sndh & (1 << 10) != 0
    }

    /// SOUNDCNT_H bit14: 0 = Timer0 clocks FIFO B, 1 = Timer1.
    #[inline]
    pub fn fifo_b_timer1(&self) -> bool {
        self.sndh & (1 << 14) != 0
    }

    /// DMA-A volume: bit2 = 0 → 50%, 1 → 100%.
    #[inline]
    pub fn fifo_a_full_vol(&self) -> bool {
        self.sndh & (1 << 2) != 0
    }

    /// DMA-B volume: bit3.
    #[inline]
    pub fn fifo_b_full_vol(&self) -> bool {
        self.sndh & (1 << 3) != 0
    }

    /// PSG master volume shift from SOUNDCNT_H bits 0–1.
    #[inline]
    pub fn psg_volume_shift(&self) -> u32 {
        match self.sndh & 3 {
            0 => 2, // 25%
            1 => 1, // 50%
            2 => 0, // 100%
            _ => 2, // 11 = 25% (GBATEK)
        }
    }

    /// True when any PSG channel could produce sound from registers alone.
    pub fn psg_has_signal(&self) -> bool {
        if !self.master_enable() {
            return false;
        }
        let any_pan = (self.sndl & 0xFF00) != 0;
        let en = |ch: u32| {
            !any_pan
                || (self.sndl & (1 << (8 + ch))) != 0
                || (self.sndl & (1 << (12 + ch))) != 0
        };
        (en(0) && (self.ch1_h >> 12) & 0xF != 0)
            || (en(1) && (self.ch2_h >> 12) & 0xF != 0)
            || (en(2) && self.ch3_l & 0x80 != 0 && ((self.ch3_h >> 5) & 3) != 0)
            || (en(3) && (self.ch4_h >> 12) & 0xF != 0)
    }
}
