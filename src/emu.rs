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
                    timers::step(&mut self.timers, &mut self.bus, c);
                    self.bus.timer_reload = self.timers.reload;
                    // Apply any timer-start reloads latched by MMIO writes
                    self.bus.apply_timer_starts(&mut self.timers);
                    if self.ppu.step(&mut self.bus, c) {
                        frame = true;
                        self.frames_since_flush += 1;
                    }
                    irq::check(&mut self.cpu, &mut self.bus);
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
                timers::step(&mut self.timers, &mut self.bus, c);
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
                irq::check(&mut self.cpu, &mut self.bus);
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
                if std::env::var_os("FAIRY_DMA_TRACE").is_some() && (200..=260).contains(&frames) {
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
    if let Some(p) = bios_path {
        return std::fs::read(p).ok();
    }
    if let Ok(p) = std::env::var("FAIRY_LANTERN_BIOS") {
        let p = Path::new(&p);
        if p.is_file() {
            return std::fs::read(p).ok();
        }
    }
    None
}
