//! DMA channels 0–3 — immediate, VBlank, HBlank, special.

use crate::bus::Bus;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Timing {
    Immediate = 0,
    VBlank = 1,
    HBlank = 2,
    Special = 3,
}

#[derive(Clone, Debug, Default)]
pub struct Channel {
    pub src: u32,
    pub dst: u32,
    pub latch_src: u32,
    pub latch_dst: u32,
    pub count: u32,
    pub ctrl: u16,
    pub active: bool,
}

#[derive(Clone, Debug, Default)]
pub struct DmaController {
    pub ch: [Channel; 4],
    /// Debug: completed transfers
    pub xfer_count: u64,
}

impl DmaController {
    pub fn new() -> Self {
        Self::default()
    }

    /// Software wrote DMAxCNT_H.
    pub fn on_cnt_h_write(&mut self, bus: &mut Bus, ch: usize) {
        if ch > 3 {
            return;
        }
        let base = 0x0400_00B0 + ch as u32 * 12;
        let sad = bus.read32(base);
        let dad = bus.read32(base + 4);
        let cnt_l = bus.read16(base + 8) as u32;
        let cnt_h = bus.read16(base + 10);
        if crate::cpu::fairy_trace() {
            eprintln!(
                "cntH ch{ch} SAD={:08X} DAD={:08X} CNT_L={:04X} CNT_H={:04X} evt{}",
                sad, dad, cnt_l, cnt_h, bus.dbg_evt
            );
            if cnt_h & 0x8000 != 0 && (ch == 1 || ch == 2) {
                eprintln!(
                    "BUF @30062A0 {:08X} {:08X} @3006400 {:08X} {:08X} @3006600 {:08X} {:08X} @3006800 {:08X} {:08X} evt{}",
                    bus.read32(0x0300_62A0), bus.read32(0x0300_62A4),
                    bus.read32(0x0300_6400), bus.read32(0x0300_6404),
                    bus.read32(0x0300_6600), bus.read32(0x0300_6604),
                    bus.read32(0x0300_6800), bus.read32(0x0300_6804),
                    bus.dbg_evt
                );
            }
        }
        let c = &mut self.ch[ch];
        let was_enabled = c.ctrl & 0x8000 != 0;
        c.ctrl = cnt_h;
        if cnt_h & 0x8000 == 0 {
            c.active = false;
            return;
        }
        // GBATEK: the internal pointer/counter registers are reloaded from the
        // MMIO SAD/DAD/CNT_L only when Enable (bit 15) changes 0→1. Rewriting
        // CNT_H while already enabled (e.g. a running FIFO DMA switching to
        // immediate, 0x8440) keeps the current internal position.
        if !was_enabled {
            c.latch_src = sad;
            c.latch_dst = dad;
            c.src = sad & 0x0FFF_FFFF;
            c.dst = dad & 0x0FFF_FFFF;
            let mut count = cnt_l & 0xFFFF;
            if count == 0 {
                count = if ch == 3 { 0x1_0000 } else { 0x4000 };
            }
            c.count = count;
        }
        c.active = true;

        let timing = (cnt_h >> 12) & 3;
        if timing == Timing::Immediate as u16 {
            let mut chn = c.clone();
            run_transfer(bus, ch, &mut chn);
            self.ch[ch] = chn;
            return;
        }
        // Empty FIFO asserts DRQ; enabling special DMA1/2 must fill now
        // (up to 32 samples), not wait for the first timer underrun.
        if timing == Timing::Special as u16 && (ch == 1 || ch == 2) {
            let which = if ch == 1 { 0u8 } else { 1 };
            for _ in 0..2 {
                let n = if which == 0 {
                    bus.sound.fifo_a_len()
                } else {
                    bus.sound.fifo_b_len()
                };
                if n >= 32 {
                    break;
                }
                let mut chn = self.ch[ch].clone();
                let saved = chn.count;
                chn.count = 4;
                run_transfer(bus, ch, &mut chn);
                chn.count = saved;
                self.ch[ch] = chn;
                bus.sound.clear_dma_req(which);
            }
        }
    }

    pub fn on_vblank(&mut self, bus: &mut Bus) {
        self.run_timing(bus, Timing::VBlank);
    }

    pub fn on_hblank(&mut self, bus: &mut Bus) {
        self.run_timing(bus, Timing::HBlank);
        self.run_video_capture(bus);
    }

    /// DMA3 start=Special: one scanline per HBlank, VCOUNT 2..=161, off at 162.
    fn run_video_capture(&mut self, bus: &mut Bus) {
        const CH: usize = 3;
        if !self.ch[CH].active {
            return;
        }
        if (self.ch[CH].ctrl >> 12) & 3 != Timing::Special as u16 {
            return;
        }
        let vcount = bus.read16(0x0400_0006);
        if vcount == 162 {
            let irq = self.ch[CH].ctrl & (1 << 14) != 0;
            let new_h = self.ch[CH].ctrl & !0x8000;
            self.ch[CH].ctrl = new_h;
            self.ch[CH].active = false;
            bus.write16_raw(0x0400_00DE, new_h);
            if irq {
                crate::irq::raise(bus, crate::irq::IRQ_DMA3);
            }
            return;
        }
        if !(2..=161).contains(&vcount) {
            return;
        }
        let mut c = self.ch[CH].clone();
        run_transfer(bus, CH, &mut c);
        self.ch[CH] = c;
    }

    /// Called after sound mix when FIFOs are half-empty (sample-timer path).
    pub fn on_fifo_request(&mut self, bus: &mut Bus) {
        for ch in 1..=2 {
            if !self.ch[ch].active {
                continue;
            }
            let t = (self.ch[ch].ctrl >> 12) & 3;
            if t != Timing::Special as u16 {
                continue;
            }
            let which = if ch == 1 { 0u8 } else { 1u8 };
            if !bus.sound.fifo_needs_dma(which) {
                continue;
            }
            let mut c = self.ch[ch].clone();
            let saved = c.count;
            c.count = 4;
            if crate::cpu::fairy_trace() {
                let mut bytes = Vec::new();
                for k in 0..4 {
                    bytes.push(bus.read32(c.src + k * 4));
                }
                eprintln!(
                    "fifo ch{ch} refill src={:08X} bytes={:08X} {:08X} {:08X} {:08X} evt{}",
                    c.src, bytes[0], bytes[1], bytes[2], bytes[3], bus.dbg_evt
                );
            }
            run_transfer(bus, ch, &mut c);
            c.count = saved;
            self.ch[ch] = c;
            bus.sound.clear_dma_req(which);
            bus.dbg_evt += 1;
            if crate::cpu::fairy_trace() {
                eprintln!(
                    "fifo ch{ch} refill src={:08X} evt{}",
                    self.ch[ch].src, bus.dbg_evt
                );
            }
        }
    }

    fn run_timing(&mut self, bus: &mut Bus, timing: Timing) {
        for ch in 0..4 {
            if !self.ch[ch].active {
                continue;
            }
            let t = (self.ch[ch].ctrl >> 12) & 3;
            if t != timing as u16 {
                continue;
            }
            if timing == Timing::Special && ch == 0 {
                continue;
            }
            let mut c = self.ch[ch].clone();
            run_transfer(bus, ch, &mut c);
            self.ch[ch] = c;
        }
    }
}

fn run_transfer(bus: &mut Bus, ch: usize, c: &mut Channel) {
    let cnt_h = c.ctrl;
    if cnt_h & 0x8000 == 0 {
        c.active = false;
        return;
    }
    // GBATEK DMA0-3CNT_H layout:
    //   bits 5-6: Dest Addr Control (0=inc,1=dec,2=fixed,3=inc/reload)
    //   bits 7-8: Source Addr Control (0=inc,1=dec,2=fixed)
    //   bit 9:    Repeat
    //   bit 10:   Transfer Type (0=16bit, 1=32bit)
    //   bit 11:   Game Pak DRQ (DMA3 only — not transfer size!)
    //   bits 12-13: Start Timing (0=Immediate,1=VBlank,2=HBlank,3=Special)
    //   bit 14:   IRQ upon end of Word Count
    //   bit 15:   Enable
    let mut word = cnt_h & (1 << 10) != 0;
    let src_adj = (cnt_h >> 7) & 3;
    let mut dst_adj = (cnt_h >> 5) & 3;
    let repeat = cnt_h & (1 << 9) != 0;
    let timing = (cnt_h >> 12) & 3;
    // Sound FIFO special DMA: destination is fixed at FIFO A/B (GBATEK).
    // Transfers are always 32-bit words of 4 samples even if CNT says halfword.
    let fifo_dma = timing == 3 && (ch == 1 || ch == 2);
    if fifo_dma {
        word = true;
        dst_adj = 2; // fixed
        // Force classic FIFO addresses
        c.dst = if ch == 1 { 0x0400_00A0 } else { 0x0400_00A4 };
        c.latch_dst = c.dst;
    }
    let step = if word { 4u32 } else { 2 };
    let base = 0x0400_00B0 + ch as u32 * 12;
    // GBATEK: the transfer runs on internal pointer registers; the MMIO
    // SAD/DAD/CNT_L registers are NEVER modified by hardware (they keep the
    // software-written values). Only the enable auto-clear below touches MMIO.
    let mut src = c.src;
    let mut dst = c.dst;
    // FIFO unit is always 4 words (16 samples) per request
    let count = if fifo_dma {
        4
    } else {
        c.count.max(1)
    };

    for _ in 0..count {
        if matches!(src >> 24, 0x08..=0x0D) {
            bus.note_cart_data();
        }
        if word {
            let v = bus.read32(src & !3);
            bus.write32(dst & !3, v);
        } else {
            let v = bus.read16(src & !1);
            if (dst & !1) >= 0x0400_00B0 && (dst & !1) <= 0x0400_00DF {
                if crate::cpu::fairy_trace() {
                    eprintln!(
                        "XFER ch{ch} dma dst={:08X} v={:04X} src={:08X} cntH={:04X} count={} evt{}",
                        dst, v, src, cnt_h, count, bus.dbg_evt
                    );
                }
            }
            bus.write16(dst & !1, v);
        }
        src = adj(src, src_adj, step);
        if !fifo_dma {
            dst = adj(dst, dst_adj, step);
        }
        // fifo_dma: always write same FIFO address
    }
    // xfer_count is on controller — bumped by caller via return; use bus side effect:
    // (counted in on_event by checking active→done; keep simple here)

    c.src = src;
    if dst_adj == 3 {
        c.dst = c.latch_dst;
    } else {
        c.dst = dst;
    }

    if !repeat {
        let new_h = cnt_h & !0x8000;
        c.ctrl = new_h;
        c.active = false;
        bus.write16_raw(base + 10, new_h);
    } else {
        let cnt_l = bus.read16(base + 8) as u32;
        let mut count = cnt_l & 0xFFFF;
        if count == 0 {
            count = if ch == 3 { 0x1_0000 } else { 0x4000 };
        }
        c.count = count;
        if dst_adj == 3 {
            c.dst = c.latch_dst;
        }
    }

    if cnt_h & (1 << 14) != 0 {
        let bit = match ch {
            0 => crate::irq::IRQ_DMA0,
            1 => crate::irq::IRQ_DMA1,
            2 => crate::irq::IRQ_DMA2,
            _ => crate::irq::IRQ_DMA3,
        };
        crate::irq::raise(bus, bit);
    }
}

fn adj(addr: u32, mode: u16, step: u32) -> u32 {
    match mode {
        0 | 3 => addr.wrapping_add(step),
        1 => addr.wrapping_sub(step),
        2 => addr,
        _ => addr.wrapping_add(step),
    }
}

#[cfg(test)]
mod tests {
    use crate::cart::Cart;
    use crate::emu::Emu;

    #[test]
    fn fifo_special_refills_four_words() {
        let cart = Cart {
            data: vec![0u8; 0x200],
            title: "t".into(),
            game_code: "T".into(),
            maker: "00".into(),
            path: "m".into(),
            inner_name: None,
        };
        let mut emu = Emu::new(&cart, None);
        emu.bus.write32(0x0300_0000, 0x0403_0201);
        emu.bus.write32(0x0300_0004, 0x0807_0605);
        emu.bus.write32(0x0300_0008, 0x0C0B_0A09);
        emu.bus.write32(0x0300_000C, 0x100F_0E0D);
        // DMA1 SAD / DAD / CNT_L
        emu.bus.write32(0x0400_00BC, 0x0300_0000);
        emu.bus.write32(0x0400_00C0, 0x0400_00A0);
        emu.bus.write16(0x0400_00C4, 4);
        // enable + 32-bit + repeat + special
        emu.bus.write16(0x0400_00C6, 0xB600);

        assert!(emu.bus.dma.ch[1].active);
        // Empty FIFO DRQ: enable must fill immediately (2×4 words → 32 samples).
        assert_eq!(
            emu.bus.sound.fifo_a_len(),
            32,
            "special DMA fills FIFO on enable"
        );
        assert_eq!(emu.bus.dma.ch[1].src, 0x0300_0020);
        assert!(emu.bus.dma.ch[1].active, "repeat keeps the channel live");
    }

    #[test]
    fn dma3_video_capture_one_line_then_stops() {
        let cart = Cart {
            data: vec![0u8; 0x200],
            title: "t".into(),
            game_code: "T".into(),
            maker: "00".into(),
            path: "m".into(),
            inner_name: None,
        };
        let mut emu = Emu::new(&cart, None);
        for i in 0..8u32 {
            emu.bus.write16(0x0200_0000 + i * 2, (0x100 + i) as u16);
        }
        emu.bus.write32(0x0400_00D4, 0x0200_0000);
        emu.bus.write32(0x0400_00D8, 0x0600_0000);
        emu.bus.write16(0x0400_00DC, 4);
        // enable + repeat + special + 16-bit
        emu.bus.write16(0x0400_00DE, 0xB200);
        assert!(emu.bus.dma.ch[3].active);

        emu.bus.set_vcount(0);
        let mut dma = std::mem::take(&mut emu.bus.dma);
        dma.on_hblank(&mut emu.bus);
        emu.bus.dma = dma;
        assert_eq!(emu.bus.read16(0x0600_0000), 0, "no capture on line 0");

        emu.bus.set_vcount(2);
        let mut dma = std::mem::take(&mut emu.bus.dma);
        dma.on_hblank(&mut emu.bus);
        emu.bus.dma = dma;
        assert_eq!(emu.bus.read16(0x0600_0000), 0x100);
        assert_eq!(emu.bus.read16(0x0600_0006), 0x103);
        assert!(emu.bus.dma.ch[3].active);

        emu.bus.set_vcount(162);
        let mut dma = std::mem::take(&mut emu.bus.dma);
        dma.on_hblank(&mut emu.bus);
        emu.bus.dma = dma;
        assert!(!emu.bus.dma.ch[3].active, "capture ends at VCOUNT 162");
        assert_eq!(emu.bus.read16(0x0400_00DE) & 0x8000, 0);
    }
}
