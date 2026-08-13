//! Picture processing unit — scanlines, VBlank/HBlank, DMA hooks.

pub mod render;

use crate::bus::Bus;

pub const WIDTH: usize = 240;
pub const HEIGHT: usize = 160;
/// Cycles per scanline (GBATEK).
pub const CYCLES_PER_LINE: u32 = 1232;
/// HBlank flag / HDMA / HBlank IRQ start here (not at the end of the line).
pub const HBLANK_CYCLE: u32 = 1006;
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
    hblank_fired: bool,
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
            hblank_fired: false,
        }
    }

    pub fn sync_hblank_from_cycles(&mut self) {
        self.hblank_fired = self.line_cycles >= HBLANK_CYCLE;
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
        self.line_cycles = self.line_cycles.saturating_add(cycles);
        loop {
            if !self.hblank_fired && self.line_cycles >= HBLANK_CYCLE {
                self.enter_hblank(bus);
                self.hblank_fired = true;
            }
            if self.line_cycles < CYCLES_PER_LINE {
                break;
            }
            self.line_cycles -= CYCLES_PER_LINE;
            self.finish_line(bus);
            self.hblank_fired = false;
        }
        self.frame_ready
    }

    fn enter_hblank(&mut self, bus: &mut Bus) {
        let in_visible = self.line < HEIGHT as u16;
        if in_visible {
            let affine = (self.bg2x, self.bg2y, self.bg3x, self.bg3y);
            render::render_scanline_affine(
                affine,
                bus,
                self.line as usize,
                &mut self.frame,
            );
        }
        let mut ds = bus.dispstat();
        ds |= 2;
        bus.set_dispstat(ds);
        let mut dma = std::mem::take(&mut bus.dma);
        dma.on_hblank(bus);
        bus.dma = dma;
        if bus.dispstat() & (1 << 4) != 0 {
            crate::irq::raise(bus, crate::irq::IRQ_HBLANK);
        }
    }

    fn finish_line(&mut self, bus: &mut Bus) {
        let line = self.line;
        let entering_vblank = line == HEIGHT as u16;

        self.line += 1;
        if self.line >= LINES_PER_FRAME as u16 {
            self.line = 0;
            self.frame_ready = true;
            self.latch_affine_from_bus(bus);
        }
        bus.set_vcount(self.line);

        let mut ds = bus.dispstat() & !3; // new line: clear vblank + hblank
        if self.line >= HEIGHT as u16 {
            ds |= 1;
        }
        let lyc = (ds >> 8) & 0xFF;
        if u32::from(self.line) == u32::from(lyc) {
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

        if line < HEIGHT as u16 {
            let mut pb2 = bus.read16(0x0400_0022) as i16 as i32;
            let mut pd2 = bus.read16(0x0400_0026) as i16 as i32;
            let mut pb3 = bus.read16(0x0400_0032) as i16 as i32;
            let mut pd3 = bus.read16(0x0400_0036) as i16 as i32;
            if crate::cpu::affine_compat() {
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
                if pd2.abs() > 0x400 {
                    pd2 = 0x100;
                }
                if pd3.abs() > 0x400 {
                    pd3 = 0x100;
                }
            }
            self.bg2x = self.bg2x.wrapping_add(pb2);
            self.bg2y = self.bg2y.wrapping_add(pd2);
            self.bg3x = self.bg3x.wrapping_add(pb3);
            self.bg3y = self.bg3y.wrapping_add(pd3);
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::Bus;
    use crate::cart::Cart;

    fn empty_bus() -> Bus {
        let cart = Cart {
            data: vec![0u8; 0x200],
            title: "t".into(),
            game_code: "T".into(),
            maker: "00".into(),
            path: "m".into(),
            inner_name: None,
        };
        Bus::new(&cart, None)
    }

    #[test]
    fn fight_savestate_hp_bar_row_is_lit() {
        let rom = std::path::Path::new(
            "/home/evenweaker/.local/share/faeos/fairy-lantern/roms/Pokemon Liquid Crystal (v3.3.00512).gba",
        );
        let st = std::path::Path::new(
            "/home/evenweaker/.local/share/faeos/fairy-lantern/states/Pokemon Liquid Crystal (v3.3.00512).flst",
        );
        if !rom.exists() || !st.exists() {
            return;
        }
        let mut emu = crate::emu::Emu::from_path(rom, None).expect("load LC ROM");
        crate::savestate::load(&mut emu, st).expect("load fight state");
        render::render_scanline(&emu.bus, 89, &mut emu.ppu.frame);
        let px = emu.ppu.frame[89 * WIDTH + 160];
        assert_ne!(
            px & 0x7FFF,
            0,
            "player HP bar row y=89 x=160 must be lit after compositor fix"
        );
    }

    #[test]
    fn hblank_starts_at_1006_not_end_of_line() {
        let mut ppu = Ppu::new();
        let mut bus = empty_bus();
        bus.write16(0x0400_0004, 1 << 4); // HBlank IRQ enable
        let _ = ppu.step(&mut bus, 1005);
        assert_eq!(ppu.line, 0);
        assert_eq!(bus.dispstat() & 2, 0, "still in hdraw");
        let _ = ppu.step(&mut bus, 1);
        assert_eq!(bus.dispstat() & 2, 2, "HBlank flag at cycle 1006");
        assert_eq!(ppu.line, 0, "line must not advance at HBlank start");
        let _ = ppu.step(&mut bus, CYCLES_PER_LINE - HBLANK_CYCLE);
        assert_eq!(ppu.line, 1);
        assert_eq!(bus.dispstat() & 2, 0, "HBlank cleared on the next line");
    }

    fn put_obj_tile_solid(bus: &mut Bus, tile: usize, nibble: u8) {
        let off = 0x10000 + tile * 32;
        let b = nibble | (nibble << 4);
        for i in 0..32 {
            bus.vram[off + i] = b;
        }
    }

    fn oam_sprite(bus: &mut Bus, i: usize, y: u8, attr0_hi: u8, x: u16, attr1_hi: u8, attr2: u16) {
        let o = i * 8;
        let a0 = u16::from(y) | (u16::from(attr0_hi) << 8);
        let a1 = (x & 0x1FF) | (u16::from(attr1_hi) << 8);
        bus.oam[o..o + 2].copy_from_slice(&a0.to_le_bytes());
        bus.oam[o + 2..o + 4].copy_from_slice(&a1.to_le_bytes());
        bus.oam[o + 4..o + 6].copy_from_slice(&attr2.to_le_bytes());
    }

    #[test]
    fn obj_32x8_healthbar_is_visible() {
        // Gen3 HP bar: 32×8, 4bpp, 1D mapping, priority 1.
        let mut bus = empty_bus();
        bus.write16(0x0400_0000, (1 << 12) | (1 << 6)); // OBJ on, 1D
        put_obj_tile_solid(&mut bus, 0, 1);
        put_obj_tile_solid(&mut bus, 1, 1);
        put_obj_tile_solid(&mut bus, 2, 1);
        put_obj_tile_solid(&mut bus, 3, 1);
        // OBJ pal 0, color 1 = red
        bus.pal[0x200 + 2] = 0x1F;
        bus.pal[0x200 + 3] = 0x00;
        // ATTR0: y=20, shape=horizontal; ATTR1: x=10, size=32x8
        oam_sprite(&mut bus, 0, 20, 1 << 6, 10, 1 << 6, 0);
        let mut frame = vec![0u16; WIDTH * HEIGHT];
        render::render_scanline(&bus, 20, &mut frame);
        let px = frame[20 * WIDTH + 12];
        assert_ne!(px & 0x7FFF, 0, "32x8 bar pixel must be lit, got {px:04X}");
        assert_eq!(px & 0x1F, 0x1F, "bar uses OBJ pal color 1 (red)");
    }

    #[test]
    fn obj_same_prio_low_index_covers_box() {
        // Healthbox (OAM 5) and HP bar (OAM 0), both priority 1.
        // Lower OAM index is in front (GBATEK). The bar must win the trough.
        let mut bus = empty_bus();
        bus.write16(0x0400_0000, (1 << 12) | (1 << 6));
        put_obj_tile_solid(&mut bus, 0, 1); // bar: pal idx 1 = red
        put_obj_tile_solid(&mut bus, 8, 2); // box: pal idx 2 = green
        bus.pal[0x200 + 2] = 0x1F;
        bus.pal[0x200 + 3] = 0x00;
        bus.pal[0x200 + 4] = 0xE0;
        bus.pal[0x200 + 5] = 0x03;
        // OAM 5: 64×64 square at (10,20), tile 8
        oam_sprite(&mut bus, 5, 20, 0, 10, 3 << 6, 8);
        // OAM 0: 32×8 bar at (10,20), tile 0
        oam_sprite(&mut bus, 0, 20, 1 << 6, 10, 1 << 6, 0);
        let mut frame = vec![0u16; WIDTH * HEIGHT];
        render::render_scanline(&bus, 20, &mut frame);
        let px = frame[20 * WIDTH + 12];
        assert_eq!(px & 0x1F, 0x1F, "HP bar (low OAM) must cover the box, got {px:04X}");
    }

    #[test]
    fn obj_64x32_healthbox_is_visible() {
        let mut bus = empty_bus();
        bus.write16(0x0400_0000, (1 << 12) | (1 << 6));
        for t in 0..32 {
            put_obj_tile_solid(&mut bus, t, 2);
        }
        bus.pal[0x200 + 4] = 0xE0; // green-ish
        bus.pal[0x200 + 5] = 0x03;
        // y=72, x=126, shape=horizontal size=64x32
        oam_sprite(&mut bus, 0, 72, 1 << 6, 126, 3 << 6, 0);
        let mut frame = vec![0u16; WIDTH * HEIGHT];
        render::render_scanline(&bus, 80, &mut frame);
        let px = frame[80 * WIDTH + 140];
        assert_ne!(px & 0x7FFF, 0, "64x32 box pixel must be lit, got {px:04X}");
    }

    #[test]
    fn frame_still_228_lines() {
        let mut ppu = Ppu::new();
        let mut bus = empty_bus();
        let mut frames = 0u32;
        for _ in 0..LINES_PER_FRAME {
            if ppu.step(&mut bus, CYCLES_PER_LINE) {
                frames += 1;
            }
        }
        assert_eq!(frames, 1);
        assert_eq!(ppu.line, 0);
    }
}
