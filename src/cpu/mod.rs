//! ARM7TDMI interpreter (pipeline-less, from scratch).

mod arm;
mod cpsr;
mod thumb;

pub use cpsr::Cpsr;

use crate::bus::Bus;
use std::sync::OnceLock;

/// Cached FAIRY_DMA_TRACE flag — env lookup is too costly on the per-instruction path.
pub static FAIRY_TRACE_ONCE: OnceLock<bool> = OnceLock::new();
#[inline]
pub fn fairy_trace() -> bool {
    *FAIRY_TRACE_ONCE.get_or_init(|| std::env::var_os("FAIRY_DMA_TRACE").is_some())
}

/// HLE BIOS address: IRQ epilogue (`ldmfd …; subs pc, lr, #4`).
pub const BIOS_IRQ_RETURN: u32 = 0x0000_0138;

#[derive(Clone, Debug)]
pub struct Cpu {
    /// R0–R15; R15 is PC (points at *current* instruction for our interpreter).
    pub r: [u32; 16],
    pub cpsr: Cpsr,
    /// Active SPSR for the current privileged mode.
    pub spsr: Cpsr,
    pub cycles: u64,
    pub halted: bool,
    /// Count of undecoded instructions (soft NOPs).
    pub unknown_ops: u64,
    /// Last unknown opcode (for diagnose).
    pub last_unknown: u32,

    // Banked R13/R14 + SPSR (IRQ / SVC / USR-SYS)
    pub r13_usr: u32,
    pub r14_usr: u32,
    pub r13_irq: u32,
    pub r14_irq: u32,
    pub spsr_irq: Cpsr,
    pub r13_svc: u32,
    pub r14_svc: u32,
    pub spsr_svc: Cpsr,
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Cpu {
    pub fn new() -> Self {
        Self {
            r: [0; 16],
            cpsr: Cpsr::new_svc(),
            spsr: Cpsr::default(),
            cycles: 0,
            halted: false,
            unknown_ops: 0,
            last_unknown: 0,
            r13_usr: 0x0300_7F00,
            r14_usr: 0,
            // BIOS defaults
            r13_irq: 0x0300_7FA0,
            r14_irq: 0,
            spsr_irq: Cpsr::default(),
            r13_svc: 0x0300_7FE0,
            r14_svc: 0,
            spsr_svc: Cpsr::default(),
        }
    }

    pub fn pc(&self) -> u32 {
        self.r[15]
    }

    pub fn set_pc(&mut self, pc: u32) {
        self.r[15] = pc;
    }

    /// Architectural PC+8 for ARM Rn/Rd-as-PC and LDR/STR base.
    ///
    /// After fetch R15 is already A+4; PC+8 = R15+4.
    /// For Rm=PC in the barrel shifter, callers add another +4 (PC+12).
    pub fn pc_arm_read(&self) -> u32 {
        self.r[15].wrapping_add(4)
    }

    /// Architectural PC+4 for Thumb PC-relative ops.
    /// After fetch R15 is A+2; PC+4 = R15+2.
    pub fn pc_thumb_read(&self) -> u32 {
        self.r[15].wrapping_add(2)
    }

    /// Switch CPSR mode, banking R13/R14/SPSR as needed.
    pub fn set_mode(&mut self, new_mode: u8) {
        let new_mode = new_mode & 0x1F;
        let old = self.cpsr.mode & 0x1F;
        if old == new_mode {
            return;
        }
        self.bank_save(old);
        self.bank_restore(new_mode);
        self.cpsr.mode = new_mode;
    }

    fn bank_slot(mode: u8) -> u8 {
        match mode & 0x1F {
            0x12 => 1, // IRQ
            0x13 => 2, // SVC
            _ => 0,    // USR / SYS / others
        }
    }

    fn bank_save(&mut self, mode: u8) {
        match Self::bank_slot(mode) {
            1 => {
                self.r13_irq = self.r[13];
                self.r14_irq = self.r[14];
                self.spsr_irq = self.spsr;
            }
            2 => {
                self.r13_svc = self.r[13];
                self.r14_svc = self.r[14];
                self.spsr_svc = self.spsr;
            }
            _ => {
                self.r13_usr = self.r[13];
                self.r14_usr = self.r[14];
            }
        }
    }

    fn bank_restore(&mut self, mode: u8) {
        match Self::bank_slot(mode) {
            1 => {
                self.r[13] = self.r13_irq;
                self.r[14] = self.r14_irq;
                self.spsr = self.spsr_irq;
            }
            2 => {
                self.r[13] = self.r13_svc;
                self.r[14] = self.r14_svc;
                self.spsr = self.spsr_svc;
            }
            _ => {
                self.r[13] = self.r13_usr;
                self.r[14] = self.r14_usr;
            }
        }
    }

    /// Restore CPSR from SPSR (for `SUBS pc, lr, #4` etc.) and unbank.
    pub fn restore_spsr(&mut self) {
        let spsr = self.spsr;
        let old = self.cpsr.mode & 0x1F;
        let new = spsr.mode & 0x1F;
        if old != new {
            self.bank_save(old);
            self.bank_restore(new);
        }
        self.cpsr = spsr;
    }

    pub fn step(&mut self, bus: &mut Bus) -> u32 {
        if self.halted {
            return 1;
        }
        // HLE BIOS IRQ epilogue
        if !self.cpsr.thumb && self.r[15] == BIOS_IRQ_RETURN {
            return crate::irq::hle_irq_return(self, bus);
        }
        let base = if self.cpsr.thumb {
            thumb::step(self, bus)
        } else {
            arm::step(self, bus)
        };
        if fairy_trace() && bus.dbg_evt < 12000 && bus.dbg_evt % 28 == 0 {
            static LFE: AtomicU64 = AtomicU64::new(0);
            use std::sync::atomic::{AtomicU64, Ordering};
            let le = LFE.load(Ordering::Relaxed);
            if bus.dbg_evt != le {
                LFE.store(bus.dbg_evt, Ordering::Relaxed);
                eprintln!(
                    "FRAME evt{} pc={:08X} mode={} sp={:08X}",
                    bus.dbg_evt,
                    self.r[15],
                    if self.cpsr.thumb { 't' } else { 'a' },
                    self.r[13]
                );
            }
        }
        if fairy_trace() {
            bus.dbg_pc = self.r[15];
            if self.r[15] == 0x03002BD4 || self.r[15] == 0x03002BD5 {
                static DE: AtomicU64 = AtomicU64::new(0);
                use std::sync::atomic::{AtomicU64, Ordering};
                let h = DE.fetch_add(1, Ordering::Relaxed);
                if h < 10 {
                    eprintln!(
                        "DEC lr={:08X} r2={:08X} r3={:08X} r4={:08X} r5={:08X} r8={:08X} sb={:08X} evt{}",
                        self.r[14], self.r[2], self.r[3], self.r[4], self.r[5], self.r[8], self.r[9],
                        bus.dbg_evt
                    );
                }
            }
            if bus.dbg_evt >= 8380 && bus.dbg_evt < 8780 {
                use std::sync::atomic::{AtomicU64, AtomicU32, Ordering};
                static LPC: AtomicU32 = AtomicU32::new(0);
                static RPC: AtomicU32 = AtomicU32::new(0);
                static CNT: AtomicU64 = AtomicU64::new(0);
                let pc = self.r[15];
                let lp = LPC.load(Ordering::Relaxed);
                if lp != pc {
                    let rp = RPC.swap(pc, Ordering::Relaxed);
                    let c = CNT.swap(0, Ordering::Relaxed);
                    if rp != 0 && c > 0 {
                        eprintln!(
                            "RUN pc={:08X} n={} evt{}",
                            rp, c, bus.dbg_evt
                        );
                    }
                    LPC.store(pc, Ordering::Relaxed);
                }
                CNT.fetch_add(1, Ordering::Relaxed);
            }
            use std::sync::atomic::{AtomicU64, Ordering};
            const DRV: [u32; 9] = [
                0x081DC9D4, 0x081DD728, 0x081DD7A4, 0x081DD7E0, 0x081DCA20, 0x081DD858, 0x081DD93C,
                0x081DD97C, 0x081DD000,
            ];
            for (i, p) in DRV.iter().enumerate() {
                if self.r[15] == *p {
                    static HITS: [AtomicU64; 9] = [const { AtomicU64::new(0) }; 9];
                    let h = HITS[i].fetch_add(1, Ordering::Relaxed);
                    if h < 6 || h % 100 == 0 {
                        eprintln!("DRV pc={:08X} lr={:08X} evt{}", p, self.r[14], bus.dbg_evt);
                    }
                }
            }
            if self.r[15] == 0x081DD640 {
                static CMD: AtomicU64 = AtomicU64::new(0);
                let h = CMD.fetch_add(1, Ordering::Relaxed);
                if h < 20 || h % 100 == 0 {
                    let pos = bus.read32(0x0300_5F50);
                    let c8 = bus.read8(0x0300_5F58);
                    let s24 = bus.read32(0x0300_5F74);
                    eprintln!(
                        "CMD pc evt{} lr={:08X} r3={:08X} pos={:08X} c8={:02X} s24={:08X}",
                        bus.dbg_evt, self.r[14], self.r[3], pos, c8, s24
                    );
                }
            }
            if self.r[15] == 0x081DE638 || self.r[15] == 0x081DE6DE {
                static ST: AtomicU64 = AtomicU64::new(0);
                let h = ST.fetch_add(1, Ordering::Relaxed);
                if h < 8 || h % 100 == 0 {
                    let s28 = bus.read32(0x0300_5F78);
                    let s = bus.read32(0x0300_5F50);
                    eprintln!(
                        "SST pc={:08X} lr={:08X} evt{} s28={:08X} pos={:08X}",
                        self.r[15], self.r[14], bus.dbg_evt, s28, s
                    );
                }
            }
            if self.r[15] == 0x081DC088 {
                static DI: AtomicU64 = AtomicU64::new(0);
                let h = DI.fetch_add(1, Ordering::Relaxed);
                if h % 200 == 0 {
                    eprintln!(
                        "WINDOW evt{} w={:08X} s0={:08X} s1={:08X} s2={:08X} s3={:08X} s4={:08X} s5={:08X} s6={:08X} s7={:08X} s8={:08X} s9={:08X}",
                        bus.dbg_evt,
                        self.r[5],
                        bus.read32(0x0300_62A0),
                        bus.read32(0x0300_62A8),
                        bus.read32(0x0300_62B0),
                        bus.read32(0x0300_62B8),
                        bus.read32(0x0300_62C0),
                        bus.read32(0x0300_62C8),
                        bus.read32(0x0300_62D0),
                        bus.read32(0x0300_62D8),
                        bus.read32(0x0300_68D0),
                        bus.read32(0x0300_68D8),
                    );
                    eprintln!(
                        "TRACK evt{} tp0={:08X} tp1={:08X} tp2={:08X} tp3={:08X} tp4={:08X} tp5={:08X}",
                        bus.dbg_evt,
                        bus.read32(0x0300_7220),
                        bus.read32(0x0300_7228),
                        bus.read32(0x0300_7230),
                        bus.read32(0x0300_7238),
                        bus.read32(0x0300_7240),
                        bus.read32(0x0300_7248),
                    );
                    eprintln!(
                        "MIXTRK evt{} a0={:08X} a1={:08X} a2={:08X} a3={:08X} a4={:08X} a5={:08X} b0={:08X} b1={:08X} c0={:08X} c1={:08X}",
                        bus.dbg_evt,
                        bus.read32(0x0300_5FA0),
                        bus.read32(0x0300_5FA8),
                        bus.read32(0x0300_5FB0),
                        bus.read32(0x0300_5FF0),
                        bus.read32(0x0300_6040),
                        bus.read32(0x0300_6090),
                        bus.read32(0x0300_60E0),
                        bus.read32(0x0300_6130),
                        bus.read32(0x0300_6180),
                        bus.read32(0x0300_61D0),
                    );
                }
            }
            if self.r[15] == 0x081DC9D4 {
                static RA: AtomicU64 = AtomicU64::new(0);
                let h = RA.fetch_add(1, Ordering::Relaxed);
                if h < 4 || h % 700 == 0 {
                    let s = bus.read8(0x0300_5F50);
                    let pos = bus.read32(0x0300_5F50);
                    let cnt = bus.read8(0x0300_5F54);
                    let rel = bus.read8(0x0300_5F5B);
                    let song = bus.read16(0x0300_5F74);
                    let st = bus.read32(0x0300_5F84);
                    let t0 = bus.read16(0x0400_0104);
                    eprintln!(
                        "REARM h={h} evt{} lr={:08X} pos={:08X} c={} rel={} song={:04X} st={:08X} tm0r={:04X}",
                        bus.dbg_evt, self.r[14], pos, cnt, rel, song, st, t0
                    );
                }
            }
            if self.r[15] == 0x081DCA20 {
                static SP: AtomicU64 = AtomicU64::new(0);
                static SPC: AtomicU64 = AtomicU64::new(0);
                let h = SP.fetch_add(1, Ordering::Relaxed);
                let c = SPC.fetch_add(1, Ordering::Relaxed);
                if h < 6 || c % 1000 == 0 {
                    let g = bus.read32(self.r[0].wrapping_add(0x34));
                    let song = bus.read32(0x0300_5F74);
                    let pos = bus.read32(0x0300_5F50);
                    let f4 = bus.read32(self.r[0].wrapping_add(4));
                    let tp = bus.read32(self.r[0].wrapping_add(0x2c));
                    let t0 = if tp != 0 { bus.read32(tp) } else { 0 };
                    let t1 = if tp != 0 { bus.read32(tp.wrapping_add(8)) } else { 0 };
                    let t2 = if tp != 0 { bus.read32(tp.wrapping_add(0x20)) } else { 0 };
                    eprintln!(
                        "SPROC h={h} c={c} evt{} r0={:08X} gate={:08X} f4={:08X} tp={:08X} t=[{:08X} {:08X} {:08X}] song={:08X} pos={:08X} lr={:08X}",
                        bus.dbg_evt, self.r[0], g, f4, tp, t0, t1, t2, song, pos, self.r[14]
                    );
                }
            }
            if self.r[15] == 0x081DC088 {
                static DISP: AtomicU64 = AtomicU64::new(0);
                let d = DISP.fetch_add(1, Ordering::Relaxed);
                if d < 8 {
                    let mut code = [0u8; 16];
                    for k in 0..16 {
                        code[k] = bus.read8(0x0300_28E0 + k as u32);
                    }
                    eprintln!(
                        "DISP evt{} r0={:08X} r5={:08X} r6={:08X} r3={:08X} code={:02X}{:02X}{:02X}{:02X} {:02X}{:02X}{:02X}{:02X}",
                        bus.dbg_evt,
                        self.r[0],
                        self.r[5],
                        self.r[6],
                        self.r[3],
                        code[0],
                        code[1],
                        code[2],
                        code[3],
                        code[4],
                        code[5],
                        code[6],
                        code[7]
                    );
                }
            }
        }
        // Region waitstates on the fetch (PC still points near the insn)
        let wait = bus.fetch_waitstates(self.r[15].wrapping_sub(if self.cpsr.thumb {
            2
        } else {
            4
        }));
        let total = base.saturating_add(wait);
        // arm/thumb already added `base` to cpu.cycles — add wait remainder
        self.cycles = self.cycles.saturating_add(wait as u64);
        total
    }

    pub fn reg(&self, i: usize) -> u32 {
        if i == 15 {
            if self.cpsr.thumb {
                self.pc_thumb_read()
            } else {
                self.pc_arm_read()
            }
        } else {
            self.r[i]
        }
    }

    pub fn set_reg(&mut self, i: usize, v: u32) {
        if i == 15 {
            // writing PC
            let mut pc = v;
            if self.cpsr.thumb {
                pc &= !1;
                // LSB of BX sets thumb; bare MOV PC keeps mode
            } else {
                pc &= !3;
            }
            self.r[15] = pc;
        } else {
            self.r[i] = v;
        }
    }
}
