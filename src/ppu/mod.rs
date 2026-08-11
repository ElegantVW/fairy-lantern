//! Picture processing unit — scanlines, VBlank/HBlank, DMA hooks.

pub mod render;

use crate::bus::Bus;

pub const WIDTH: usize = 240;
pub const HEIGHT: usize = 160;
/// Approx cycles per scanline (GBA ~1232); simplified.
pub const CYCLES_PER_LINE: u32 = 1232;
pub const LINES_PER_FRAME: u32 = 228; // 160 vis + 68 vblank

pub struct Ppu {
    pub line: u16,
    pub line_cycles: u32,
    pub frame: [u16; WIDTH * HEIGHT], // BGR555
    pub frame_ready: bool,
    /// Affine BG internal reference points (28.8 fixed), latched from IO when written.
    pub bg2x: i32,
    pub bg2y: i32,
    pub bg3x: i32,
    pub bg3y: i32,
}

impl Default for Ppu {
    fn default() -> Self {
        Self::new()
    }
}

impl Ppu {
    pub fn new() -> Self {
        Self {
            line: 0,
            line_cycles: 0,
            frame: [0; WIDTH * HEIGHT],
            frame_ready: false,
            bg2x: 0,
            bg2y: 0,
            bg3x: 0,
            bg3y: 0,
        }
    }

    /// Latch affine refs from IO (games wrote BG2X/Y etc.).
    pub fn latch_affine_from_bus(&mut self, bus: &Bus) {
        self.bg2x = sign_ext_28(bus.read32(0x0400_0028));
        self.bg2y = sign_ext_28(bus.read32(0x0400_002C));
        self.bg3x = sign_ext_28(bus.read32(0x0400_0038));
        self.bg3y = sign_ext_28(bus.read32(0x0400_003C));
    }

    /// Advance PPU by cpu cycles; returns true if a full frame was completed.
    pub fn step(&mut self, bus: &mut Bus, cycles: u32) -> bool {
        self.frame_ready = false;
        self.line_cycles += cycles;
        while self.line_cycles >= CYCLES_PER_LINE {
            self.line_cycles -= CYCLES_PER_LINE;
            let line = self.line;
            let entering_vblank = line == HEIGHT as u16;
            let in_visible = line < HEIGHT as u16;

            // HBlank DMA after drawing a visible line (approx at end of scanline)
            if in_visible {
                // Split borrows: copy affine state, render into frame buffer
                let affine = (self.bg2x, self.bg2y, self.bg3x, self.bg3y);
                render::render_scanline_affine(
                    affine,
                    bus,
                    line as usize,
                    &mut self.frame,
                );
                // HBlank flag
                let mut ds = bus.dispstat();
                ds = (ds & !2) | 2;
                bus.set_dispstat(ds);
                let mut dma = std::mem::take(&mut bus.dma);
                dma.on_hblank(bus);
                bus.dma = dma;
                if bus.dispstat() & (1 << 4) != 0 {
                    crate::irq::raise(bus, crate::irq::IRQ_HBLANK);
                }
            }

            self.line += 1;
            if self.line >= LINES_PER_FRAME as u16 {
                self.line = 0;
                self.frame_ready = true;
                // Reload affine refs for next frame from current IO (games may update during vblank)
                self.latch_affine_from_bus(bus);
            }
            bus.set_vcount(self.line);

            let mut ds = bus.dispstat() & !3; // clear vblank + hblank flags we'll set
            if self.line >= HEIGHT as u16 {
                ds |= 1; // VBlank flag
            }
            // V-counter match
            let lyc = (ds >> 8) & 0xFF;
            if (self.line as u16) == lyc {
                ds |= 4;
                if ds & (1 << 5) != 0 {
                    crate::irq::raise(bus, crate::irq::IRQ_VCOUNTER);
                }
            }
            bus.set_dispstat(ds);

            if entering_vblank {
                crate::irq::raise(bus, crate::irq::IRQ_VBLANK);
                let mut dma = std::mem::take(&mut bus.dma);
                dma.on_vblank(bus);
                bus.dma = dma;
            }

            // Advance affine refs by PB/PD each scanline (during visible + for next)
            if line < HEIGHT as u16 {
                let mut pb2 = bus.read16(0x0400_0022) as i16 as i32;
                let mut pd2 = bus.read16(0x0400_0026) as i16 as i32;
                let mut pb3 = bus.read16(0x0400_0032) as i16 as i32;
                let mut pd3 = bus.read16(0x0400_0036) as i16 as i32;
                // Match render.rs: if matrix is degenerate, advance as identity
                let pa2 = bus.read16(0x0400_0020) as i16 as i32;
                if pa2.abs() < 0x10 && pb2.abs() < 0x10 {
                    pb2 = 0;
                    pd2 = 0x100;
                } else if pd2.abs() < 0x10 {
                    pd2 = 0x100;
                }
                let pa3 = bus.read16(0x0400_0030) as i16 as i32;
                if pa3.abs() < 0x10 && pb3.abs() < 0x10 {
                    pb3 = 0;
                    pd3 = 0x100;
                } else if pd3.abs() < 0x10 {
                    pd3 = 0x100;
                }
                // Cap absurd PD so garbage doesn't fling the map off-screen
                if pd2.abs() > 0x400 {
                    pd2 = 0x100;
                }
                if pd3.abs() > 0x400 {
                    pd3 = 0x100;
                }
                self.bg2x = self.bg2x.wrapping_add(pb2);
                self.bg2y = self.bg2y.wrapping_add(pd2);
                self.bg3x = self.bg3x.wrapping_add(pb3);
                self.bg3y = self.bg3y.wrapping_add(pd3);
            }
        }
        self.frame_ready
    }
}

fn sign_ext_28(v: u32) -> i32 {
    let v = v & 0x0FFF_FFFF;
    if v & 0x0800_0000 != 0 {
        (v | 0xF000_0000) as i32
    } else {
        v as i32
    }
}
