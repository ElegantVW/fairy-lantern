//! Cartridge GPIO real-time clock (SIIRTC / Pokemon-style).
//!
//! Liquid Crystal and other gen-2 ports use `RTC_V` in the ROM header area and
//! bit-bang a Seiko Instruments RTC via GPIO at 0x080000C4–C8. We expose host
//! local time so in-game clocks and day/night advance correctly.

use std::time::{SystemTime, UNIX_EPOCH};

/// GPIO registers (relative to 0x0800_0000 cart space — full addr 0x080000C4).
pub const GPIO_DATA: u32 = 0x0800_00C4;
pub const GPIO_DIR: u32 = 0x0800_00C6;
pub const GPIO_CTRL: u32 = 0x0800_00C8;

#[derive(Clone, Debug)]
pub struct Rtc {
    pub present: bool,
    /// GPIO data latch (bits: 0=data, 1=clock, 2=select — layout varies; we use
    /// common mGBA/Pokemon mapping: bit0=SCK, bit1=SIO, bit2=CS).
    data: u16,
    dir: u16,
    ctrl: u16,
    /// Serial transfer state
    bit_count: u8,
    cmd: u8,
    /// Buffer for 7 datetime bytes or status
    buf: [u8; 8],
    buf_len: u8,
    buf_idx: u8,
    reading: bool,
    /// Last CS level
    cs: bool,
    sck: bool,
    /// Command phase complete
    cmd_done: bool,
}

impl Default for Rtc {
    fn default() -> Self {
        Self::new(false)
    }
}

impl Rtc {
    pub fn new(present: bool) -> Self {
        Self {
            present,
            data: 0,
            dir: 0,
            ctrl: 0,
            bit_count: 0,
            cmd: 0,
            buf: [0; 8],
            buf_len: 0,
            buf_idx: 0,
            reading: false,
            cs: false,
            sck: false,
            cmd_done: false,
        }
    }

    pub fn detect(rom: &[u8]) -> bool {
        let s = String::from_utf8_lossy(rom);
        s.contains("SIIRTC_V")
            || s.contains("RTC_V")
            || s.contains("IRTC_V")
            || s.contains("SIIRTC")
    }

    pub fn read16(&self, addr: u32) -> Option<u16> {
        if !self.present {
            return None;
        }
        match addr & !1 {
            GPIO_DATA => {
                // When SIO is input, reflect serial out bit in bit1
                let mut v = self.data;
                if self.dir & 0x2 == 0 {
                    // SIO as input — provide next bit if reading
                    let bit = self.serial_out_bit();
                    if bit {
                        v |= 0x2;
                    } else {
                        v &= !0x2;
                    }
                }
                Some(v | 0x4) // keep CS readable
            }
            GPIO_DIR => Some(self.dir),
            GPIO_CTRL => Some(self.ctrl | 1), // RTC present flag often bit0
            _ => None,
        }
    }

    pub fn write16(&mut self, addr: u32, val: u16) {
        if !self.present {
            return;
        }
        match addr & !1 {
            GPIO_DATA => {
                let old = self.data;
                self.data = val;
                self.on_data_write(old, val);
            }
            GPIO_DIR => self.dir = val,
            GPIO_CTRL => self.ctrl = val,
            _ => {}
        }
    }

    fn on_data_write(&mut self, old: u16, new: u16) {
        let cs = new & 0x4 != 0;
        let sck = new & 0x1 != 0;
        let sio = new & 0x2 != 0;

        // CS rising edge: start transaction
        if cs && !self.cs {
            self.bit_count = 0;
            self.cmd = 0;
            self.cmd_done = false;
            self.reading = false;
            self.buf_idx = 0;
            self.buf_len = 0;
        }
        // CS falling: end
        if !cs && self.cs {
            self.bit_count = 0;
            self.cmd_done = false;
        }
        self.cs = cs;

        if !cs {
            self.sck = sck;
            return;
        }

        // Clock rising edge: sample SIO for command/data in
        if sck && !self.sck {
            if !self.cmd_done {
                // Command byte, LSB first
                if sio {
                    self.cmd |= 1 << self.bit_count;
                }
                self.bit_count += 1;
                if self.bit_count >= 8 {
                    self.bit_count = 0;
                    self.cmd_done = true;
                    self.begin_command(self.cmd);
                }
            } else if !self.reading {
                // Write data bits into buffer
                if self.buf_idx < self.buf_len {
                    let byte_i = self.buf_idx as usize;
                    let bit_i = self.bit_count;
                    if sio {
                        self.buf[byte_i] |= 1 << bit_i;
                    }
                    self.bit_count += 1;
                    if self.bit_count >= 8 {
                        self.bit_count = 0;
                        self.buf_idx += 1;
                    }
                }
            } else {
                // Reading: advance bit on clock (SO presented while SCK high/low)
                self.bit_count += 1;
                if self.bit_count >= 8 {
                    self.bit_count = 0;
                    self.buf_idx += 1;
                }
            }
        }
        self.sck = sck;
        let _ = old;
    }

    fn begin_command(&mut self, cmd: u8) {
        // SII commands (Pokemon):
        // 0x60 status read, 0x62 datetime read, 0x64 datetime write, 0x66 time write…
        match cmd & 0xF0 {
            0x60 => {
                // status / datetime read family
                match cmd {
                    0x60 => {
                        // status: 1 byte
                        self.reading = true;
                        self.buf[0] = 0x40; // power-ok-ish
                        self.buf_len = 1;
                        self.buf_idx = 0;
                        self.bit_count = 0;
                    }
                    0x62 | 0x66 => {
                        // 7-byte BCD datetime: year mon day week hour min sec
                        self.reading = true;
                        self.fill_datetime_bcd();
                        self.buf_len = 7;
                        self.buf_idx = 0;
                        self.bit_count = 0;
                    }
                    _ => {
                        self.reading = true;
                        self.fill_datetime_bcd();
                        self.buf_len = 7;
                        self.buf_idx = 0;
                        self.bit_count = 0;
                    }
                }
            }
            0x20 | 0x00 => {
                // write family — accept bits but ignore for host-clock authority
                self.reading = false;
                self.buf = [0; 8];
                self.buf_len = 7;
                self.buf_idx = 0;
                self.bit_count = 0;
            }
            _ => {
                // default: respond with datetime
                self.reading = true;
                self.fill_datetime_bcd();
                self.buf_len = 7;
                self.buf_idx = 0;
                self.bit_count = 0;
            }
        }
    }

    fn serial_out_bit(&self) -> bool {
        if !self.reading || self.buf_idx >= self.buf_len {
            return false;
        }
        let b = self.buf[self.buf_idx as usize];
        (b >> (self.bit_count.min(7))) & 1 != 0
    }

    fn fill_datetime_bcd(&mut self) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        // UTC broken-down without external crate
        let (y, mo, d, wd, h, mi, s) = unix_to_civil(now);
        self.buf[0] = bin_to_bcd((y % 100) as u8);
        self.buf[1] = bin_to_bcd(mo);
        self.buf[2] = bin_to_bcd(d);
        self.buf[3] = bin_to_bcd(wd);
        self.buf[4] = bin_to_bcd(h);
        self.buf[5] = bin_to_bcd(mi);
        self.buf[6] = bin_to_bcd(s);
    }

    /// Human-readable clock string for window title.
    pub fn clock_string(&self) -> String {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let (y, mo, d, _, h, mi, s) = unix_to_civil(now);
        if self.present {
            format!("RTC {y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
        } else {
            format!("clk {h:02}:{mi:02}:{s:02}")
        }
    }
}

fn bin_to_bcd(v: u8) -> u8 {
    let v = v.min(99);
    ((v / 10) << 4) | (v % 10)
}

/// Civil date from Unix seconds (UTC). Good enough for in-game clocks.
fn unix_to_civil(mut secs: u64) -> (u32, u8, u8, u8, u8, u8, u8) {
    let s = (secs % 60) as u8;
    secs /= 60;
    let mi = (secs % 60) as u8;
    secs /= 60;
    let h = (secs % 24) as u8;
    secs /= 24;
    // days since 1970-01-01 (Thursday = 4)
    let mut days = secs;
    let wd = ((days + 4) % 7) as u8; // 0=Sun … match some Pokemon (0=Sun)
    let mut y = 1970u32;
    loop {
        let diy = if is_leap(y) { 366 } else { 365 };
        if days < diy {
            break;
        }
        days -= diy;
        y += 1;
    }
    let months: [u32; 12] = if is_leap(y) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut mo = 1u8;
    for &md in &months {
        if days < md as u64 {
            break;
        }
        days -= md as u64;
        mo += 1;
    }
    let d = (days + 1) as u8;
    (y, mo, d, wd, h, mi, s)
}

fn is_leap(y: u32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0)
}
