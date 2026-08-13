//! Fairy Lantern core — CPU + bus + PPU + timers + IRQ + battery.

use crate::battery;
use crate::bus::Bus;
use crate::cart::Cart;
use crate::cpu::Cpu;
use crate::irq;
use crate::ppu::Ppu;
use crate::timers::{self, Timers};
use anyhow::Result;
use std::path::{Path, PathBuf};

pub struct Emu {
    pub cpu: Cpu,
    pub bus: Bus,
    pub ppu: Ppu,
    pub timers: Timers,
    pub cart_title: String,
    pub rom_path: Option<PathBuf>,
    frames_since_flush: u32,
    dbg_win_prev: Option<Vec<u8>>,
}

impl Emu {
    pub fn new(cart: &Cart, bios: Option<Vec<u8>>) -> Self {
        let mut cpu = Cpu::new();
        let bus = Bus::new(cart, bios);
        // Start in System mode (post-BIOS), with USR/SYS stack
        cpu.cpsr.thumb = false;
        cpu.cpsr.irq_disable = false;
        cpu.cpsr.fiq_disable = false;
        cpu.set_mode(0x1F);
        cpu.r[13] = 0x0300_7F00;
        cpu.r13_usr = 0x0300_7F00;
        cpu.r[14] = 0;
        cpu.set_pc(0x0800_0000);
        Self {
            cpu,
            bus,
            ppu: Ppu::new(),
            timers: Timers::new(),
            cart_title: cart.title.clone(),
            rom_path: None,
            frames_since_flush: 0,
            dbg_win_prev: None,
        }
    }

    pub fn from_path(path: &Path, bios_path: Option<&Path>) -> Result<Self> {
        let cart = Cart::load(path)?;
        let mut emu = Self::new(&cart, load_bios(bios_path));
        emu.attach_rom_path(path);
        Ok(emu)
    }

    pub fn from_cart(cart: Cart, bios_path: Option<&Path>) -> Self {
        Self::new(&cart, load_bios(bios_path))
    }

    /// Wire battery .sav next to the ROM (or under data dir).
    pub fn attach_rom_path(&mut self, path: &Path) {
        self.rom_path = Some(path.to_path_buf());
        let sav = battery::sav_path_for_rom(path);
        self.bus.load_battery(sav);
        eprintln!(
            "  battery: {} → {}",
            self.bus.save_type.label(),
            self.bus
                .save_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(none)".into())
        );
        if self.bus.rtc.present {
            eprintln!("  rtc: SIIRTC/GPIO clock enabled");
        }
    }

    pub fn flush_battery(&mut self) {
        if let Err(e) = self.bus.flush_battery() {
            eprintln!("fairy-lantern: battery save failed: {e:#}");
        }
    }

    pub fn state_path(&self) -> Option<PathBuf> {
        self.rom_path.as_ref().map(|p| battery::state_path_for_rom(p))
    }

    /// Step a few CPU cycles; returns true if a video frame completed.
    pub fn step_cycles(&mut self, min_cycles: u32) -> bool {
        let mut left = min_cycles.max(1);
        let mut frame = false;
        while left > 0 {
            // BIOS Halt / IntrWait: burn cycles on PPU until VBlank/IF match
            // Accumulate a slice of work, then tick sound once (not per-insn from UI).
            let mut slice = 0u32;
            let slice_target = 64u32.min(left);

            while slice < slice_target {
                if self.bus.halt_wait {
                    let c = 64u32;
                    self.timers.reload = self.bus.timer_reload;
                    let ov = timers::step(&mut self.timers, &mut self.bus, c);
                    for i in 0..4 {
                        self.bus.timer_overflows[i] =
                            self.bus.timer_overflows[i].saturating_add(ov[i]);
                    }
                    self.bus.timer_reload = self.timers.reload;
                    // Apply any timer-start reloads latched by MMIO writes
                    self.bus.apply_timer_starts(&mut self.timers);
                    if self.ppu.step(&mut self.bus, c) {
                        frame = true;
                        self.frames_since_flush += 1;
                    }
                    irq::check(&mut self.cpu, &mut self.bus, c);
                    let if_ = self.bus.read16(0x0400_0202);
                    let ie = self.bus.read16(0x0400_0200);
                    let bios_flag = self.bus.read16(0x0300_7FF8);
                    let wake = if self.bus.intr_wait_mask != 0 {
                        // IntrWait: specific IRQ bits (IF or BIOS mirror)
                        let m = self.bus.intr_wait_mask;
                        (if_ & m) != 0 || (bios_flag & m) != 0
                    } else {
                        // Halt: any enabled pending IRQ
                        (if_ & ie) != 0
                    };
                    if wake {
                        self.bus.halt_wait = false;
                        self.bus.intr_wait_mask = 0;
                    }
                    slice += c;
                    left = left.saturating_sub(c);
                    if frame {
                        break;
                    }
                    continue;
                }

                let c = self.cpu.step(&mut self.bus);
                self.timers.reload = self.bus.timer_reload;
                self.bus.apply_timer_starts(&mut self.timers);
                let ov = timers::step(&mut self.timers, &mut self.bus, c);
                for i in 0..4 {
                    self.bus.timer_overflows[i] =
                        self.bus.timer_overflows[i].saturating_add(ov[i]);
                }
                self.bus.timer_reload = self.timers.reload;
                if self.ppu.line == 0 && self.ppu.line_cycles < 32 {
                    self.ppu.latch_affine_from_bus(&self.bus);
                }
                if self.ppu.step(&mut self.bus, c) {
                    frame = true;
                    self.frames_since_flush += 1;
                    if self.frames_since_flush >= 120 && self.bus.save_dirty {
                        self.flush_battery();
                        self.frames_since_flush = 0;
                    }
                }
                irq::check(&mut self.cpu, &mut self.bus, c);
                slice += c;
                left = left.saturating_sub(c);
                if frame {
                    break;
                }
            }
            if slice > 0 {
                self.bus.tick_sound(slice);
            }
        }
        frame
    }

    /// Run until one video frame completes (batched; use this in the play loop).
    pub fn run_one_frame(&mut self) -> bool {
        // ~280896 cycles/frame; step in 256-cycle chunks for low overhead
        let mut guard = 0u32;
        while !self.step_cycles(256) {
            guard += 1;
            if guard > 2_000_000 {
                return false;
            }
        }
        // Ensure last audio samples reach the host ring
        self.bus.tick_sound(0);
        true
    }

    pub fn run_frames(&mut self, n: u32) -> u32 {
        self.run_frames_with_input(n, None)
    }

    /// Run N frames with optional headless key automation.
    pub fn run_frames_with_input(&mut self, n: u32, auto_input: Option<AutoInput>) -> u32 {
        let mut frames = 0u32;
        let mut guard = 0u64;
        let ai = auto_input.unwrap_or(AutoInput::off());
        while frames < n {
            if ai.enabled {
                self.bus.set_keys_pressed(ai.buttons_at(frames));
            }
            if self.step_cycles(64) {
                frames += 1;
                if frames % 25 == 0 && std::env::var_os("FAIRY_MIX_STAT").is_some() {
                    dump_mix_stat(&self.bus, frames);
                }
                if crate::cpu::fairy_trace() && (200..=260).contains(&frames) {
                    let w = &self.bus.iwram[0x62A0..0x68C0];
                    let d = if let Some(prev) = self.dbg_win_prev.as_ref() {
                        w.iter().zip(prev.iter()).filter(|(a, b)| a != b).count()
                    } else {
                        0
                    };
                    self.dbg_win_prev = Some(w.to_vec());
                    let nz = w.iter().filter(|&&b| b != 0).count();
                    eprintln!("WINDIFF f{frames} diff={d} nz={nz}");
                }
            }
            guard += 1;
            // ~280k cycles/frame ÷ 64 ≈ 4400 steps/frame; allow long headless runs
            if guard > 500_000_000 {
                break;
            }
        }
        self.bus.set_keys_pressed(0);
        dump_oam_stat(&self.bus, frames);
        if std::env::var_os("FAIRY_DUMP_IWRAM").is_some() {
            let w = &self.bus.iwram[0x62A0..0x68C0];
            eprint!("MIXWIN:");
            for (i, b) in w.iter().enumerate() {
                if i % 16 == 0 {
                    eprintln!();
                    eprint!("{:04X}:", 0x62A0 + i);
                }
                eprint!("{:02X} ", b);
            }
            eprintln!();
        }
        self.flush_battery();
        frames
    }
}

/// m4a SoundInfo @ 0x03005F50, 12×64-byte SoundChannels @ 0x03005FA0.
/// DMA1 walks pcmBuffer A (0x62A0, 1584 bytes); VSync should rewind SAD
/// every pcmDmaPeriod frames. A stuck START/LOOP voice or a SAD that never
/// rewinds is the "first SFX under the song, then mush" pattern.
fn dump_mix_stat(bus: &Bus, frames: u32) {
    let iw = &bus.iwram;
    let rd8 = |off: usize| iw.get(off).copied().unwrap_or(0);
    let rd32 = |off: usize| u32::from_le_bytes([rd8(off), rd8(off + 1), rd8(off + 2), rd8(off + 3)]);
    let stat = |off: usize, n: usize| {
        let mut peak = 0u32;
        let mut sum = 0u32;
        let mut rail = 0u32;
        for i in 0..n {
            let a = (rd8(off + i) as i8).unsigned_abs() as u32;
            peak = peak.max(a);
            sum += a;
            if a >= 120 {
                rail += 1;
            }
        }
        (peak, sum / n.max(1) as u32, rail)
    };
    let (ap, am, ar) = stat(0x62A0, 1584);
    let (bp, bm, br) = stat(0x68D0, 1584);
    let ident = rd32(0x5F50);
    let dma_ctr = rd8(0x5F54);
    let reverb = rd8(0x5F55);
    let max_ch = rd8(0x5F56);
    let masvol = rd8(0x5F57);
    let period = rd8(0x5F5B);
    let spv = rd32(0x5F60) as i32;
    let divf = rd32(0x5F68);
    eprint!(
        "MIXSTAT f{frames} A peak={ap} mean={am} rail={ar}  B peak={bp} mean={bm} rail={br}  \
         id={ident:08X} ctr={dma_ctr} per={period} rev={reverb} ch={max_ch} vol={masvol} spv={spv} div={divf:08X}  A0="
    );
    for i in 0..12 {
        eprint!("{:02X}", rd8(0x62A0 + i));
    }
    let src1 = bus.dma.ch[1].src;
    let src2 = bus.dma.ch[2].src;
    eprint!("  dma1={src1:08X} dma2={src2:08X}");
    eprintln!();
    eprint!("  chans");
    for i in 0..12 {
        let base = 0x5FA0 + i * 0x40;
        let flags = rd8(base);
        // SOUND_CHANNEL_SF_ON = START|STOP|IEC|ENV = 0xC7
        if flags & 0xC7 == 0 {
            continue;
        }
        let typ = rd8(base + 1);
        let evy = rd8(base + 9);
        let evr = rd8(base + 10);
        let evl = rd8(base + 11);
        let count = rd32(base + 0x18);
        let fw = rd32(base + 0x1C);
        let freq = rd32(base + 0x20);
        let wav = rd32(base + 0x24);
        let ptr = rd32(base + 0x28);
        eprint!(
            " [{i} st={flags:02X} ty={typ:02X} ev={evy:02X}/{evr:02X}/{evl:02X} \
             n={count:X} fw={fw:X} f={freq:X} wav={wav:08X} p={ptr:08X}]"
        );
    }
    eprintln!();
    eprint!("  songs");
    for &sbase in &[
        0x6FB0usize, 0x6F70, 0x73D0, 0x7380, 0x7340, 0x7300,
    ] {
        let status = rd32(sbase + 4);
        let clock = rd32(sbase + 0xC);
        if status != 0 || clock != 0 {
            eprint!(" @{sbase:04X} st={status:08X} clk={clock}");
        }
    }
    eprintln!();
}

/// Live OAM: Y, X, shape/size, affine, mosaic, prio, tile. 32×8 (shape1 size1)
/// is the Gen3 HP bar; 64×32 (shape1 size3) / square 64 is the healthbox.
pub fn dump_oam_stat(bus: &Bus, frames: u32) {
    let dispcnt = bus.dispcnt();
    let winin = bus.read16(0x0400_0048);
    let winout = bus.read16(0x0400_004A);
    let bldcnt = bus.read16(0x0400_0050);
    let mosaic = bus.read16(0x0400_004C);
    let win0h = bus.read16(0x0400_0040);
    let win0v = bus.read16(0x0400_0044);
    let obj_on = dispcnt & (1 << 12) != 0;
    let one_d = dispcnt & (1 << 6) != 0;
    eprint!(
        "OAMSTAT f{frames} dispcnt={dispcnt:04X} mode={} obj={} map={} win0={} win1={} objwin={} \
         winin={winin:04X} winout={winout:04X} win0h={win0h:04X} win0v={win0v:04X} \
         bld={bldcnt:04X} mos={mosaic:04X}",
        dispcnt & 7,
        obj_on as u8,
        if one_d { "1D" } else { "2D" },
        (dispcnt & (1 << 13) != 0) as u8,
        (dispcnt & (1 << 14) != 0) as u8,
        (dispcnt & (1 << 15) != 0) as u8,
    );
    eprintln!();
    let mut live = 0u32;
    for i in 0..128 {
        let o = i * 8;
        let a0 = u16::from_le_bytes([bus.oam[o], bus.oam[o + 1]]);
        let a1 = u16::from_le_bytes([bus.oam[o + 2], bus.oam[o + 3]]);
        let a2 = u16::from_le_bytes([bus.oam[o + 4], bus.oam[o + 5]]);
        let y = a0 & 0xFF;
        let x = a1 & 0x1FF;
        let affine = a0 & (1 << 8) != 0;
        let hide = !affine && a0 & (1 << 9) != 0;
        if hide {
            continue;
        }
        // Dummy off-screen slot (y=160, x=240+) used by unused OAM
        if y == 160 && x >= 240 {
            continue;
        }
        live += 1;
        let shape = (a0 >> 14) & 3;
        let size = (a1 >> 14) & 3;
        let gfx = (a0 >> 10) & 3;
        let mosaic = a0 & (1 << 12) != 0;
        let bpp8 = a0 & (1 << 13) != 0;
        let prio = (a2 >> 10) & 3;
        let tile = a2 & 0x3FF;
        let pal = (a2 >> 12) & 0xF;
        eprintln!(
            "  oam{i:02} y={y:3} x={x:3} sh={shape} sz={size} gfx={gfx} \
             af={} mos={} c{} prio={prio} tile={tile:03X} pal={pal}",
            affine as u8,
            mosaic as u8,
            if bpp8 { 8 } else { 4 },
        );
    }
    eprintln!("  oam_live={live}");
}

/// Headless key automation for advancing title screens / menus / overworld.
#[derive(Clone, Copy, Debug)]
pub struct AutoInput {
    pub enabled: bool,
}

impl AutoInput {
    pub fn off() -> Self {
        Self { enabled: false }
    }

    /// Phased input for commercial Pokémon-class boots:
    /// 1) Start on title  2) A through menus/dialogue  3) light D-pad walk.
    pub fn title_advance() -> Self {
        Self { enabled: true }
    }

    /// KEYINPUT pressed mask for this frame (0 = nothing held).
    pub fn buttons_at(self, frame: u32) -> u16 {
        if !self.enabled {
            return 0;
        }
        const A: u16 = 1 << 0;
        const START: u16 = 1 << 3;
        const RIGHT: u16 = 1 << 4;
        const DOWN: u16 = 1 << 7;

        // Before title is ready
        if frame < 2500 {
            return 0;
        }
        // Title: a few Start presses
        if frame < 2800 {
            let t = frame - 2500;
            return if t % 40 < 5 { START } else { 0 };
        }
        // Main menu / intro / dialogue: A pulses (not Start — Start opens pause)
        if frame < 9000 {
            let t = frame - 2800;
            return if t % 50 < 4 { A } else { 0 };
        }
        // Overworld: walk in a small pattern + occasional A (interact)
        let t = frame - 9000;
        let phase = t % 240;
        if phase < 4 {
            A
        } else if phase < 60 {
            RIGHT
        } else if phase < 64 {
            0
        } else if phase < 120 {
            DOWN
        } else {
            0
        }
    }
}

fn load_bios(bios_path: Option<&Path>) -> Option<Vec<u8>> {
    let candidates: Vec<PathBuf> = {
        let mut v = Vec::new();
        if let Some(p) = bios_path {
            v.push(p.into());
        }
        if let Ok(s) = std::env::var("FAIRY_LANTERN_BIOS") {
            v.push(PathBuf::from(s));
        }
        // Default paths (user-supplied only — no GBA BIOS ships with the emulator).
        if let Ok(home) = std::env::var("HOME") {
            v.push(PathBuf::from(format!(
                "{home}/.local/share/faeos/fairy-lantern/bios.bin"
            )));
        }
        v.push(PathBuf::from("./bios.bin"));
        v
    };
    for p in &candidates {
        if !p.is_file() {
            continue;
        }
        match std::fs::read(p) {
            Ok(data) => {
                if data.len() != 0x4000 {
                    eprintln!(
                        "fairy: ignoring bios {} (size={} bytes, expected 16384)",
                        p.display(),
                        data.len()
                    );
                    continue;
                }
                let sum = data.iter().fold(0u8, |a, &b| a.wrapping_add(b));
                // Known GBA BIOS checksum is 0xBAAE1880 — warn on mismatch.
                if sum != 0x80 {
                    eprintln!(
                        "fairy: bios checksum {:02X} (expected 80) — file may be corrupted",
                        sum
                    );
                } else {
                    eprintln!("fairy: using bios {}", p.display());
                }
                return Some(data);
            }
            Err(e) => {
                eprintln!("fairy: cannot read bios {}: {e}", p.display());
            }
        }
    }
    None
}
