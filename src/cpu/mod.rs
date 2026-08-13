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

/// When false (`FAIRY_ACCURATE_AFFINE=1`), skip Liquid Crystal affine identity/PD hacks.
pub fn affine_compat() -> bool {
    static ONCE: OnceLock<bool> = OnceLock::new();
    *ONCE.get_or_init(|| std::env::var_os("FAIRY_ACCURATE_AFFINE").is_none())
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

    // Banked registers (ARMv4T)
    pub r8_usr: [u32; 5], // R8–R12 (also used by IRQ/SVC/ABT/UND/SYS)
    pub r13_usr: u32,
    pub r14_usr: u32,
    pub r8_fiq: [u32; 5],
    pub r13_fiq: u32,
    pub r14_fiq: u32,
    pub spsr_fiq: Cpsr,
    pub r13_irq: u32,
    pub r14_irq: u32,
    pub spsr_irq: Cpsr,
    pub r13_svc: u32,
    pub r14_svc: u32,
    pub spsr_svc: Cpsr,
    pub r13_abt: u32,
    pub r14_abt: u32,
    pub spsr_abt: Cpsr,
    pub r13_und: u32,
    pub r14_und: u32,
    pub spsr_und: Cpsr,
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
            r8_usr: [0; 5],
            r13_usr: 0x0300_7F00,
            r14_usr: 0,
            r8_fiq: [0; 5],
            r13_fiq: 0x0300_7F80,
            r14_fiq: 0,
            spsr_fiq: Cpsr::default(),
            r13_irq: 0x0300_7FA0,
            r14_irq: 0,
            spsr_irq: Cpsr::default(),
            r13_svc: 0x0300_7FE0,
            r14_svc: 0,
            spsr_svc: Cpsr::default(),
            r13_abt: 0x0300_7F00,
            r14_abt: 0,
            spsr_abt: Cpsr::default(),
            r13_und: 0x0300_7F00,
            r14_und: 0,
            spsr_und: Cpsr::default(),
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

    fn bank_save(&mut self, mode: u8) {
        match mode & 0x1F {
            0x11 => {
                self.r8_fiq.copy_from_slice(&self.r[8..13]);
                self.r13_fiq = self.r[13];
                self.r14_fiq = self.r[14];
                self.spsr_fiq = self.spsr;
            }
            0x12 => {
                self.r8_usr.copy_from_slice(&self.r[8..13]);
                self.r13_irq = self.r[13];
                self.r14_irq = self.r[14];
                self.spsr_irq = self.spsr;
            }
            0x13 => {
                self.r8_usr.copy_from_slice(&self.r[8..13]);
                self.r13_svc = self.r[13];
                self.r14_svc = self.r[14];
                self.spsr_svc = self.spsr;
            }
            0x17 => {
                self.r8_usr.copy_from_slice(&self.r[8..13]);
                self.r13_abt = self.r[13];
                self.r14_abt = self.r[14];
                self.spsr_abt = self.spsr;
            }
            0x1B => {
                self.r8_usr.copy_from_slice(&self.r[8..13]);
                self.r13_und = self.r[13];
                self.r14_und = self.r[14];
                self.spsr_und = self.spsr;
            }
            _ => {
                self.r8_usr.copy_from_slice(&self.r[8..13]);
                self.r13_usr = self.r[13];
                self.r14_usr = self.r[14];
            }
        }
    }

    fn bank_restore(&mut self, mode: u8) {
        match mode & 0x1F {
            0x11 => {
                self.r[8..13].copy_from_slice(&self.r8_fiq);
                self.r[13] = self.r13_fiq;
                self.r[14] = self.r14_fiq;
                self.spsr = self.spsr_fiq;
            }
            0x12 => {
                self.r[8..13].copy_from_slice(&self.r8_usr);
                self.r[13] = self.r13_irq;
                self.r[14] = self.r14_irq;
                self.spsr = self.spsr_irq;
            }
            0x13 => {
                self.r[8..13].copy_from_slice(&self.r8_usr);
                self.r[13] = self.r13_svc;
                self.r[14] = self.r14_svc;
                self.spsr = self.spsr_svc;
            }
            0x17 => {
                self.r[8..13].copy_from_slice(&self.r8_usr);
                self.r[13] = self.r13_abt;
                self.r[14] = self.r14_abt;
                self.spsr = self.spsr_abt;
            }
            0x1B => {
                self.r[8..13].copy_from_slice(&self.r8_usr);
                self.r[13] = self.r13_und;
                self.r[14] = self.r14_und;
                self.spsr = self.spsr_und;
            }
            _ => {
                self.r[8..13].copy_from_slice(&self.r8_usr);
                self.r[13] = self.r13_usr;
                self.r[14] = self.r14_usr;
            }
        }
    }

    /// User-bank R8–R14 (for `LDM/STM ^` without PC).
    pub fn user_reg(&self, i: usize) -> u32 {
        let mode = self.cpsr.mode & 0x1F;
        match i {
            8..=12 => {
                if mode == 0x11 {
                    self.r8_usr[i - 8]
                } else {
                    self.r[i]
                }
            }
            13 => {
                if mode == 0x10 || mode == 0x1F {
                    self.r[13]
                } else {
                    self.r13_usr
                }
            }
            14 => {
                if mode == 0x10 || mode == 0x1F {
                    self.r[14]
                } else {
                    self.r14_usr
                }
            }
            _ => self.r[i],
        }
    }

    pub fn set_user_reg(&mut self, i: usize, v: u32) {
        let mode = self.cpsr.mode & 0x1F;
        match i {
            8..=12 => {
                if mode == 0x11 {
                    self.r8_usr[i - 8] = v;
                } else {
                    self.r[i] = v;
                    self.r8_usr[i - 8] = v;
                }
            }
            13 => {
                if mode == 0x10 || mode == 0x1F {
                    self.r[13] = v;
                }
                self.r13_usr = v;
            }
            14 => {
                if mode == 0x10 || mode == 0x1F {
                    self.r[14] = v;
                }
                self.r14_usr = v;
            }
            _ => self.r[i] = v,
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

    pub fn note_unknown(&mut self, op: u32, thumb: bool) {
        self.unknown_ops = self.unknown_ops.wrapping_add(1);
        self.last_unknown = op;
        if self.unknown_ops <= 8 {
            let pc = if thumb {
                self.r[15].wrapping_sub(2)
            } else {
                self.r[15].wrapping_sub(4)
            };
            eprintln!(
                "  unk_op #{} pc={pc:08X} op={op:08X} {}",
                self.unknown_ops,
                if thumb { "thumb" } else { "arm" }
            );
        }
    }

    pub fn step(&mut self, bus: &mut Bus) -> u32 {
        if self.halted {
            return 1;
        }
        bus.exec_pc = self.r[15];
        #[cfg(debug_assertions)]
        {
            bus.dbg_pc = self.r[15];
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
        #[cfg(debug_assertions)]
        debug_trace_after_step(self, bus);
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

#[cfg(debug_assertions)]
fn debug_trace_after_step(cpu: &Cpu, bus: &mut Bus) {
        if !fairy_trace() {
            return;
        }
        if bus.dbg_evt < 12000 && bus.dbg_evt % 28 == 0 {
            static LFE: AtomicU64 = AtomicU64::new(0);
            use std::sync::atomic::{AtomicU64, Ordering};
            let le = LFE.load(Ordering::Relaxed);
            if bus.dbg_evt != le {
                LFE.store(bus.dbg_evt, Ordering::Relaxed);
                eprintln!(
                    "FRAME evt{} pc={:08X} mode={} sp={:08X}",
                    bus.dbg_evt,
                    cpu.r[15],
                    if cpu.cpsr.thumb { 't' } else { 'a' },
                    cpu.r[13]
                );
            }
        }
        {
            bus.dbg_pc = cpu.r[15];
            if cpu.r[15] == 0x03002BD4 || cpu.r[15] == 0x03002BD5 {
                static DE: AtomicU64 = AtomicU64::new(0);
                use std::sync::atomic::{AtomicU64, Ordering};
                let h = DE.fetch_add(1, Ordering::Relaxed);
                if h < 10 {
                    eprintln!(
                        "DEC lr={:08X} r2={:08X} r3={:08X} r4={:08X} r5={:08X} r8={:08X} sb={:08X} evt{}",
                        cpu.r[14], cpu.r[2], cpu.r[3], cpu.r[4], cpu.r[5], cpu.r[8], cpu.r[9],
                        bus.dbg_evt
                    );
                }
            }
            if bus.dbg_evt >= 8380 && bus.dbg_evt < 8780 {
                use std::sync::atomic::{AtomicU64, AtomicU32, Ordering};
                static LPC: AtomicU32 = AtomicU32::new(0);
                static RPC: AtomicU32 = AtomicU32::new(0);
                static CNT: AtomicU64 = AtomicU64::new(0);
                let pc = cpu.r[15];
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
                if cpu.r[15] == *p {
                    static HITS: [AtomicU64; 9] = [const { AtomicU64::new(0) }; 9];
                    let h = HITS[i].fetch_add(1, Ordering::Relaxed);
                    if h < 6 || h % 100 == 0 {
                        eprintln!("DRV pc={:08X} lr={:08X} evt{}", p, cpu.r[14], bus.dbg_evt);
                    }
                }
            }
            if cpu.r[15] == 0x081DD640 {
                static CMD: AtomicU64 = AtomicU64::new(0);
                let h = CMD.fetch_add(1, Ordering::Relaxed);
                if h < 20 || h % 100 == 0 {
                    let pos = bus.read32(0x0300_5F50);
                    let c8 = bus.read8(0x0300_5F58);
                    let s24 = bus.read32(0x0300_5F74);
                    eprintln!(
                        "CMD pc evt{} lr={:08X} r3={:08X} pos={:08X} c8={:02X} s24={:08X}",
                        bus.dbg_evt, cpu.r[14], cpu.r[3], pos, c8, s24
                    );
                }
            }
            if cpu.r[15] == 0x081DE638 || cpu.r[15] == 0x081DE6DE {
                static ST: AtomicU64 = AtomicU64::new(0);
                let h = ST.fetch_add(1, Ordering::Relaxed);
                if h < 8 || h % 100 == 0 {
                    let s28 = bus.read32(0x0300_5F78);
                    let s = bus.read32(0x0300_5F50);
                    eprintln!(
                        "SST pc={:08X} lr={:08X} evt{} s28={:08X} pos={:08X}",
                        cpu.r[15], cpu.r[14], bus.dbg_evt, s28, s
                    );
                }
            }
            if cpu.r[15] == 0x081DC088 {
                static DI: AtomicU64 = AtomicU64::new(0);
                let h = DI.fetch_add(1, Ordering::Relaxed);
                if h % 200 == 0 {
                    eprintln!(
                        "WINDOW evt{} w={:08X} s0={:08X} s1={:08X} s2={:08X} s3={:08X} s4={:08X} s5={:08X} s6={:08X} s7={:08X} s8={:08X} s9={:08X}",
                        bus.dbg_evt,
                        cpu.r[5],
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
            if cpu.r[15] == 0x081DC9D4 {
                static RA: AtomicU64 = AtomicU64::new(0);
                let h = RA.fetch_add(1, Ordering::Relaxed);
                if h < 4 || h % 700 == 0 {
                    let pos = bus.read32(0x0300_5F50);
                    let cnt = bus.read8(0x0300_5F54);
                    let rel = bus.read8(0x0300_5F5B);
                    let song = bus.read16(0x0300_5F74);
                    let st = bus.read32(0x0300_5F84);
                    let t0 = bus.read16(0x0400_0104);
                    eprintln!(
                        "REARM h={h} evt{} lr={:08X} pos={:08X} c={} rel={} song={:04X} st={:08X} tm0r={:04X}",
                        bus.dbg_evt, cpu.r[14], pos, cnt, rel, song, st, t0
                    );
                }
            }
            if cpu.r[15] == 0x081DCA20 {
                static SP: AtomicU64 = AtomicU64::new(0);
                static SPC: AtomicU64 = AtomicU64::new(0);
                let h = SP.fetch_add(1, Ordering::Relaxed);
                let c = SPC.fetch_add(1, Ordering::Relaxed);
                if h < 6 || c % 1000 == 0 {
                    let g = bus.read32(cpu.r[0].wrapping_add(0x34));
                    let song = bus.read32(0x0300_5F74);
                    let pos = bus.read32(0x0300_5F50);
                    let f4 = bus.read32(cpu.r[0].wrapping_add(4));
                    let tp = bus.read32(cpu.r[0].wrapping_add(0x2c));
                    let t0 = if tp != 0 { bus.read32(tp) } else { 0 };
                    let t1 = if tp != 0 { bus.read32(tp.wrapping_add(8)) } else { 0 };
                    let t2 = if tp != 0 { bus.read32(tp.wrapping_add(0x20)) } else { 0 };
                    eprintln!(
                        "SPROC h={h} c={c} evt{} r0={:08X} gate={:08X} f4={:08X} tp={:08X} t=[{:08X} {:08X} {:08X}] song={:08X} pos={:08X} lr={:08X}",
                        bus.dbg_evt, cpu.r[0], g, f4, tp, t0, t1, t2, song, pos, cpu.r[14]
                    );
                }
            }
            if cpu.r[15] == 0x081DC088 {
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
                        cpu.r[0],
                        cpu.r[5],
                        cpu.r[6],
                        cpu.r[3],
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::Bus;
    use crate::cart::Cart;

    fn cart_with(ops: &[u32]) -> Cart {
        let mut data = vec![0u8; 0x200];
        for (i, op) in ops.iter().enumerate() {
            data[i * 4..i * 4 + 4].copy_from_slice(&op.to_le_bytes());
        }
        Cart {
            data,
            title: "t".into(),
            game_code: "T".into(),
            maker: "00".into(),
            path: "m".into(),
            inner_name: None,
        }
    }

    #[test]
    fn msr_cpsr_does_not_set_thumb() {
        // AL MSR CPSR_c, #0x3F  — SYS + T. Hardware ignores T on MSR.
        let cart = cart_with(&[0xE321_F03F]);
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(&cart, None);
        cpu.set_mode(0x1F);
        cpu.cpsr.thumb = false;
        cpu.set_pc(0x0800_0000);
        cpu.step(&mut bus);
        assert!(!cpu.cpsr.thumb, "MSR must not enter Thumb");
        assert_eq!(cpu.cpsr.mode, 0x1F);
        assert_eq!(cpu.unknown_ops, 0);
    }

    #[test]
    fn usr_msr_cannot_change_mode() {
        // AL MSR CPSR_c, #0x13  from USR
        let cart = cart_with(&[0xE321_F013]);
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(&cart, None);
        cpu.set_mode(0x10);
        cpu.cpsr.irq_disable = false;
        cpu.set_pc(0x0800_0000);
        cpu.step(&mut bus);
        assert_eq!(cpu.cpsr.mode, 0x10, "USR cannot MSR the control field");
        assert!(!cpu.cpsr.irq_disable);
    }

    #[test]
    fn msr_imm_sets_sys_mode() {
        // AL MSR CPSR_c, #0x1F  (System, I/F clear)
        let cart = cart_with(&[0xE321_F01F]);
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(&cart, None);
        assert_eq!(cpu.cpsr.mode, 0x13, "reset is SVC");
        assert!(cpu.cpsr.irq_disable);
        cpu.set_pc(0x0800_0000);
        cpu.step(&mut bus);
        assert_eq!(cpu.cpsr.mode, 0x1F, "MSR #imm must change mode");
        assert!(!cpu.cpsr.irq_disable);
        assert!(!cpu.cpsr.fiq_disable);
        assert!(!cpu.cpsr.thumb);
        assert_eq!(cpu.unknown_ops, 0);
    }

    #[test]
    fn msr_imm_sets_irq_disable() {
        // AL MSR CPSR_c, #0xDF  (System + I + F)
        let cart = cart_with(&[0xE321_F0DF]);
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(&cart, None);
        cpu.set_mode(0x1F);
        cpu.cpsr.irq_disable = false;
        cpu.cpsr.fiq_disable = false;
        cpu.set_pc(0x0800_0000);
        cpu.step(&mut bus);
        assert_eq!(cpu.cpsr.mode, 0x1F);
        assert!(cpu.cpsr.irq_disable);
        assert!(cpu.cpsr.fiq_disable);
        assert_eq!(cpu.unknown_ops, 0);
    }

    #[test]
    fn strb_reg_offset_uses_rm_not_imm() {
        // STRB r0, [r5, r6]  — must write r5+r6, not r5+(r6 as rotated imm8).
        // m4a reverb is this encoding with r6 = PCM_DMA_BUF_SIZE (1584).
        let cart = cart_with(&[0xE7C5_0006]);
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(&cart, None);
        cpu.set_mode(0x1F);
        cpu.r[0] = 0xAB;
        cpu.r[5] = 0x0300_0000;
        cpu.r[6] = 1584;
        cpu.set_pc(0x0800_0000);
        cpu.step(&mut bus);
        assert_eq!(bus.read8(0x0300_0000 + 1584), 0xAB, "STRB [r5, r6] → r5+1584");
        assert_eq!(
            bus.read8(0x0300_0006),
            0,
            "must not treat Rm as an 8-bit immediate (r5+6)"
        );
        assert_eq!(cpu.unknown_ops, 0);
    }

    #[test]
    fn data_proc_reg_shift_costs_extra_cycle() {
        // MOV r0, r1, LSL r2  vs  MOV r0, r1
        let cart = cart_with(&[0xE1A0_0211, 0xE1A0_0001]);
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(&cart, None);
        cpu.set_mode(0x1F);
        cpu.r[1] = 4;
        cpu.r[2] = 1;
        cpu.set_pc(0x0300_0000);
        bus.write32(0x0300_0000, 0xE1A0_0211);
        bus.write32(0x0300_0004, 0xE1A0_0001);
        let c_shift = cpu.step(&mut bus);
        assert_eq!(cpu.r[0], 8);
        cpu.set_pc(0x0300_0004);
        let c_mov = cpu.step(&mut bus);
        assert_eq!(c_shift, c_mov + 1, "Rs shift is +1 I-cycle");
        assert_eq!(cpu.unknown_ops, 0);
    }

    #[test]
    fn ldr_reg_lsl_offset() {
        // LDR r2, [r0, r1, LSL #2]
        let cart = cart_with(&[0xE790_2101]);
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(&cart, None);
        cpu.set_mode(0x1F);
        bus.write32(0x0300_0010, 0x1234_5678);
        cpu.r[0] = 0x0300_0000;
        cpu.r[1] = 4;
        cpu.set_pc(0x0800_0000);
        cpu.step(&mut bus);
        assert_eq!(cpu.r[2], 0x1234_5678);
        assert_eq!(cpu.unknown_ops, 0);
    }

    #[test]
    fn ldr_pc_stays_arm_on_v4() {
        // LDR r15, [r0]  — word at [r0] has bit 0 set; must NOT enter Thumb.
        let cart = cart_with(&[0xE590_F000]);
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(&cart, None);
        bus.write32(0x0300_0000, 0x0800_0001);
        cpu.set_mode(0x1F);
        cpu.r[0] = 0x0300_0000;
        cpu.set_pc(0x0800_0000);
        cpu.step(&mut bus);
        assert!(!cpu.cpsr.thumb, "ARMv4 LDR PC ignores bit 0");
        assert_eq!(cpu.r[15] & 3, 0);
        assert_eq!(cpu.unknown_ops, 0);
    }

    #[test]
    fn fiq_banks_r8_and_r13() {
        let mut cpu = Cpu::new();
        cpu.set_mode(0x1F);
        cpu.r[8] = 0x1111_1111;
        cpu.r[13] = 0xAAAA_0000;
        cpu.set_mode(0x11); // FIQ
        cpu.r[8] = 0x2222_2222;
        cpu.r[13] = 0xBBBB_0000;
        cpu.set_mode(0x1F);
        assert_eq!(cpu.r[8], 0x1111_1111, "USR R8 restored");
        assert_eq!(cpu.r[13], 0xAAAA_0000, "USR R13 restored");
        cpu.set_mode(0x11);
        assert_eq!(cpu.r[8], 0x2222_2222, "FIQ R8 restored");
        assert_eq!(cpu.r[13], 0xBBBB_0000, "FIQ R13 restored");
    }

    #[test]
    fn und_does_not_clobber_usr_sp() {
        let mut cpu = Cpu::new();
        cpu.set_mode(0x1F);
        cpu.r[13] = 0x0300_7F00;
        cpu.set_mode(0x1B); // UND
        cpu.r[13] = 0xDEAD_BEEF;
        cpu.set_mode(0x1F);
        assert_eq!(cpu.r[13], 0x0300_7F00);
    }

    #[test]
    fn stm_user_bank_stores_usr_sp() {
        // STMIA r0, {r13}^
        let cart = cart_with(&[0xE8C0_2000]);
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(&cart, None);
        cpu.set_mode(0x1F);
        cpu.r[13] = 0x1111_1111;
        cpu.set_mode(0x12); // IRQ
        cpu.r[13] = 0x2222_2222;
        cpu.r[0] = 0x0300_0000;
        cpu.set_pc(0x0800_0000);
        cpu.step(&mut bus);
        assert_eq!(bus.read32(0x0300_0000), 0x1111_1111);
        assert_eq!(cpu.r[13], 0x2222_2222, "IRQ SP unchanged");
    }

    #[test]
    fn ldm_user_bank_loads_usr_sp() {
        // LDMIA r0, {r13}^
        let cart = cart_with(&[0xE8D0_2000]);
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(&cart, None);
        cpu.set_mode(0x1F);
        cpu.r[13] = 0x1111_1111;
        cpu.set_mode(0x12);
        cpu.r[13] = 0x2222_2222;
        cpu.r[0] = 0x0300_0000;
        bus.write32(0x0300_0000, 0x3333_3333);
        cpu.set_pc(0x0800_0000);
        cpu.step(&mut bus);
        assert_eq!(cpu.r[13], 0x2222_2222, "IRQ SP not overwritten");
        assert_eq!(cpu.r13_usr, 0x3333_3333);
    }

    #[test]
    fn ldrh_unaligned_rors() {
        // LDRH r0, [r1]
        let cart = cart_with(&[0xE1D1_00B0]);
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(&cart, None);
        bus.write16(0x0300_0000, 0x1234);
        cpu.r[1] = 0x0300_0001;
        cpu.set_pc(0x0800_0000);
        cpu.step(&mut bus);
        assert_eq!(cpu.unknown_ops, 0, "must decode LDRH");
        assert_eq!(cpu.r[0], 0x3400_0012, "unaligned LDRH is 32-bit ROR 8");
    }

    #[test]
    fn thumb_ldrh_unaligned_rors() {
        // LDRH r0, [r1, #0]
        let mut mem = vec![0u8; 0x100];
        mem[0..2].copy_from_slice(&0x8808u16.to_le_bytes());
        let cart = cart_with(&[]);
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(&cart, None);
        bus.iwram[..mem.len()].copy_from_slice(&mem);
        bus.write16(0x0300_0010, 0x1234);
        cpu.cpsr.thumb = true;
        cpu.r[1] = 0x0300_0011;
        cpu.set_pc(0x0300_0000);
        cpu.step(&mut bus);
        assert_eq!(cpu.r[0], 0x3400_0012, "Thumb unaligned LDRH is 32-bit ROR 8");
    }

    #[test]
    fn thumb_ldmia_skips_writeback_when_rb_in_list() {
        // LDMIA r0!, {r0, r1}  — r0 must keep the loaded word, not r0+8.
        let mut mem = vec![0u8; 0x100];
        mem[0..2].copy_from_slice(&0xC803u16.to_le_bytes());
        let cart = cart_with(&[]);
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(&cart, None);
        bus.iwram[..mem.len()].copy_from_slice(&mem);
        bus.write32(0x0300_0010, 0x1111_1111);
        bus.write32(0x0300_0014, 0x2222_2222);
        cpu.cpsr.thumb = true;
        cpu.r[0] = 0x0300_0010;
        cpu.set_pc(0x0300_0000);
        cpu.step(&mut bus);
        assert_eq!(cpu.r[0], 0x1111_1111, "loaded value wins over writeback");
        assert_eq!(cpu.r[1], 0x2222_2222);
        assert_eq!(cpu.unknown_ops, 0);
    }

    #[test]
    fn arm_ldm_skips_writeback_when_rn_in_list() {
        // LDMIA r0!, {r0, r1}
        let cart = cart_with(&[0xE8B0_0003]);
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(&cart, None);
        bus.write32(0x0300_0010, 0x1111_1111);
        bus.write32(0x0300_0014, 0x2222_2222);
        cpu.set_mode(0x1F);
        cpu.r[0] = 0x0300_0010;
        cpu.set_pc(0x0800_0000);
        cpu.step(&mut bus);
        assert_eq!(cpu.r[0], 0x1111_1111, "loaded value wins over writeback");
        assert_eq!(cpu.r[1], 0x2222_2222);
        assert_eq!(cpu.unknown_ops, 0);
    }

    #[test]
    fn arm_stm_stores_old_base_when_rn_first() {
        // STMIA r0!, {r0, r1}
        let cart = cart_with(&[0xE8A0_0003]);
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(&cart, None);
        cpu.set_mode(0x1F);
        cpu.r[0] = 0x0300_0020;
        cpu.r[1] = 0xABCD_0001;
        cpu.set_pc(0x0800_0000);
        cpu.step(&mut bus);
        assert_eq!(bus.read32(0x0300_0020), 0x0300_0020, "first is old base");
        assert_eq!(bus.read32(0x0300_0024), 0xABCD_0001);
        assert_eq!(cpu.r[0], 0x0300_0028);
    }

    #[test]
    fn thumb_ldrh_reg_unaligned_rors() {
        // LDRH r0, [r1, r2]
        let mut mem = vec![0u8; 0x100];
        mem[0..2].copy_from_slice(&0x5A88u16.to_le_bytes());
        let cart = cart_with(&[]);
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(&cart, None);
        bus.iwram[..mem.len()].copy_from_slice(&mem);
        bus.write16(0x0300_0010, 0x1234);
        cpu.cpsr.thumb = true;
        cpu.r[1] = 0x0300_0011;
        cpu.r[2] = 0;
        cpu.set_pc(0x0300_0000);
        cpu.step(&mut bus);
        assert_eq!(cpu.r[0], 0x3400_0012);
        assert_eq!(cpu.unknown_ops, 0);
    }

    #[test]
    fn thumb_stmia_always_writebacks() {
        // STMIA r0!, {r1}  — r0 advances even though r0 is not stored.
        let mut mem = vec![0u8; 0x100];
        mem[0..2].copy_from_slice(&0xC002u16.to_le_bytes());
        let cart = cart_with(&[]);
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(&cart, None);
        bus.iwram[..mem.len()].copy_from_slice(&mem);
        cpu.cpsr.thumb = true;
        cpu.r[0] = 0x0300_0020;
        cpu.r[1] = 0xABCD_EF01;
        cpu.set_pc(0x0300_0000);
        cpu.step(&mut bus);
        assert_eq!(bus.read32(0x0300_0020), 0xABCD_EF01);
        assert_eq!(cpu.r[0], 0x0300_0024);
    }

    #[test]
    fn ldrsh_unaligned_is_ldrsb() {
        // LDRSH r0, [r1]
        let cart = cart_with(&[0xE1D1_00F0]);
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(&cart, None);
        bus.write8(0x0300_0001, 0x80);
        cpu.r[1] = 0x0300_0001;
        cpu.set_pc(0x0800_0000);
        cpu.step(&mut bus);
        assert_eq!(cpu.r[0], 0xFFFF_FF80, "unaligned LDRSH is LDRSB");
    }
}
