//! Cartridge battery saves (SRAM / Flash / EEPROM) + paths for .sav files.

use crate::recents;
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveType {
    None,
    /// Size in bytes (typically 32 KiB or 64 KiB)
    Sram(usize),
    /// 64 KiB flash
    Flash64,
    /// 128 KiB flash (banked)
    Flash128,
    /// 512-byte EEPROM (6-bit address)
    Eeprom512,
    /// 8 KiB EEPROM (14-bit address)
    Eeprom8K,
}

impl SaveType {
    pub fn size(self) -> usize {
        match self {
            SaveType::None => 0,
            SaveType::Sram(n) => n,
            SaveType::Flash64 => 64 * 1024,
            SaveType::Flash128 => 128 * 1024,
            SaveType::Eeprom512 => 512,
            SaveType::Eeprom8K => 8 * 1024,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SaveType::None => "none",
            SaveType::Sram(n) if n <= 32 * 1024 => "SRAM 32K",
            SaveType::Sram(_) => "SRAM 64K",
            SaveType::Flash64 => "FLASH 64K",
            SaveType::Flash128 => "FLASH 128K",
            SaveType::Eeprom512 => "EEPROM 512B",
            SaveType::Eeprom8K => "EEPROM 8K",
        }
    }

    pub fn is_eeprom(self) -> bool {
        matches!(self, SaveType::Eeprom512 | SaveType::Eeprom8K)
    }
}

/// Detect save type from strings embedded in the ROM (Nintendo SDK tags).
pub fn detect(rom: &[u8]) -> SaveType {
    let s = String::from_utf8_lossy(rom);
    // Order matters: more specific first
    if s.contains("FLASH1M_V") || s.contains("FLASH1M") {
        return SaveType::Flash128;
    }
    if s.contains("FLASH512_V") || s.contains("FLASH_V") || s.contains("FLASH512") {
        return SaveType::Flash64;
    }
    if s.contains("SRAM_V") || s.contains("SRAM_F_V") {
        // Most SRAM carts are 32K; some 64K — use 64K buffer, games only use what they need
        return SaveType::Sram(64 * 1024);
    }
    if s.contains("EEPROM_V") {
        // Size is often only known after first transfer bit-length; default 8K
        // and auto-narrow on short address streams (see EepromChip).
        return SaveType::Eeprom8K;
    }
    // Untagged carts stay None at detect time. First write to 0x0E000000
    // promotes to SRAM (homebrew without an SDK string). Flash/EEPROM carts
    // carry a tag, so they never sit on None.
    SaveType::None
}

/// Manufacturer/device IDs the SDK flash lib compares against.
///
/// Only chips that speak the AMD unlock we implement. Never report Atmel
/// (`0x1F`/`0x3D`) — that library uses a different command set.
pub fn flash_ids(rom: &[u8], size: usize) -> (u8, u8) {
    let s = String::from_utf8_lossy(rom);
    if size >= 128 * 1024 {
        // FLASH1M_V102 Macronix 0xC2/0x09; V103 (FRLG / LC) Sanyo 0x62/0x13.
        if s.contains("FLASH1M_V102") {
            return (0xC2, 0x09);
        }
        return (0x62, 0x13);
    }
    if s.contains("PANASONIC") {
        return (0x32, 0x1B);
    }
    if s.contains("MACRONIX") {
        return (0xC2, 0x1C);
    }
    (0xBF, 0xD4) // SST — FLASH512_V13x default
}

/// `.sav` next to the ROM, or under data dir if ROM path is weird.
pub fn sav_path_for_rom(rom: &Path) -> PathBuf {
    let stem = rom.file_stem().and_then(|s| s.to_str()).unwrap_or("fable");
    if let Some(parent) = rom.parent() {
        if parent.exists() {
            return parent.join(format!("{stem}.sav"));
        }
    }
    let dir = recents::data_dir().join("saves");
    let _ = fs::create_dir_all(&dir);
    dir.join(format!("{stem}.sav"))
}

pub fn state_path_for_rom(rom: &Path) -> PathBuf {
    let stem = rom.file_stem().and_then(|s| s.to_str()).unwrap_or("fable");
    let dir = recents::data_dir().join("states");
    let _ = fs::create_dir_all(&dir);
    dir.join(format!("{stem}.flst"))
}

pub fn load_sav(path: &Path, size: usize) -> Vec<u8> {
    let mut buf = vec![0xFF; size.max(1)];
    if size == 0 {
        return buf;
    }
    if let Ok(data) = fs::read(path) {
        let n = data.len().min(size);
        buf[..n].copy_from_slice(&data[..n]);
    }
    buf
}

pub fn save_sav(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).ok();
    }
    fs::write(path, data).with_context(|| format!("write battery {}", path.display()))?;
    Ok(())
}

/// Flash command state machine (64K / 128K, simplified).
#[derive(Clone, Debug, Default)]
pub struct FlashChip {
    pub data: Vec<u8>,
    pub bank: usize,
    cmd_step: u8,
    /// 0 = ready, 1 = ID mode, 2 = erase setup, 3 = write byte
    mode: u8,
    manufacturer: u8,
    device: u8,
}

impl FlashChip {
    pub fn new(size: usize) -> Self {
        let (m, d) = flash_ids(&[], size);
        Self::with_ids(size, m, d)
    }

    /// Pick IDs from the ROM's SDK tag (FLASH1M_V103 → Sanyo, etc.).
    pub fn new_for(size: usize, rom: &[u8]) -> Self {
        let (m, d) = flash_ids(rom, size);
        Self::with_ids(size, m, d)
    }

    fn with_ids(size: usize, manufacturer: u8, device: u8) -> Self {
        Self {
            data: vec![0xFF; size.max(64 * 1024)],
            bank: 0,
            cmd_step: 0,
            mode: 0,
            manufacturer,
            device,
        }
    }

    pub fn restore_fsm(&mut self, mode: u8, cmd_step: u8, bank: usize) {
        self.mode = mode;
        self.cmd_step = cmd_step;
        self.bank = bank;
    }

    /// (mode, cmd_step, bank, manufacturer, device)
    pub fn debug_fsm(&self) -> (u8, u8, usize, u8, u8) {
        (
            self.mode,
            self.cmd_step,
            self.bank,
            self.manufacturer,
            self.device,
        )
    }

    fn bank_base(&self) -> usize {
        // 128K: two 64K banks
        if self.data.len() >= 128 * 1024 {
            self.bank.saturating_mul(64 * 1024)
        } else {
            0
        }
    }

    pub fn read(&self, addr: u32) -> u8 {
        let off = (addr as usize) & 0xFFFF;
        if self.mode == 1 {
            // ID mode
            return match off {
                0 => self.manufacturer,
                1 => self.device,
                _ => 0xFF,
            };
        }
        let i = self.bank_base() + off;
        self.data.get(i).copied().unwrap_or(0xFF)
    }

    pub fn write(&mut self, addr: u32, val: u8) -> bool {
        let off = (addr as usize) & 0xFFFF;

        // Program / bank-select consume the next write (no unlock prefix).
        if self.mode == 3 {
            let i = self.bank_base() + off;
            if i < self.data.len() {
                self.data[i] &= val; // flash programs 1→0
            }
            self.mode = 0;
            self.cmd_step = 0;
            return true;
        }
        if self.mode == 4 {
            self.bank = (val & 1) as usize;
            self.mode = 0;
            self.cmd_step = 0;
            return false;
        }

        // AMD unlock: [5555]=AA, [2AAA]=55, then the command byte.
        match self.cmd_step {
            0 if off == 0x5555 && val == 0xAA => {
                self.cmd_step = 1;
                return false;
            }
            1 if off == 0x2AAA && val == 0x55 => {
                self.cmd_step = 2;
                return false;
            }
            2 => {
                self.cmd_step = 0;
                // Second AA/55 after erase-prep (80h): 10h chip, 30h 4K sector.
                // Must be handled here — the idle decoder used to swallow 10/30.
                if self.mode == 2 {
                    if val == 0x10 {
                        self.data.fill(0xFF);
                        self.mode = 0;
                        return true;
                    }
                    if val == 0x30 {
                        let base = self.bank_base() + (off & !0xFFF);
                        for b in self.data.iter_mut().skip(base).take(0x1000) {
                            *b = 0xFF;
                        }
                        self.mode = 0;
                        return true;
                    }
                    self.mode = 0;
                    return false;
                }
                match val {
                    0x90 => self.mode = 1, // ID
                    0xF0 => self.mode = 0,
                    0x80 => self.mode = 2, // erase prep
                    0xA0 => self.mode = 3, // program next byte
                    0xB0 => self.mode = 4, // bank
                    _ => {}
                }
                return false;
            }
            _ => {
                self.cmd_step = 0;
            }
        }

        // Many SDK drivers write F0 to leave ID without a full prefix.
        if val == 0xF0 {
            self.mode = 0;
            self.cmd_step = 0;
        }
        false
    }
}

/// EEPROM serial engine (not the data array) for savestates.
#[derive(Clone, Copy, Debug)]
pub struct EepromFsm {
    pub addr_bits: u8,
    pub bits: u64,
    pub bit_count: u8,
    pub phase: u8,
    pub dirty: bool,
    pub read_stream: u128,
    pub read_left: u8,
    pub write_addr: u16,
    pub write_buf: u64,
}

/// Serial EEPROM (512 B or 8 KiB) bit-bang on bus `0x0Dxxxxxx`.
///
/// Games DMA halfwords to/from the cart; only bit 0 of each halfword is the
/// serial line. Protocol (GBATEK):
/// - write: `1 | 10 | addr | 64 data bits | 0`
/// - read:  `1 | 11 | addr` then read `4 dummy + 64 data bits`
#[derive(Clone, Debug)]
pub struct EepromChip {
    pub data: Vec<u8>,
    /// Address width in bits (6 → 512B, 14 → 8K). Auto-detected on first short stream.
    addr_bits: u8,
    bits: u64,
    bit_count: u8,
    /// 0 idle, 1 header, 2 write-data, 3 read-out
    phase: u8,
    pub dirty: bool,
    /// 68-bit stream: 4 dummy zeros then 64 data bits (MSB first of data).
    read_stream: u128,
    read_left: u8,
    write_addr: u16,
    write_buf: u64,
}

impl Default for EepromChip {
    fn default() -> Self {
        Self::new(8 * 1024)
    }
}

impl EepromChip {
    pub fn new(size: usize) -> Self {
        let size = if size <= 512 { 512 } else { 8 * 1024 };
        let addr_bits = if size <= 512 { 6 } else { 14 };
        Self {
            data: vec![0xFF; size],
            addr_bits,
            bits: 0,
            bit_count: 0,
            phase: 0,
            dirty: false,
            read_stream: 0,
            read_left: 0,
            write_addr: 0,
            write_buf: 0,
        }
    }

    pub fn from_save_type(st: SaveType) -> Option<Self> {
        match st {
            SaveType::Eeprom512 => Some(Self::new(512)),
            SaveType::Eeprom8K => Some(Self::new(8 * 1024)),
            _ => None,
        }
    }

    pub fn snapshot_fsm(&self) -> EepromFsm {
        EepromFsm {
            addr_bits: self.addr_bits,
            bits: self.bits,
            bit_count: self.bit_count,
            phase: self.phase,
            dirty: self.dirty,
            read_stream: self.read_stream,
            read_left: self.read_left,
            write_addr: self.write_addr,
            write_buf: self.write_buf,
        }
    }

    pub fn restore_fsm(&mut self, s: EepromFsm) {
        self.addr_bits = s.addr_bits;
        self.bits = s.bits;
        self.bit_count = s.bit_count;
        self.phase = s.phase;
        self.dirty = s.dirty;
        self.read_stream = s.read_stream;
        self.read_left = s.read_left;
        self.write_addr = s.write_addr;
        self.write_buf = s.write_buf;
    }

    /// Serial read halfword (bit0 = next bit; idle returns 1).
    /// First 4 of 68 bits are dummy zeros, then 64 data bits MSB-first.
    pub fn read_serial(&mut self) -> u16 {
        if self.phase != 3 || self.read_left == 0 {
            return 1;
        }
        let pos = self.read_left; // 68 .. 1
        self.read_left -= 1;
        if self.read_left == 0 {
            self.phase = 0;
        }
        if pos > 64 {
            return 0;
        }
        ((self.read_stream >> (pos - 1)) & 1) as u16
    }

    /// Serial write: consume bit0 of halfword.
    pub fn write_bit(&mut self, half: u16) {
        let bit = (half & 1) as u64;
        match self.phase {
            0 => {
                if bit == 0 {
                    return;
                }
                self.phase = 1;
                self.bits = 1;
                self.bit_count = 1;
            }
            1 => {
                self.bits = (self.bits << 1) | bit;
                self.bit_count = self.bit_count.saturating_add(1);

                // 512B devices: DMA often sends exactly 9 bits (1+2+6).
                if self.bit_count == 9 && self.addr_bits == 14 {
                    let cmd = (self.bits >> 6) & 3;
                    if cmd == 0b10 || cmd == 0b11 {
                        self.addr_bits = 6;
                        self.finish_header();
                        return;
                    }
                }

                let need = 3 + self.addr_bits; // start+cmd+addr
                if self.bit_count == need {
                    self.finish_header();
                }
            }
            2 => {
                self.write_buf = (self.write_buf << 1) | bit;
                self.bit_count = self.bit_count.saturating_add(1);
                if self.bit_count >= 64 {
                    self.commit_write();
                    // Trailing stop bit (0) may follow; ignore via idle.
                    self.phase = 0;
                    self.bit_count = 0;
                    self.bits = 0;
                }
            }
            3 => {
                // New command while reading — restart if start bit.
                self.phase = 0;
                self.read_left = 0;
                self.bit_count = 0;
                self.bits = 0;
                if bit == 1 {
                    self.phase = 1;
                    self.bits = 1;
                    self.bit_count = 1;
                }
            }
            _ => self.phase = 0,
        }
    }

    fn finish_header(&mut self) {
        let ab = self.addr_bits as u32;
        let cmd = ((self.bits >> ab) & 3) as u8;
        let addr = (self.bits & ((1u64 << ab) - 1)) as u16;
        self.write_addr = addr;
        match cmd {
            0b10 => {
                self.phase = 2;
                self.bit_count = 0;
                self.write_buf = 0;
            }
            0b11 => {
                let byte_off = (addr as usize).saturating_mul(8);
                let mut data64 = 0u64;
                for i in 0..8 {
                    let b = self.data.get(byte_off + i).copied().unwrap_or(0xFF) as u64;
                    data64 = (data64 << 8) | b;
                }
                // 68 bits: 0000 || data64  (MSB of stream emitted first)
                self.read_stream = data64 as u128; // low 64 = data
                // Position dummy zeros in bits 67..64: stream = data64, left=68
                // When left=68..65 → bits 67..64 of a 68-bit value are 0 if we
                // only stored 64 bits and index as (left-1) only for data portion:
                self.read_stream = data64 as u128;
                self.read_left = 68;
                self.phase = 3;
                self.bit_count = 0;
                self.bits = 0;
            }
            _ => {
                self.phase = 0;
                self.bit_count = 0;
                self.bits = 0;
            }
        }
    }

    fn commit_write(&mut self) {
        let byte_off = (self.write_addr as usize).saturating_mul(8);
        for i in 0..8 {
            let shift = (7 - i) * 8;
            let b = ((self.write_buf >> shift) & 0xFF) as u8;
            if byte_off + i < self.data.len() {
                self.data[byte_off + i] = b;
            }
        }
        self.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unlock(flash: &mut FlashChip) {
        flash.write(0x5555, 0xAA);
        flash.write(0x2AAA, 0x55);
    }

    /// SDK FLASH1M sector erase: AA/55/80, AA/55, then 30 at the sector.
    fn sector_erase(flash: &mut FlashChip, sector: u32) {
        unlock(flash);
        flash.write(0x5555, 0x80);
        unlock(flash);
        flash.write(sector, 0x30);
    }

    fn program(flash: &mut FlashChip, addr: u32, val: u8) {
        unlock(flash);
        flash.write(0x5555, 0xA0);
        flash.write(addr, val);
    }

    #[test]
    fn untagged_rom_is_not_sram() {
        assert_eq!(detect(&[0u8; 256]), SaveType::None);
        let mut sram = vec![0u8; 256];
        sram[10..16].copy_from_slice(b"SRAM_V");
        assert!(matches!(detect(&sram), SaveType::Sram(_)));
    }

    #[test]
    fn flash_ids_follow_sdk_tag() {
        assert_eq!(flash_ids(b"FLASH1M_V103", 128 * 1024), (0x62, 0x13));
        assert_eq!(flash_ids(b"FLASH1M_V102", 128 * 1024), (0xC2, 0x09));
        assert_eq!(flash_ids(b"FLASH512_V131", 64 * 1024), (0xBF, 0xD4));
        assert_eq!(flash_ids(b"PANASONIC FLASH_V", 64 * 1024), (0x32, 0x1B));
        // Never claim Atmel — we do not implement its command set.
        assert_ne!(flash_ids(b"FLASH512_V", 64 * 1024).0, 0x1F);
    }

    #[test]
    fn flash128_id_is_sanyo() {
        let mut flash = FlashChip::new(128 * 1024);
        unlock(&mut flash);
        flash.write(0x5555, 0x90);
        assert_eq!(flash.read(0), 0x62);
        assert_eq!(flash.read(1), 0x13);
        flash.write(0, 0xF0);
        assert_eq!(flash.read(0), 0xFF);
    }

    #[test]
    fn amd_sector_erase_after_second_unlock() {
        let mut flash = FlashChip::new(128 * 1024);
        flash.data[0x1000] = 0x12;
        flash.data[0x1FFF] = 0x34;
        flash.data[0x2000] = 0x56;
        sector_erase(&mut flash, 0x1000);
        assert_eq!(flash.read(0x1000), 0xFF, "sector start erased");
        assert_eq!(flash.read(0x1FFF), 0xFF, "sector end erased");
        assert_eq!(flash.data[0x2000], 0x56, "next sector untouched");
    }

    #[test]
    fn amd_chip_erase() {
        let mut flash = FlashChip::new(64 * 1024);
        flash.data[0] = 0x11;
        flash.data[0x8000] = 0x22;
        unlock(&mut flash);
        flash.write(0x5555, 0x80);
        unlock(&mut flash);
        flash.write(0x5555, 0x10);
        assert!(flash.data.iter().all(|&b| b == 0xFF));
    }

    #[test]
    fn program_after_erase_and_no_raw_write() {
        let mut flash = FlashChip::new(128 * 1024);
        sector_erase(&mut flash, 0);
        program(&mut flash, 0x20, 0xA5);
        assert_eq!(flash.read(0x20), 0xA5);
        // Command bytes must not poke SRAM-style into the array.
        flash.write(0x20, 0x00);
        assert_eq!(flash.read(0x20), 0xA5);
    }

    #[test]
    fn bank_switch_selects_second_64k() {
        let mut flash = FlashChip::new(128 * 1024);
        flash.data[64 * 1024] = 0x77;
        unlock(&mut flash);
        flash.write(0x5555, 0xB0);
        flash.write(0, 1);
        assert_eq!(flash.read(0), 0x77);
        unlock(&mut flash);
        flash.write(0x5555, 0xB0);
        flash.write(0, 0);
        assert_eq!(flash.read(0), 0xFF);
    }
}


