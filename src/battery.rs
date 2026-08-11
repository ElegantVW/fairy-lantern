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
    // Default: many homebrew / unknown — give SRAM so casual saves can work
    SaveType::Sram(64 * 1024)
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
        let (m, d) = if size >= 128 * 1024 {
            (0x62, 0x13) // Sanyo 128K-ish id used by some emus
        } else {
            (0xBF, 0xD4) // SST / Panasonic style 64K
        };
        Self {
            data: vec![0xFF; size.max(64 * 1024)],
            bank: 0,
            cmd_step: 0,
            mode: 0,
            manufacturer: m,
            device: d,
        }
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
        // Bank switch (128K): write bank to 0x0E000000 after unlock sequence varies;
        // common: after 0xAA/0x55, command 0xB0 then write bank at 0x0000
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
                match val {
                    0x90 => {
                        self.mode = 1; // ID
                        return false;
                    }
                    0xF0 => {
                        self.mode = 0;
                        return false;
                    }
                    0x80 => {
                        self.mode = 2; // erase prep
                        return false;
                    }
                    0xA0 => {
                        self.mode = 3; // program next byte
                        return false;
                    }
                    0xB0 => {
                        self.mode = 4; // bank
                        return false;
                    }
                    _ => return false,
                }
            }
            _ => {}
        }

        if self.mode == 4 {
            // bank select
            self.bank = (val & 1) as usize;
            self.mode = 0;
            return false;
        }
        if self.mode == 3 {
            let i = self.bank_base() + off;
            if i < self.data.len() {
                self.data[i] &= val; // flash programs 1→0
            }
            self.mode = 0;
            return true;
        }
        if self.mode == 2 {
            // erase commands: 0x30 sector, 0x10 chip after second unlock
            if off == 0x5555 && val == 0xAA {
                self.cmd_step = 1;
                return false;
            }
            // simplify: any 0x30/0x10 erases whole chip or sector
            if val == 0x10 || val == 0x30 {
                if val == 0x10 {
                    self.data.fill(0xFF);
                } else {
                    let base = self.bank_base() + (off & !0xFFF);
                    for b in self.data.iter_mut().skip(base).take(0x1000) {
                        *b = 0xFF;
                    }
                }
                self.mode = 0;
                return true;
            }
        }
        // raw write fallback (some homebrew)
        if self.mode == 0 && self.cmd_step == 0 {
            let i = self.bank_base() + off;
            if i < self.data.len() {
                self.data[i] = val;
                return true;
            }
        }
        false
    }
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


