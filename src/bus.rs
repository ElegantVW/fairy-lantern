//! GBA memory map (simplified waitstates) + battery-backed cart save.

use crate::battery::{self, EepromChip, FlashChip, SaveType};
use crate::cart::Cart;
use crate::dma::DmaController;
use crate::rtc::Rtc;
use crate::sound::{bios::SoundDriver, Sound};
use crate::timers::Timers;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;

pub const EWRAM_SIZE: usize = 256 * 1024;
pub const IWRAM_SIZE: usize = 32 * 1024;
pub const VRAM_SIZE: usize = 96 * 1024;
pub const PAL_SIZE: usize = 1024;
pub const OAM_SIZE: usize = 1024;
pub const BIOS_SIZE: usize = 16 * 1024;
pub const IO_SIZE: usize = 0x400;

pub struct Bus {
    pub bios: Vec<u8>,
    pub ewram: Vec<u8>,
    pub iwram: Vec<u8>,
    pub io: Vec<u8>,
    pub pal: Vec<u8>,
    pub vram: Vec<u8>,
    pub oam: Vec<u8>,
    pub rom: Vec<u8>,
    /// SRAM mirror (also used when SaveType::Sram)
    pub sram: Vec<u8>,
    pub flash: Option<FlashChip>,
    /// RefCell: serial reads advance state from `&self` read16 paths (DMA).
    pub eeprom: RefCell<Option<EepromChip>>,
    pub save_type: SaveType,
    pub save_path: Option<PathBuf>,
    pub save_dirty: bool,
    /// KEYINPUT active-low bits (0 = pressed)
    pub keyinput: u16,
    /// Timer reload shadow (synced from Emu.timers on write)
    pub timer_reload: [u16; 4],
    /// Previous TMxCNT_H enable bits (for 0→1 start detection).
    pub(crate) timer_ctrl_prev: [u16; 4],
    /// Channels that need counter←reload on next timer step (bit0=TM0…).
    pub timer_start_mask: u8,
    /// Overflows since last `tick_sound` (DirectSound sample clock).
    pub timer_overflows: [u32; 4],
    /// Last value driven on the data bus (open-bus for unmapped reads).
    last_bus: Cell<u32>,
    /// Last BIOS prefetch. ROM code reading BIOS sees this, not `last_bus`.
    /// GBATEK: 00 after reset, before any BIOS fetch (HLE never fetches).
    last_bios: Cell<u32>,
    /// Cycles the IRQ line has been visibly pending (IME && IE && IF && !I).
    /// Hardware takes the exception ~2 clocks after it becomes true.
    pub irq_countdown: u8,
    /// Last instruction-fetch address (for N vs S waitstates).
    last_fetch: Cell<u32>,
    last_fetch_ok: Cell<bool>,
    /// Game Pak prefetch halfwords waiting (0..=8).
    prefetch_halfs: Cell<u8>,
    /// BIOS IntrWait / Halt: run until these IF bits appear (or VBlank)
    pub halt_wait: bool,
    pub intr_wait_mask: u16,
    /// IME to restore when IntrWait wakes (BIOS saves/restores it).
    pub intr_wait_ime: u16,
    /// True when using HLE BIOS (no real GBA BIOS binary loaded).
    pub hle_bios: bool,
    /// Debug: how many times the CPU entered IRQ.
    pub irq_count: u64,
    pub dma: DmaController,
    pub sound: Sound,
    pub sound_driver: SoundDriver,
    /// Temporary DMA trace event counter (FAIRY_DMA_TRACE=1).
    pub dbg_evt: u64,
    /// Temporary: last CPU PC (set each step; printed in DMA trace).
    pub dbg_pc: u32,
    /// PC of the instruction currently executing (BIOS open-bus gate).
    pub exec_pc: u32,
    pub rtc: Rtc,
    pub swi_counts: [u32; 256],
    pub swi_unknown: u32,
    pub last_swi_unknown: u8,
    /// Last LZ77UnCompVram dest + size (debug)
    pub last_lz77v_dst: u32,
    pub last_lz77v_size: u32,
    pub last_lz77v_to_0600: u32,
}


impl Bus {
    pub fn new(cart: &Cart, bios: Option<Vec<u8>>) -> Self {
        let save_type = battery::detect(&cart.data);
        let size = save_type.size().max(64 * 1024);
        let hle_bios = bios.is_none();
        let mut b = Self {
            bios: bios.unwrap_or_else(|| vec![0; BIOS_SIZE]),
            ewram: vec![0; EWRAM_SIZE],
            iwram: vec![0; IWRAM_SIZE],
            io: vec![0; IO_SIZE],
            pal: vec![0; PAL_SIZE],
            vram: vec![0; VRAM_SIZE],
            oam: vec![0; OAM_SIZE],
            rom: cart.data.clone(),
            sram: vec![0xFF; size],
            flash: match save_type {
                SaveType::Flash64 => Some(FlashChip::new_for(64 * 1024, &cart.data)),
                SaveType::Flash128 => Some(FlashChip::new_for(128 * 1024, &cart.data)),
                _ => None,
            },
            eeprom: RefCell::new(EepromChip::from_save_type(save_type)),
            save_type,
            save_path: None,
            save_dirty: false,
            keyinput: 0x03FF,
            timer_reload: [0; 4],
            timer_ctrl_prev: [0; 4],
            timer_start_mask: 0,
            timer_overflows: [0; 4],
            last_bus: Cell::new(0),
            last_bios: Cell::new(0),
            irq_countdown: 0,
            last_fetch: Cell::new(0),
            last_fetch_ok: Cell::new(false),
            prefetch_halfs: Cell::new(0),
            halt_wait: false,
            intr_wait_mask: 0,
            intr_wait_ime: 1,
            hle_bios,
            irq_count: 0,
            dma: DmaController::new(),
            sound: Sound::new(),
            sound_driver: SoundDriver::new(),
            dbg_evt: 0,
            dbg_pc: 0,
            exec_pc: 0x0800_0000,
            rtc: Rtc::new(Rtc::detect(&cart.data)),
            swi_counts: [0; 256],
            swi_unknown: 0,
            last_swi_unknown: 0,
            last_lz77v_dst: 0,
            last_lz77v_size: 0,
            last_lz77v_to_0600: 0,
        };
        b.write16_raw(0x0400_0130, 0x03FF);
        b.write16_raw(0x0400_0000, 0x0080);
        // Real BIOS leaves POSTFLG=1 after the boot sequence.
        b.io[0x300] = 1;
        // SoundBias default (BIOS-like) — mid-level DC bias
        b.write16_raw(0x0400_0088, 0x0200);
        // SOUNDCNT_X — PSG/FIFO master enable defaults ON (real hardware)
        b.write16_raw(0x0400_0084, 0x0080);
        // Affine BG identity scale (games often assume PA/PD = 0x100 without rewriting)
        b.write16_raw(0x0400_0020, 0x0100); // BG2PA
        b.write16_raw(0x0400_0026, 0x0100); // BG2PD
        b.write16_raw(0x0400_0030, 0x0100); // BG3PA
        b.write16_raw(0x0400_0036, 0x0100); // BG3PD
        b
    }

    /// Attach a .sav path and load battery contents.
    pub fn load_battery(&mut self, sav: PathBuf) {
        if self.save_type == SaveType::None {
            // A leftover .sav from a previous promote (untagged homebrew).
            if sav.exists() {
                if let Ok(data) = std::fs::read(&sav) {
                    if !data.is_empty() {
                        self.promote_sram_if_none();
                        let n = data.len().min(self.sram.len());
                        self.sram[..n].copy_from_slice(&data[..n]);
                        self.save_path = Some(sav);
                        self.save_dirty = false;
                        return;
                    }
                }
            }
            self.save_path = Some(sav);
            return;
        }
        let size = self.save_type.size().max(1);
        let data = battery::load_sav(&sav, size);
        match self.save_type {
            SaveType::Flash64 | SaveType::Flash128 => {
                if let Some(ref mut f) = self.flash {
                    let n = data.len().min(f.data.len());
                    f.data[..n].copy_from_slice(&data[..n]);
                }
            }
            SaveType::Eeprom512 | SaveType::Eeprom8K => {
                if let Some(ref mut e) = self.eeprom.borrow_mut().as_mut() {
                    let n = data.len().min(e.data.len());
                    e.data[..n].copy_from_slice(&data[..n]);
                }
            }
            SaveType::Sram(_) => {
                let n = data.len().min(self.sram.len());
                self.sram[..n].copy_from_slice(&data[..n]);
            }
            SaveType::None => {}
        }
        self.save_path = Some(sav);
        self.save_dirty = false;
    }

    /// Flush dirty battery to disk.
    pub fn flush_battery(&mut self) -> anyhow::Result<()> {
        if !self.save_dirty {
            return Ok(());
        }
        let Some(ref path) = self.save_path else {
            return Ok(());
        };
        // Pull eeprom dirty flag into save_dirty before packing.
        if let Some(ref mut e) = self.eeprom.borrow_mut().as_mut() {
            if e.dirty {
                self.save_dirty = true;
                e.dirty = false;
            }
        }
        if !self.save_dirty {
            return Ok(());
        }
        let owned: Vec<u8> = match self.save_type {
            SaveType::Flash64 | SaveType::Flash128 => {
                if let Some(ref f) = self.flash {
                    f.data.clone()
                } else {
                    self.sram.clone()
                }
            }
            SaveType::Eeprom512 | SaveType::Eeprom8K => {
                if let Some(ref e) = *self.eeprom.borrow() {
                    e.data.clone()
                } else {
                    return Ok(());
                }
            }
            SaveType::Sram(n) => self.sram[..n.min(self.sram.len())].to_vec(),
            SaveType::None => return Ok(()),
        };
        battery::save_sav(path, &owned)?;
        self.save_dirty = false;
        Ok(())
    }

    pub fn read8(&self, addr: u32) -> u8 {
        let a = addr;
        let v = match a >> 24 {
            0x00 => {
                // 16K BIOS only. 00004000–01FFFFFF is unused (open bus),
                // not a mirror — even when exec_pc is in BIOS.
                if a >= 0x4000 {
                    return self.open_bus8(a);
                }
                // Protected: only BIOS-resident PC may read the image.
                // Otherwise the last *BIOS* prefetch (0 after reset / HLE).
                // Using last ROM prefetch here made Anguna's stop-music
                // (`LDR [NULL, #0x14]`) see 0x69596959 and spin forever.
                if self.exec_pc < 0x4000 {
                    let v = self.bios.get(a as usize).copied().unwrap_or(0);
                    self.note_bios8(a, v);
                    v
                } else {
                    return self.bios_open_bus8(a);
                }
            }
            0x02 => self.ewram[(a as usize) & (EWRAM_SIZE - 1)],
            0x03 => self.iwram[(a as usize) & (IWRAM_SIZE - 1)],
            0x04 => {
                let off = (a as u16) & 0x3FF;
                if a >= 0x0400_0400 || !io_readable(off) {
                    // Unused / write-only IO → open bus (not zeros in io[]).
                    return self.open_bus8(a);
                }
                if a == 0x0400_0130 || a == 0x0400_0131 {
                    let v = self.keyinput;
                    if a & 1 == 0 {
                        (v & 0xFF) as u8
                    } else {
                        (v >> 8) as u8
                    }
                } else if a == 0x0400_0084 {
                    // Bits 0–3 are live PSG-on flags (read-only). Bit 7 is master.
                    (self.io.get(0x84).copied().unwrap_or(0) & 0x80)
                        | self.sound.psg_channel_status()
                } else if (0x0400_0090..=0x0400_009F).contains(&a) {
                    self.sound
                        .psg_get_wave_byte((a - 0x0400_0090) as u8)
                } else {
                    self.io
                        .get((a as usize) & (IO_SIZE - 1))
                        .copied()
                        .unwrap_or(0)
                }
            }
            0x05 => self.pal[(a as usize) & (PAL_SIZE - 1)],
            0x06 => self.vram[vram_index(a)],
            0x07 => self.oam[(a as usize) & (OAM_SIZE - 1)],
            0x08 | 0x09 | 0x0A | 0x0B | 0x0C | 0x0D => {
                // EEPROM on bus D — prefer read16 for proper bit streaming.
                if (a >> 24) == 0x0D {
                    if let Ok(guard) = self.eeprom.try_borrow() {
                        if guard.is_some() {
                            return 1;
                        }
                    }
                }
                // GPIO RTC window (Pokemon SIIRTC)
                if self.rtc.present {
                    if let Some(v) = self.rtc.read16(a & !1) {
                        return if a & 1 == 0 {
                            (v & 0xFF) as u8
                        } else {
                            (v >> 8) as u8
                        };
                    }
                }
                let off = (a as usize) & 0x01FF_FFFF;
                match self.rom.get(off).copied() {
                    Some(b) => b,
                    None => return self.open_bus8(a),
                }
            }
            0x0E | 0x0F => self.read_save(a),
            // Unused memory: open bus
            _ => return self.open_bus8(a),
        };
        self.note_bus8(a, v);
        v
    }

    #[inline]
    fn note_bus8(&self, addr: u32, v: u8) {
        let shift = (addr & 3) * 8;
        let cur = self.last_bus.get();
        let mask = !(0xFFu32 << shift);
        self.last_bus.set((cur & mask) | ((v as u32) << shift));
    }

    #[inline]
    fn open_bus8(&self, addr: u32) -> u8 {
        let shift = (addr & 3) * 8;
        ((self.last_bus.get() >> shift) & 0xFF) as u8
    }

    #[inline]
    fn note_bios8(&self, addr: u32, v: u8) {
        let shift = (addr & 3) * 8;
        let cur = self.last_bios.get();
        let mask = !(0xFFu32 << shift);
        self.last_bios.set((cur & mask) | ((v as u32) << shift));
    }

    #[inline]
    fn bios_open_bus8(&self, addr: u32) -> u8 {
        let shift = (addr & 3) * 8;
        ((self.last_bios.get() >> shift) & 0xFF) as u8
    }

    /// SoftReset: HLE never fetched BIOS, so open-bus is 0 again.
    pub fn clear_last_bios(&self) {
        self.last_bios.set(0);
    }

    /// RegisterRamReset bit7 zeros DMA/timer IO via `zero_io`, which does not
    /// go through MMIO handlers. Stop the live units or FIFO DMA keeps running
    /// after a title-screen SoftReset.
    pub fn stop_dma_and_timers(&mut self) {
        for ch in &mut self.dma.ch {
            ch.active = false;
            ch.ctrl = 0;
            ch.count = 0;
        }
        self.timer_ctrl_prev = [0; 4];
        self.timer_reload = [0; 4];
        self.timer_start_mask = 0;
        self.timer_overflows = [0; 4];
        self.irq_countdown = 0;
    }

    /// BIOS IntrWait latch at `03007FF8h` (also visible via the IWRAM mirror).
    pub fn irq_check_flags(&self) -> u16 {
        let i = 0x7FF8usize;
        if i + 1 >= self.iwram.len() {
            return 0;
        }
        u16::from_le_bytes([self.iwram[i], self.iwram[i + 1]])
    }

    pub fn set_irq_check_flags(&mut self, v: u16) {
        let i = 0x7FF8usize;
        if i + 1 >= self.iwram.len() {
            return;
        }
        let b = v.to_le_bytes();
        self.iwram[i] = b[0];
        self.iwram[i + 1] = b[1];
    }

    pub fn or_irq_check_flags(&mut self, bits: u16) {
        let cur = self.irq_check_flags();
        self.set_irq_check_flags(cur | bits);
    }

    /// First SRAM-window write on an untagged cart → 64K SRAM.
    /// EEPROM/Flash carts carry an SDK string so they never sit on `None`.
    fn promote_sram_if_none(&mut self) {
        if self.save_type != SaveType::None {
            return;
        }
        self.save_type = SaveType::Sram(64 * 1024);
        if self.sram.len() < 64 * 1024 {
            self.sram.resize(64 * 1024, 0xFF);
        }
    }

    /// Apply timer enable 0→1 reloads latched during MMIO writes.
    pub fn apply_timer_starts(&mut self, timers: &mut Timers) {
        let m = self.timer_start_mask;
        if m == 0 {
            return;
        }
        for i in 0..4 {
            if m & (1 << i) != 0 {
                let r = self.timer_reload[i];
                timers.reload[i] = r;
                timers.counter[i] = r as u32;
                timers.frac[i] = 0;
                self.write16_raw(0x0400_0100 + i as u32 * 4, r);
            }
        }
        self.timer_start_mask = 0;
    }

    fn read_save(&self, addr: u32) -> u8 {
        if self.save_type == SaveType::None {
            return 0xFF;
        }
        if let Some(ref flash) = self.flash {
            return flash.read(addr);
        }
        let idx = if self.sram.len().is_power_of_two() {
            (addr as usize) & (self.sram.len() - 1)
        } else {
            (addr as usize) % self.sram.len().max(1)
        };
        self.sram.get(idx).copied().unwrap_or(0xFF)
    }

    pub fn write8(&mut self, addr: u32, val: u8) {
        let a = addr;
        match a >> 24 {
            0x02 => self.ewram[(a as usize) & (EWRAM_SIZE - 1)] = val,
            0x03 => {
                use std::sync::atomic::{AtomicU64, Ordering};
                static BWR: AtomicU64 = AtomicU64::new(0);
                if crate::cpu::fairy_trace()
                    && (0x0300_28E0..0x0300_28EC).contains(&a)
                {
                    eprintln!(
                        "SLOTWR {:08X}={:02X} evt{} pc={:08X}",
                        a, val, self.dbg_evt, self.dbg_pc
                    );
                }
                if crate::cpu::fairy_trace()
                    && (0x0300_5FA0..0x0300_6228).contains(&a)
                {
                    use std::sync::atomic::{AtomicU64, Ordering};
                    static MTW: AtomicU64 = AtomicU64::new(0);
                    let c = MTW.fetch_add(1, Ordering::Relaxed);
                    if c < 40 || c & 0x3FF == 0 {
                        eprintln!(
                            "MIXTRKW {:08X}={:02X} evt{} pc={:08X} cum{}",
                            a, val, self.dbg_evt, self.dbg_pc, c
                        );
                    }
                }
                if crate::cpu::fairy_trace()
                    && (0x0300_62A0..0x0300_6F00).contains(&a)
                {
                    let c = BWR.fetch_add(1, Ordering::Relaxed);
                    if c < 16 {
                        eprintln!(
                            "BUFWR {:08X}={:02X} evt{} pc={:08X}",
                            a, val, self.dbg_evt, self.dbg_pc
                        );
                    } else if c & 0x1FF == 0 {
                        eprintln!("BUFWR {:08X}={:02X} evt{} pc={:08X} cum{}", a, val, self.dbg_evt, self.dbg_pc, c);
                    }
                }
                self.iwram[(a as usize) & (IWRAM_SIZE - 1)] = val;
            }
            0x04 => {
                if a >= 0x0400_0400 {
                    return;
                }
                let off = (a as u16) & 0x3FF;
                if off == 0x301 {
                    // HALTCNT (write-only): bit7 0=Halt, 1=Stop. Both wait for IRQ.
                    self.halt_wait = true;
                    return;
                }
                if off == 0x300 {
                    if let Some(slot) = self.io.get_mut(0x300) {
                        *slot = val & 1;
                    }
                    return;
                }
                if !io_writable(off) {
                    return;
                }
                let stored = match off {
                    0x201 => val & 0x3F, // IE bits 14–15 unused
                    0x202 | 0x203 => {
                        // IF is write-1-to-clear even on a byte poke.
                        let cur = self.io.get(off as usize).copied().unwrap_or(0);
                        if let Some(slot) = self.io.get_mut(off as usize) {
                            *slot = cur & !val;
                        }
                        return;
                    }
                    0x004 => {
                        // DISPSTAT bits 0–2 are live flags (read-only).
                        let cur = self.io.get(0x004).copied().unwrap_or(0);
                        (val & !7) | (cur & 7)
                    }
                    0x205 => val & 0x7F, // WAITCNT bit15 always 0 on GBA
                    0x208 => val & 1,    // IME bit 0 only
                    _ => val,
                };
                if let Some(slot) = self.io.get_mut(off as usize) {
                    *slot = stored;
                }
            }
            0x05 | 0x07 => {
                // Palette and OAM ignore 8-bit writes (GBATEK).
            }
            0x06 => self.write8_vram(a, val),
            0x08 | 0x09 | 0x0A | 0x0B | 0x0C | 0x0D => {
                if (a >> 24) == 0x0D {
                    if let Ok(mut g) = self.eeprom.try_borrow_mut() {
                        if let Some(ref mut e) = *g {
                            e.write_bit(val as u16);
                            if e.dirty {
                                self.save_dirty = true;
                            }
                            return;
                        }
                    }
                }
                if self.rtc.present {
                    let a16 = a & !1;
                    if matches!(
                        a16,
                        crate::rtc::GPIO_DATA | crate::rtc::GPIO_DIR | crate::rtc::GPIO_CTRL
                    ) {
                        // merge into 16-bit GPIO write
                        let cur = self.rtc.read16(a16).unwrap_or(0);
                        let v = if a & 1 == 0 {
                            (cur & 0xFF00) | val as u16
                        } else {
                            (cur & 0x00FF) | ((val as u16) << 8)
                        };
                        self.rtc.write16(a16, v);
                    }
                }
            }
            0x0E | 0x0F => self.write_save(a, val),
            _ => {}
        }
    }

    fn write_save(&mut self, addr: u32, val: u8) {
        if self.save_type == SaveType::None {
            self.promote_sram_if_none();
        }
        if let Some(ref mut flash) = self.flash {
            if flash.write(addr, val) {
                self.save_dirty = true;
            }
            return;
        }
        let idx = if self.sram.len().is_power_of_two() {
            (addr as usize) & (self.sram.len() - 1)
        } else {
            (addr as usize) % self.sram.len().max(1)
        };
        if let Some(slot) = self.sram.get_mut(idx) {
            if *slot != val {
                *slot = val;
                self.save_dirty = true;
            }
        }
    }

    pub fn read16(&self, addr: u32) -> u16 {
        let a = addr & !1;
        // EEPROM serial out (DMA halfword reads from 0x0Dxxxxxx)
        if (a >> 24) == 0x0D {
            if let Ok(mut g) = self.eeprom.try_borrow_mut() {
                if let Some(ref mut e) = *g {
                    return e.read_serial();
                }
            }
        }
        if self.rtc.present {
            if let Some(v) = self.rtc.read16(a) {
                return v;
            }
        }
        let v = u16::from_le_bytes([self.read8(a), self.read8(a.wrapping_add(1))]);
        self.latch_open_bus16(a, v);
        v
    }

    /// After a 16-bit read: latch the value the next unused-region read will see.
    /// 16-bit buses (ROM / pal / VRAM / OAM) duplicate the halfword. 32-bit
    /// buses (EWRAM / IWRAM) keep the aligned word.
    fn latch_open_bus16(&self, addr: u32, v: u16) {
        match addr >> 24 {
            0x05 | 0x06 | 0x07 | 0x08 | 0x09 | 0x0A | 0x0B | 0x0C => {
                let d = v as u32;
                self.last_bus.set(d | (d << 16));
            }
            0x02 | 0x03 => {
                let a = addr & !3;
                let w = u32::from_le_bytes([
                    self.read8(a),
                    self.read8(a.wrapping_add(1)),
                    self.read8(a.wrapping_add(2)),
                    self.read8(a.wrapping_add(3)),
                ]);
                self.last_bus.set(w);
            }
            _ => {}
        }
    }

    pub fn write32_raw(&mut self, addr: u32, val: u32) {
        let b = val.to_le_bytes();
        self.write16_raw(addr, u16::from_le_bytes([b[0], b[1]]));
        self.write16_raw(addr.wrapping_add(2), u16::from_le_bytes([b[2], b[3]]));
    }

    pub fn write16_raw(&mut self, addr: u32, val: u16) {
        let a = addr & !1;
        let b = val.to_le_bytes();
        match a >> 24 {
            0x02 => {
                let i = (a as usize) & (EWRAM_SIZE - 1);
                if i + 1 < self.ewram.len() {
                    self.ewram[i] = b[0];
                    self.ewram[i + 1] = b[1];
                }
            }
            0x03 => {
                let i = (a as usize) & (IWRAM_SIZE - 1);
                if i + 1 < self.iwram.len() {
                    self.iwram[i] = b[0];
                    self.iwram[i + 1] = b[1];
                }
            }
            0x04 => {
                let i0 = (a as usize) & (IO_SIZE - 1);
                if i0 + 1 < self.io.len() {
                    self.io[i0] = b[0];
                    self.io[i0 + 1] = b[1];
                }
            }
            0x05 => {
                let i = (a as usize) & (PAL_SIZE - 1);
                if i + 1 < self.pal.len() {
                    self.pal[i] = b[0];
                    self.pal[i + 1] = b[1];
                }
            }
            0x06 => {
                let i = vram_index(a) & !1;
                if i + 1 < self.vram.len() {
                    self.vram[i] = b[0];
                    self.vram[i + 1] = b[1];
                }
            }
            0x07 => {
                let i = (a as usize) & (OAM_SIZE - 1);
                if i + 1 < self.oam.len() {
                    self.oam[i] = b[0];
                    self.oam[i + 1] = b[1];
                }
            }
            _ => {
                // SRAM / flash / cart: byte protocol (two serial or two SRAM pokes).
                self.write8(a, b[0]);
                self.write8(a.wrapping_add(1), b[1]);
            }
        }
    }

    pub fn write16(&mut self, addr: u32, val: u16) {
        let a = addr & !1;
        // EEPROM serial in (DMA halfword writes to 0x0Dxxxxxx)
        if (a >> 24) == 0x0D {
            if let Ok(mut g) = self.eeprom.try_borrow_mut() {
                if let Some(ref mut e) = *g {
                    e.write_bit(val);
                    if e.dirty {
                        self.save_dirty = true;
                    }
                    return;
                }
            }
        }
        // GPIO RTC (cart space)
        if self.rtc.present
            && matches!(
                a,
                crate::rtc::GPIO_DATA | crate::rtc::GPIO_DIR | crate::rtc::GPIO_CTRL
            )
        {
            self.rtc.write16(a, val);
            return;
        }
        if (a >> 24) == 0x04 {
            if a >= 0x0400_0400 {
                return;
            }
            let off = (a & 0x3FF) as u16;
            if a == 0x0400_0300 {
                // POSTFLG (bit0) + HALTCNT (high byte, write-only).
                self.write16_raw(a, val & 1);
                self.halt_wait = true;
                return;
            }
            if !io_writable(off) && !io_writable(off.wrapping_add(1)) {
                return;
            }
            match a {
                0x0400_0208 => {
                    self.write16_raw(a, val & 1);
                    return;
                }
                0x0400_0004 => {
                    let cur = self.read16(0x0400_0004);
                    self.write16_raw(a, (val & !7) | (cur & 7));
                    return;
                }
                0x0400_0048 | 0x0400_004A => {
                    // WININ / WINOUT: bits 6–7 / 14–15 unused
                    self.write16_raw(a, val & 0x3F3F);
                    return;
                }
                0x0400_0050 => {
                    self.write16_raw(a, val & 0x3FFF);
                    return;
                }
                0x0400_0200 => {
                    self.write16_raw(a, val & 0x3FFF);
                    return;
                }
                0x0400_0204 => {
                    self.write16_raw(a, val & 0x7FFF);
                    return;
                }
                0x0400_0208 => {
                    self.write16_raw(a, val & 1);
                    return;
                }
                0x0400_0202 => {
                    let cur = self.read16(0x0400_0202);
                    self.write16_raw(a, (cur & !val) & 0x3FFF);
                    return;
                }
                // TMxCNT_L — write sets reload latch only (counter unchanged)
                0x0400_0100 | 0x0400_0104 | 0x0400_0108 | 0x0400_010C => {
                    let idx = ((a - 0x0400_0100) / 4) as usize;
                    if idx < 4 {
                        self.timer_reload[idx] = val;
                    }
                    if crate::cpu::fairy_trace() {
                        eprintln!("ioW {:08X}={:04X} evt{}", a, val, self.dbg_evt);
                    }
                    return;
                }
                // TMxCNT_H — enable 0→1 reloads counter from latch
                0x0400_0102 | 0x0400_0106 | 0x0400_010A | 0x0400_010E => {
                    let idx = ((a - 0x0400_0102) / 4) as usize;
                    let prev = self.timer_ctrl_prev.get(idx).copied().unwrap_or(0);
                    let val = val & 0xC7; // presc, cascade, IRQ, enable
                    self.write16_raw(a, val);
                    if crate::cpu::fairy_trace() {
                        eprintln!("ioW {:08X}={:04X} evt{}", a, val, self.dbg_evt);
                    }
                    if idx < 4 {
                        self.timer_ctrl_prev[idx] = val;
                        if (val & 0x80) != 0 && (prev & 0x80) == 0 {
                            self.timer_start_mask |= 1 << idx;
                        }
                    }
                    return;
                }
                 0x0400_00BC | 0x0400_00BE | 0x0400_00C0 | 0x0400_00C2 | 0x0400_00C4
                | 0x0400_00C8 | 0x0400_00CA | 0x0400_00CC | 0x0400_00CE | 0x0400_00D0 => {
                    if crate::cpu::fairy_trace() {
                        eprintln!(
                            "dmaW {:08X}={:04X} evt{} pc={:08X}",
                            a, val, self.dbg_evt, self.dbg_pc
                        );
                    }
                    self.write16_raw(a, val);
                    return;
                }
                 0x0400_00B0 | 0x0400_00B2 | 0x0400_00B4 | 0x0400_00B6 | 0x0400_00B8 => {
                    if crate::cpu::fairy_trace() {
                        eprintln!(
                            "dmaW {:08X}={:04X} evt{} pc={:08X}",
                            a, val, self.dbg_evt, self.dbg_pc
                        );
                    }
                    self.write16_raw(a, val);
                    return;
                }
                 0x0400_00C6 | 0x0400_00D2 => {
                    self.write16_raw(a, val);
                    if crate::cpu::fairy_trace() {
                        eprintln!("dmaW {:08X}={:04X} CNT_H evt{} pc={:08X}", a, val, self.dbg_evt, self.dbg_pc);
                    }
                    let ch = if a == 0x0400_00C6 { 1 } else { 2 };
                    let mut dma = std::mem::take(&mut self.dma);
                    dma.on_cnt_h_write(self, ch);
                    self.dma = dma;
                    return;
                }
                // SOUNDCNT_H — FIFO reset bits 11/15 clear A/B (write-1-to-reset)
                0x0400_0082 => {
                    if crate::cpu::fairy_trace() {
                        eprintln!("ioW {:08X}={:04X} evt{}", a, val, self.dbg_evt);
                    }
                    if val & (1 << 11) != 0 {
                        self.sound.reset_fifo_a();
                    }
                    if val & (1 << 15) != 0 {
                        self.sound.reset_fifo_b();
                    }
                    // Don't store sticky reset bits
                    self.write16_raw(a, val & !((1 << 11) | (1 << 15)));
                    return;
                }
                // SOUNDCNT_X — only bit 7 is writable; 0–3 are live PSG status.
                0x0400_0084 => {
                    if crate::cpu::fairy_trace() {
                        eprintln!("ioW {:08X}={:04X} evt{}", a, val, self.dbg_evt);
                    }
                    self.write16_raw(a, val & 0x80);
                    if val & 0x80 == 0 {
                        self.sound.psg_all_off();
                    }
                    return;
                }
                // SOUND3CNT_L — wave dimension / bank / DAC
                0x0400_0070 => {
                    self.write16_raw(a, val);
                    self.sound.psg_sync_wave(val);
                    return;
                }
                0x0400_00BA | 0x0400_00DE => {
                    self.write16_raw(a, val);
                    let ch = if a == 0x0400_00BA { 0 } else { 3 };
                    let mut dma = std::mem::take(&mut self.dma);
                    dma.on_cnt_h_write(self, ch);
                    self.dma = dma;
                    return;
                }
                // DISPCNT — entering mode 1/2: ensure BG2/3 have identity scale if still zero
                0x0400_0000 => {
                    self.write16_raw(a, val);
                    let mode = val & 7;
                    if crate::cpu::affine_compat() && (mode == 1 || mode == 2) {
                        // Opt-in LC / battle-HUD patch. Default is hardware
                        // (zero PA stays zero). FAIRY_AFFINE_COMPAT=1.
                        Self::ensure_affine_identity(self, 2);
                        if mode == 2 {
                            Self::ensure_affine_identity(self, 3);
                        }
                    }
                    return;
                }
                // PSG channel frequency/control — bit15 = init/retrigger (write-only)
                0x0400_0064 | 0x0400_006C | 0x0400_0074 | 0x0400_007C => {
                    self.write16_raw(a, val & !0x8000);
                    if val & 0x8000 != 0 {
                        let ch = match a {
                            0x0400_0064 => 1,
                            0x0400_006C => 2,
                            0x0400_0074 => 3,
                            _ => 4,
                        };
                        let regs = self.psg_regs();
                        self.sound.psg_trigger(ch, &regs);
                    }
                    return;
                }
                // Wave RAM (two bytes per halfword)
                0x0400_0090
                | 0x0400_0092
                | 0x0400_0094
                | 0x0400_0096
                | 0x0400_0098
                | 0x0400_009A
                | 0x0400_009C
                | 0x0400_009E => {
                    self.write16_raw(a, val);
                    self.sound.psg_sync_wave(self.read16(0x0400_0070));
                    let off = (a - 0x0400_0090) as u8;
                    self.sound.psg_set_wave_byte(off, (val & 0xFF) as u8);
                    self.sound
                        .psg_set_wave_byte(off.wrapping_add(1), (val >> 8) as u8);
                    return;
                }
                // FIFO halfword writes (16-bit: 2 samples, GBATEK word = 4 samples)
                0x0400_00A0 | 0x0400_00A2 => {
                    self.sound.push_fifo_a_half(val);
                    return;
                }
                0x0400_00A4 | 0x0400_00A6 => {
                    self.sound.push_fifo_b_half(val);
                    return;
                }
                // SIOCNT — start bit (7) completes immediately so games that
                // poll transfer-done (bit 3) or the serial IRQ do not hang.
                0x0400_0128 => {
                    let mut v = val;
                    if v & (1 << 7) != 0 {
                        v &= !(1 << 7);
                        v |= 1 << 3; // transfer complete
                        self.write16_raw(a, v);
                        if v & (1 << 14) != 0 {
                            crate::irq::raise(self, crate::irq::IRQ_SERIAL);
                        }
                    } else {
                        self.write16_raw(a, v);
                    }
                    return;
                }
                _ => {}
            }
        }
        self.write16_raw(a, val);
    }

    /// Latch flag for PPU when affine refs are written (full 32-bit pairs).
    pub fn is_affine_ref_write(&self, addr: u32) -> bool {
        matches!(
            addr & !3,
            0x0400_0028 | 0x0400_002C | 0x0400_0038 | 0x0400_003C
        )
    }

    pub fn read32(&self, addr: u32) -> u32 {
        let a = addr & !3;
        // EEPROM: stream two halfwords (two serial bits)
        if (a >> 24) == 0x0D {
            let lo = self.read16(a) as u32;
            let hi = self.read16(a.wrapping_add(2)) as u32;
            return lo | (hi << 16);
        }
        u32::from_le_bytes([
            self.read8(a),
            self.read8(a.wrapping_add(1)),
            self.read8(a.wrapping_add(2)),
            self.read8(a.wrapping_add(3)),
        ])
    }

    pub fn write32(&mut self, addr: u32, val: u32) {
        let a = addr & !3;
        if crate::cpu::fairy_trace() {
            // 6 song structs' +4 and +0x34 fields
            const SONG_FIELDS: [u32; 12] = [
                0x0300_6FB4, 0x0300_6FE4, 0x0300_6F74, 0x0300_6FA4, 0x0300_73D4, 0x0300_7404,
                0x0300_7384, 0x0300_73B4, 0x0300_7344, 0x0300_7374, 0x0300_7304, 0x0300_7334,
            ];
            if SONG_FIELDS.contains(&a) {
                use std::sync::atomic::{AtomicU64, Ordering};
                static SW: [AtomicU64; 12] = [const { AtomicU64::new(0) }; 12];
                let idx = SONG_FIELDS.iter().position(|&x| x == a).unwrap();
                let c = SW[idx].fetch_add(1, Ordering::Relaxed);
                if c < 20 || c % 500 == 0 {
                    eprintln!(
                        "SONGWR {:08X}={:08X} evt{} pc={:08X} n{}",
                        a, val, self.dbg_evt, self.dbg_pc, c
                    );
                }
            }
            if a == 0x0300_5F78 {
                use std::sync::atomic::{AtomicU64, Ordering};
                static S28: AtomicU64 = AtomicU64::new(0);
                let c = S28.fetch_add(1, Ordering::Relaxed);
                if c < 20 || c % 500 == 0 {
                    eprintln!(
                        "S28WR {:08X}={:08X} evt{} pc={:08X} n{}",
                        a, val, self.dbg_evt, self.dbg_pc, c
                    );
                }
            }
        }
        // Sound FIFO A/B — word writes from DMA
        if a == 0x0400_00A0 {
            if crate::cpu::fairy_trace() {
                use std::sync::atomic::{AtomicU64, Ordering};
                static FW: AtomicU64 = AtomicU64::new(0);
                let c = FW.fetch_add(1, Ordering::Relaxed);
                if c < 40 || c % 500 == 0 {
                    eprintln!(
                        "FIFOW A {:08X} evt{} pc={:08X} len={}",
                        val, self.dbg_evt, self.dbg_pc, self.sound.fifo_a_len()
                    );
                }
            }
            self.sound.push_fifo_a_word(val);
            return;
        }
        if a == 0x0400_00A4 {
            self.sound.push_fifo_b_word(val);
            return;
        }
        let b = val.to_le_bytes();
        self.write16(a, u16::from_le_bytes([b[0], b[1]]));
        self.write16(a.wrapping_add(2), u16::from_le_bytes([b[2], b[3]]));
    }

    pub fn dispcnt(&self) -> u16 {
        self.read16(0x0400_0000)
    }

    pub fn set_vcount(&mut self, v: u16) {
        self.write16_raw(0x0400_0006, v);
    }

    pub fn dispstat(&self) -> u16 {
        self.read16(0x0400_0004)
    }

    pub fn set_dispstat(&mut self, v: u16) {
        self.write16_raw(0x0400_0004, v);
    }

    /// Advance DirectSound + PSG mix by CPU cycles; service FIFO DMA if half-empty.
    pub fn tick_sound(&mut self, cycles: u32) {
        let regs = self.psg_regs();
        let t0 = self.read16(0x0400_0102);
        let t1 = self.read16(0x0400_0106);
        let reloads = self.timer_reload;
        let ov = self.timer_overflows;
        self.timer_overflows = [0; 4];
        self.sound.step(cycles, &regs, reloads, t0, t1, ov);
        // Refill FIFOs when the mixer marked them half-empty (not only on HBlank).
        if self.sound.dma_req_a || self.sound.dma_req_b {
            let mut dma = std::mem::take(&mut self.dma);
            dma.on_fifo_request(self);
            self.dma = dma;
        }
    }

    fn psg_regs(&self) -> crate::sound::PsgRegs {
        let mut wave = [0u8; 16];
        for i in 0..16 {
            wave[i] = self.io.get(0x90 + i).copied().unwrap_or(0);
        }
        crate::sound::PsgRegs {
            sndl: self.read16(0x0400_0080),
            sndh: self.read16(0x0400_0082),
            sndx: self.read16(0x0400_0084),
            bias: self.read16(0x0400_0088),
            ch1_l: self.read16(0x0400_0060),
            ch1_h: self.read16(0x0400_0062),
            ch1_x: self.read16(0x0400_0064),
            ch2_l: self.read16(0x0400_0068),
            ch2_h: self.read16(0x0400_006C),
            ch3_l: self.read16(0x0400_0070),
            ch3_h: self.read16(0x0400_0072),
            ch3_x: self.read16(0x0400_0074),
            ch4_l: self.read16(0x0400_0078),
            ch4_h: self.read16(0x0400_007C),
            wave,
        }
    }

    /// If affine scale is unset (PA==0), install identity PA/PB/PC/PD for bg 2 or 3.
    fn ensure_affine_identity(bus: &mut Bus, bg: u32) {
        let base = if bg == 2 {
            0x0400_0020u32
        } else {
            0x0400_0030
        };
        let pa = bus.read16(base);
        if pa == 0 {
            bus.write16_raw(base, 0x0100); // PA
            bus.write16_raw(base + 2, 0);    // PB
            bus.write16_raw(base + 4, 0);    // PC
            bus.write16_raw(base + 6, 0x0100); // PD
        }
    }

    /// Cart data (CPU or DMA) uses the Game Pak bus: next fetch is N, prefetch dies.
    pub fn note_cart_data(&self) {
        self.last_fetch_ok.set(false);
        self.prefetch_halfs.set(0);
    }

    /// Instruction-fetch waitstates beyond the 1 I-cycle baseline.
    /// Sequential if `pc` is last_fetch+2 or +4 in the same region; else N-cycle.
    pub fn fetch_waitstates(&self, pc: u32) -> u32 {
        let sequential = self.last_fetch_ok.get()
            && (pc == self.last_fetch.get().wrapping_add(2)
                || pc == self.last_fetch.get().wrapping_add(4))
            && (pc >> 24) == (self.last_fetch.get() >> 24);
        self.last_fetch.set(pc);
        self.last_fetch_ok.set(true);

        let prefetch_on = self.read16(0x0400_0204) & (1 << 14) != 0;
        match pc >> 24 {
            0x00 | 0x03 | 0x04 | 0x05 | 0x06 | 0x07 => {
                if prefetch_on {
                    let n = self.prefetch_halfs.get();
                    self.prefetch_halfs.set((n + 1).min(8));
                }
                0
            }
            0x02 => 2, // EWRAM: +2 on 16/32-bit
            0x08 | 0x09 | 0x0A | 0x0B | 0x0C | 0x0D => {
                let (n_cycles, s_cycles) = self.rom_ns(pc);
                let total: u32 = if sequential {
                    if prefetch_on {
                        // I-cycle hides one S=1 fetch; keep a halfword queued.
                        self.prefetch_halfs.set(1);
                        1
                    } else {
                        s_cycles
                    }
                } else {
                    self.prefetch_halfs.set(0);
                    n_cycles
                };
                total.saturating_sub(1)
            }
            0x0E | 0x0F => self.sram_wait().saturating_sub(1),
            _ => 1,
        }
    }

    /// GBATEK: SRAM/Flash 0x0E wait = WAITCNT bits 0–1 → 4,3,2,8.
    fn sram_wait(&self) -> u32 {
        match self.read16(0x0400_0204) & 3 {
            0 => 4,
            1 => 3,
            2 => 2,
            _ => 8,
        }
    }

    fn rom_ns(&self, addr: u32) -> (u32, u32) {
        let waitcnt = self.read16(0x0400_0204);
        // S-cycle when the sequential bit is 0: WS0=2, WS1=4, WS2=8.
        let (n_shift, s_bit, s_slow) = match addr >> 24 {
            0x08 | 0x09 => (2u32, 4u32, 2u32),
            0x0A | 0x0B => (5, 7, 4),
            _ => (8, 10, 8),
        };
        let n = (waitcnt >> n_shift) & 3;
        let n_cycles = match n {
            0 => 4,
            1 => 3,
            2 => 2,
            _ => 8,
        };
        let s_cycles = if waitcnt & (1 << s_bit) != 0 {
            1
        } else {
            s_slow
        };
        (n_cycles, s_cycles)
    }

    /// Extra cycles on a data access (beyond the 1 I-cycle the insn already counted).
    pub fn data_waitstates(&self, addr: u32, bytes: u32) -> u32 {
        if matches!(addr >> 24, 0x08..=0x0D) {
            self.note_cart_data();
        }
        match addr >> 24 {
            0x00 | 0x03 | 0x04 => 0,
            0x02 => {
                if bytes >= 4 {
                    4
                } else {
                    2
                }
            }
            0x05 | 0x06 | 0x07 => {
                if bytes >= 4 {
                    1
                } else {
                    0
                }
            }
            0x08 | 0x09 | 0x0A | 0x0B | 0x0C | 0x0D => {
                let (n, s) = self.rom_ns(addr);
                let total = if bytes >= 4 { n.saturating_add(s) } else { n };
                total.saturating_sub(1)
            }
            0x0E | 0x0F => self.sram_wait().saturating_sub(1),
            _ => 0,
        }
    }

    /// Extra cycles for a burst of `words` 32-bit transfers (LDM/STM).
    pub fn data_burst_waitstates(&self, addr: u32, words: u32) -> u32 {
        let words = words.max(1);
        let first = self.data_waitstates(addr, 4);
        if words == 1 {
            return first;
        }
        let rest = match addr >> 24 {
            0x02 => 2 * (words - 1),
            0x05 | 0x06 | 0x07 => words - 1,
            0x08 | 0x09 | 0x0A | 0x0B | 0x0C | 0x0D => {
                let (_, s) = self.rom_ns(addr);
                s.saturating_sub(1).saturating_mul(words - 1)
            }
            _ => 0,
        };
        first.saturating_add(rest)
    }

    pub fn set_keys_pressed(&mut self, pressed_mask: u16) {
        let prev = self.keyinput;
        self.keyinput = (!pressed_mask) & 0x03FF;
        // KEYCNT IRQ (0x04000132): bit14=irq enable, bit15=AND(1)/OR(0)
        let keycnt = self.read16(0x0400_0132);
        if keycnt & (1 << 14) == 0 {
            return;
        }
        let mask = keycnt & 0x03FF;
        if mask == 0 {
            return;
        }
        // pressed bits are 0 in keyinput
        let pressed = (!self.keyinput) & 0x03FF;
        let fire = if keycnt & (1 << 15) != 0 {
            // AND: all selected keys pressed
            pressed & mask == mask
        } else {
            // OR: any selected key pressed
            pressed & mask != 0
        };
        // edge: only when newly matching (avoid re-fire every frame while held)
        let was = (!prev) & 0x03FF;
        let was_fire = if keycnt & (1 << 15) != 0 {
            was & mask == mask
        } else {
            was & mask != 0
        };
        if fire && !was_fire {
            crate::irq::raise(self, crate::irq::IRQ_KEYPAD);
        }
    }

    /// 8-bit VRAM: BG (and bitmap) writes the byte to both halves of the
    /// halfword; OBJ VRAM ignores the store (GBATEK).
    fn write8_vram(&mut self, addr: u32, val: u8) {
        let idx = vram_index(addr);
        let window = (addr as usize) & 0x1FFFF;
        let mode = self.dispcnt() & 7;
        let mut obj = idx >= 0x10000;
        if mode >= 3 && window < 0x14000 {
            obj = false;
        }
        if obj {
            return;
        }
        let aligned = idx & !1;
        if aligned + 1 < self.vram.len() {
            self.vram[aligned] = val;
            self.vram[aligned + 1] = val;
        }
    }
}

/// GBATEK: unused IO (and write-only regs on read) is open bus, not 00.
pub(crate) fn io_readable(off: u16) -> bool {
    matches!(
        off,
        0x000..=0x04D
            | 0x050..=0x055
            | 0x060..=0x065
            | 0x068..=0x069
            | 0x06C..=0x06D
            | 0x070..=0x075
            | 0x078..=0x079
            | 0x07C..=0x07D
            | 0x080..=0x085
            | 0x088..=0x089
            | 0x090..=0x09F
            | 0x0B0..=0x0DF
            | 0x100..=0x10F
            | 0x120..=0x12B
            | 0x130..=0x137
            | 0x140..=0x141
            | 0x150..=0x159
            | 0x200..=0x205
            | 0x208
            | 0x300
    )
}

/// KEYINPUT / VCOUNT / IME-high are not writable; FIFO and HALTCNT are.
pub(crate) fn io_writable(off: u16) -> bool {
    match off {
        0x006 | 0x007 => false, // VCOUNT
        0x130 | 0x131 => false, // KEYINPUT
        0x209 => false,         // IME high byte unused
        0x0A0..=0x0A7 => true,  // FIFO A/B (write-only)
        0x202 | 0x203 => true,  // IF (write-1-to-clear)
        0x301 => true,          // HALTCNT
        _ => io_readable(off),
    }
}

/// GBATEK: 96K VRAM (64K+32K) repeats every 128K; the upper 32K of each
/// 128K window mirrors the OBJ 32K (`06018000` → `06010000`), not `% 96K`.
pub(crate) fn vram_index(addr: u32) -> usize {
    let a = (addr as usize) & 0x1FFFF;
    if a >= 0x18000 {
        a - 0x8000
    } else {
        a
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cart::Cart;

    #[test]
    fn untagged_cart_promotes_sram_on_write() {
        let cart = Cart {
            data: vec![0u8; 0x200],
            title: "t".into(),
            game_code: "T".into(),
            maker: "00".into(),
            path: "m".into(),
            inner_name: None,
        };
        let mut bus = Bus::new(&cart, None);
        assert_eq!(bus.save_type, SaveType::None);
        assert_eq!(bus.read8(0x0E00_0000), 0xFF, "unread window is open/empty");
        bus.write8(0x0E00_0000, 0x42);
        assert!(bus.save_dirty, "first 0x0E write becomes SRAM");
        assert!(matches!(bus.save_type, SaveType::Sram(_)));
        assert_eq!(bus.read8(0x0E00_0000), 0x42);
    }

    #[test]
    fn siocnt_start_completes_and_can_irq() {
        let cart = Cart {
            data: vec![0u8; 0x200],
            title: "t".into(),
            game_code: "T".into(),
            maker: "00".into(),
            path: "m".into(),
            inner_name: None,
        };
        let mut bus = Bus::new(&cart, None);
        bus.write16(0x0400_0128, (1 << 7) | (1 << 14));
        let v = bus.read16(0x0400_0128);
        assert_eq!(v & (1 << 7), 0, "start bit clears");
        assert_eq!(v & (1 << 3), 1 << 3, "transfer complete");
        assert_eq!(bus.read16(0x0400_0202) & (1 << 7), 1 << 7, "serial IRQ");
    }

    #[test]
    fn soundcnt_x_live_bits_not_writable() {
        let cart = Cart {
            data: vec![0u8; 0x200],
            title: "t".into(),
            game_code: "T".into(),
            maker: "00".into(),
            path: "m".into(),
            inner_name: None,
        };
        let mut bus = Bus::new(&cart, None);
        bus.write16(0x0400_0084, 0x008F);
        assert_eq!(bus.read16(0x0400_0084) & 0x80, 0x80);
        assert_eq!(bus.read16(0x0400_0084) & 0x0F, 0, "bits 0–3 are live, not latched");
        bus.write16(0x0400_0084, 0);
        assert_eq!(bus.read16(0x0400_0084) & 0x80, 0);
    }

    #[test]
    fn bios_hidden_when_pc_outside() {
        let cart = Cart {
            data: vec![0u8; 0x200],
            title: "t".into(),
            game_code: "T".into(),
            maker: "00".into(),
            path: "m".into(),
            inner_name: None,
        };
        let mut bus = Bus::new(&cart, Some(vec![0xEA; BIOS_SIZE]));
        bus.exec_pc = 0x0800_0000;
        assert_eq!(bus.read8(0), 0, "ROM code must not dump BIOS");
        let _ = bus.read16(0x0800_0000);
        assert_eq!(
            bus.read32(0x0000_000C),
            0,
            "BIOS open bus is last BIOS fetch (0 after HLE reset), not last ROM halfword"
        );
        bus.exec_pc = 0x0000_0138;
        assert_eq!(bus.read8(0), 0xEA, "BIOS code can read the image");
        bus.write32(0x0300_0000, 0xA1B2_C3D4);
        let _ = bus.read32(0x0300_0000);
        assert_eq!(
            bus.read8(0x0000_4000),
            0xD4,
            "00004000+ is unused, not a BIOS mirror"
        );
    }

    #[test]
    fn vram_128k_window_mirrors_obj_not_bg() {
        let cart = Cart {
            data: vec![0u8; 0x200],
            title: "t".into(),
            game_code: "T".into(),
            maker: "00".into(),
            path: "m".into(),
            inner_name: None,
        };
        let mut bus = Bus::new(&cart, None);
        // OBJ is 16-bit only; 8-bit stores are ignored (see byte_writes test).
        bus.write16(0x0601_0000, 0xABAB);
        bus.write8(0x0600_0000, 0x11);
        assert_eq!(bus.read8(0x0601_8000), 0xAB, "06018000 mirrors OBJ 32K");
        assert_ne!(bus.read8(0x0601_8000), 0x11, "must not wrap onto BG 64K");
        bus.write16(0x0601_8004, 0xCDCD);
        assert_eq!(bus.read8(0x0601_0004), 0xCD);
        bus.write8(0x0602_0008, 0xEF); // +128K → same 64K BG (8-bit duplicates)
        assert_eq!(bus.read8(0x0600_0008), 0xEF);
        assert_eq!(vram_index(0x0601_8000), 0x10000);
        assert_eq!(vram_index(0x0600_FFFF), 0x0FFFF);
        assert_eq!(vram_index(0x0601_7FFF), 0x17FFF);
    }

    #[test]
    fn sequential_rom_fetch_cheaper_than_n() {
        let cart = Cart {
            data: vec![0u8; 0x200],
            title: "t".into(),
            game_code: "T".into(),
            maker: "00".into(),
            path: "m".into(),
            inner_name: None,
        };
        let bus = Bus::new(&cart, None);
        // WAITCNT=0 → WS0 N=4, S=2
        let n = bus.fetch_waitstates(0x0800_0000);
        let s = bus.fetch_waitstates(0x0800_0004);
        let n2 = bus.fetch_waitstates(0x0800_1000);
        assert_eq!(n, 3, "N-cycle extra");
        assert_eq!(s, 1, "S-cycle extra");
        assert_eq!(n2, 3, "taken branch is N again");
        assert!(s < n);
    }

    #[test]
    fn rom_data_access_breaks_sequential_fetch() {
        let cart = Cart {
            data: vec![0u8; 0x200],
            title: "t".into(),
            game_code: "T".into(),
            maker: "00".into(),
            path: "m".into(),
            inner_name: None,
        };
        let bus = Bus::new(&cart, None);
        let n = bus.fetch_waitstates(0x0800_0000);
        let s = bus.fetch_waitstates(0x0800_0004);
        let _ = bus.data_waitstates(0x0800_0100, 4);
        let after = bus.fetch_waitstates(0x0800_0008);
        assert_eq!(s, 1);
        assert_eq!(after, n, "LDR from ROM must make the next fetch an N-cycle");
    }

    #[test]
    fn rom_32bit_data_costs_n_plus_s() {
        let cart = Cart {
            data: vec![0u8; 0x200],
            title: "t".into(),
            game_code: "T".into(),
            maker: "00".into(),
            path: "m".into(),
            inner_name: None,
        };
        let bus = Bus::new(&cart, None);
        let w16 = bus.data_waitstates(0x0800_0000, 2);
        let w32 = bus.data_waitstates(0x0800_0000, 4);
        assert_eq!(w16, 3, "N=4 minus the 1 already in the insn");
        assert_eq!(w32, 5, "N+S=6 minus 1");
        assert!(w32 > w16);
        let iwram = bus.data_waitstates(0x0300_0000, 4);
        assert_eq!(iwram, 0);
    }

    #[test]
    fn ws1_ws2_sequential_use_gbatek_s() {
        let cart = Cart {
            data: vec![0u8; 0x200],
            title: "t".into(),
            game_code: "T".into(),
            maker: "00".into(),
            path: "m".into(),
            inner_name: None,
        };
        let mut bus = Bus::new(&cart, None);
        // WAITCNT=0: WS1 S=4, WS2 S=8
        let _ = bus.fetch_waitstates(0x0A00_0000);
        assert_eq!(bus.fetch_waitstates(0x0A00_0004), 3, "WS1 S=4 extra");
        let _ = bus.fetch_waitstates(0x0C00_0000);
        assert_eq!(bus.fetch_waitstates(0x0C00_0004), 7, "WS2 S=8 extra");
        // sequential bits on: S=1 for all
        bus.write16(0x0400_0204, (1 << 7) | (1 << 10));
        let _ = bus.fetch_waitstates(0x0A00_0000);
        assert_eq!(bus.fetch_waitstates(0x0A00_0004), 0, "WS1 S=1 extra");
        let _ = bus.fetch_waitstates(0x0C00_0000);
        assert_eq!(bus.fetch_waitstates(0x0C00_0004), 0, "WS2 S=1 extra");
    }

    #[test]
    fn sram_wait_follows_waitcnt() {
        let cart = Cart {
            data: vec![0u8; 0x200],
            title: "t".into(),
            game_code: "T".into(),
            maker: "00".into(),
            path: "m".into(),
            inner_name: None,
        };
        let mut bus = Bus::new(&cart, None);
        assert_eq!(bus.data_waitstates(0x0E00_0000, 1), 3, "default SRAM 4-1");
        bus.write16(0x0400_0204, 3);
        assert_eq!(bus.data_waitstates(0x0E00_0000, 1), 7, "SRAM wait 8-1");
    }

    fn tiny_bus() -> Bus {
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
    fn rom_halfword_open_bus_duplicates_on_both_halves() {
        let mut data = vec![0u8; 0x200];
        data[0..2].copy_from_slice(&0x7801u16.to_le_bytes());
        let cart = Cart {
            data,
            title: "t".into(),
            game_code: "T".into(),
            maker: "00".into(),
            path: "m".into(),
            inner_name: None,
        };
        let mut bus = Bus::new(&cart, Some(vec![0xEA; BIOS_SIZE]));
        bus.exec_pc = 0x0800_0000;
        assert_eq!(bus.read16(0x0800_0000), 0x7801);
        // Unused 00004000+ is last-bus (not BIOS). Thumb latch is 7801_7801.
        assert_eq!(bus.read8(0x0000_4000), 0x01);
        assert_eq!(bus.read8(0x0000_4001), 0x78);
        assert_eq!(bus.read8(0x0000_4002), 0x01);
        assert_eq!(bus.read8(0x0000_4003), 0x78);
        assert_eq!(bus.read32(0x0000_4000), 0x7801_7801);
        assert_eq!(bus.read32(0x0000_000C), 0, "BIOS itself stays last-BIOS (0)");
    }

    #[test]
    fn iwram_halfword_open_bus_is_the_aligned_word() {
        let mut bus = tiny_bus();
        bus.write32(0x0300_0000, 0xA1B2_C3D4);
        let _ = bus.read16(0x0300_0002);
        assert_eq!(
            bus.read8(0x0000_4000),
            0xD4,
            "IWRAM is a 32-bit bus: unused read sees the whole word"
        );
        assert_eq!(bus.read8(0x0000_4001), 0xC3);
        assert_eq!(bus.read8(0x0000_4002), 0xB2);
        assert_eq!(bus.read8(0x0000_4003), 0xA1);
    }

    #[test]
    fn vram_halfword_open_bus_duplicates() {
        let mut bus = tiny_bus();
        bus.write16(0x0600_0000, 0x1F3F);
        let _ = bus.read16(0x0600_0000);
        assert_eq!(bus.read8(0x0000_4000), 0x3F);
        assert_eq!(bus.read8(0x0000_4001), 0x1F);
        assert_eq!(bus.read8(0x0000_4002), 0x3F);
        assert_eq!(bus.read8(0x0000_4003), 0x1F);
    }

    #[test]
    fn unused_io_is_open_bus_not_zero() {
        let mut bus = tiny_bus();
        bus.write32(0x0300_0000, 0xA1B2_C3D4);
        let _ = bus.read32(0x0300_0000);
        bus.write16(0x0400_004E, 0xBEEF);
        assert_ne!(
            bus.read16(0x0400_004E),
            0xBEEF,
            "unused MOSAIC pad must not latch"
        );
        // 0xE0 is unused; addr&3==0 → last-word low byte
        assert_eq!(bus.read8(0x0400_00E0), 0xD4, "open bus byte 0 of last word");
        assert_eq!(bus.read8(0x0400_00A0), 0xD4, "FIFO A is write-only");
        assert_eq!(bus.read16(0x0400_0000), 0x0080, "DISPCNT still mapped");
    }

    #[test]
    fn haltcnt_byte_write_halts() {
        let mut bus = tiny_bus();
        assert_eq!(bus.read8(0x0400_0300), 1, "HLE boot leaves POSTFLG set");
        assert!(!bus.halt_wait);
        bus.write8(0x0400_0300, 1);
        assert!(!bus.halt_wait, "POSTFLG alone is not Halt");
        assert_eq!(bus.read8(0x0400_0300), 1);
        bus.write8(0x0400_0301, 0);
        assert!(bus.halt_wait);
        bus.write32(0x0300_0000, 0xA1B2_C3D4);
        let _ = bus.read32(0x0300_0000);
        assert_eq!(
            bus.read8(0x0400_0301),
            0xC3,
            "HALTCNT reads are open bus, not the written 0"
        );
    }

    #[test]
    fn ime_and_ie_mask_unused_bits() {
        let mut bus = tiny_bus();
        bus.write16(0x0400_0208, 0xFFFF);
        assert_eq!(bus.read16(0x0400_0208) & 0xFF, 1, "IME is bit 0 only");
        bus.write16(0x0400_0200, 0xFFFF);
        assert_eq!(bus.read16(0x0400_0200), 0x3FFF);
        let keys = bus.read16(0x0400_0130);
        bus.write16(0x0400_0130, 0);
        assert_eq!(bus.read16(0x0400_0130), keys, "KEYINPUT is read-only");
    }

    #[test]
    fn pal_oam_ignore_byte_writes() {
        let mut bus = tiny_bus();
        bus.write16(0x0500_0000, 0x7FFF);
        bus.write8(0x0500_0000, 0x12);
        assert_eq!(bus.read16(0x0500_0000), 0x7FFF);
        bus.write16(0x0700_0000, 0x1234);
        bus.write8(0x0700_0000, 0xAB);
        assert_eq!(bus.read16(0x0700_0000), 0x1234);
    }

    #[test]
    fn vram_byte_write_bg_duplicates_obj_ignored() {
        let mut bus = tiny_bus();
        bus.write16(0x0600_0000, 0x0000);
        bus.write8(0x0600_0000, 0x5A);
        assert_eq!(bus.read16(0x0600_0000), 0x5A5A, "BG 8-bit writes both bytes");
        bus.write16(0x0601_0000, 0x1111);
        bus.write8(0x0601_0000, 0x22);
        assert_eq!(bus.read16(0x0601_0000), 0x1111, "OBJ 8-bit writes ignored");
        bus.write16(0x0400_0000, 0x0003); // mode 3 bitmap
        bus.write8(0x0601_0000, 0x33);
        assert_eq!(
            bus.read16(0x0601_0000),
            0x3333,
            "mode-3 bitmap allows 8-bit dual write"
        );
    }

    #[test]
    fn dispstat_write_keeps_live_flags() {
        let mut bus = tiny_bus();
        bus.write16_raw(0x0400_0004, 0x0007);
        bus.write16(0x0400_0004, 0x2800);
        assert_eq!(bus.read16(0x0400_0004) & 7, 7, "v/h/vc flags stay");
        assert_eq!(bus.read16(0x0400_0004) & 0x2800, 0x2800);
        bus.write16(0x0400_0204, 0xFFFF);
        assert_eq!(bus.read16(0x0400_0204) & 0x8000, 0, "WAITCNT bit15 is 0");
        crate::irq::raise(&mut bus, crate::irq::IRQ_VBLANK);
        bus.write16(0x0400_0202, 0xC000);
        assert_eq!(
            bus.read16(0x0400_0202) & 0xC000,
            0,
            "IF bits 14–15 do not exist"
        );
        assert_eq!(bus.read16(0x0400_0202) & 1, 1, "W1C unused bits leave IF");
        bus.write16(0x0400_0048, 0xFFFF);
        assert_eq!(bus.read16(0x0400_0048), 0x3F3F, "WININ unused bits");
        bus.write16(0x0400_0050, 0xFFFF);
        assert_eq!(bus.read16(0x0400_0050), 0x3FFF, "BLDCNT unused bits");
        bus.write16(0x0400_0102, 0x00FF);
        assert_eq!(bus.read16(0x0400_0102), 0x00C7, "TMxCNT unused bits");
    }
}
