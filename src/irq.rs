//! Interrupt controller (IME / IE / IF) + CPU IRQ entry + BIOS IRQ HLE.

use crate::bus::Bus;
use crate::cpu::{Cpu, BIOS_IRQ_RETURN};

pub const IRQ_VBLANK: u16 = 1 << 0;
pub const IRQ_HBLANK: u16 = 1 << 1;
pub const IRQ_VCOUNTER: u16 = 1 << 2;
pub const IRQ_TIMER0: u16 = 1 << 3;
pub const IRQ_TIMER1: u16 = 1 << 4;
pub const IRQ_TIMER2: u16 = 1 << 5;
pub const IRQ_TIMER3: u16 = 1 << 6;
pub const IRQ_DMA0: u16 = 1 << 8;
pub const IRQ_DMA1: u16 = 1 << 9;
pub const IRQ_DMA2: u16 = 1 << 10;
pub const IRQ_DMA3: u16 = 1 << 11;
pub const IRQ_KEYPAD: u16 = 1 << 12;

/// Raise a hardware IRQ source (sets IF bit).
pub fn raise(bus: &mut Bus, bit: u16) {
    let if_ = bus.read16(0x0400_0202) | bit;
    bus.write16_raw(0x0400_0202, if_);
    // HLE BIOS IntrWait mirror (real BIOS latches checked IRQs at 0x03007FF8)
    let idx = 0x7FF8usize;
    if idx + 1 < bus.iwram.len() {
        let cur = u16::from_le_bytes([bus.iwram[idx], bus.iwram[idx + 1]]);
        let n = cur | bit;
        let b = n.to_le_bytes();
        bus.iwram[idx] = b[0];
        bus.iwram[idx + 1] = b[1];
    }
}

/// After each CPU step: if pending and enabled, enter IRQ.
pub fn check(cpu: &mut Cpu, bus: &mut Bus) {
    if cpu.cpsr.irq_disable {
        return;
    }
    let ime = bus.read16(0x0400_0208) & 1;
    if ime == 0 {
        return;
    }
    let ie = bus.read16(0x0400_0200);
    let if_ = bus.read16(0x0400_0202);
    if ie & if_ == 0 {
        return;
    }
    enter_irq(cpu, bus);
}

fn enter_irq(cpu: &mut Cpu, bus: &mut Bus) {
    bus.irq_count = bus.irq_count.wrapping_add(1);
    // LR_irq = address of next instruction + 4 (both ARM and Thumb on ARMv4T IRQ)
    let lr = cpu.r[15].wrapping_add(4);
    let spsr = cpu.cpsr;

    // Switch to IRQ mode (banks R13/R14/SPSR)
    cpu.set_mode(0x12);
    cpu.spsr = spsr;
    cpu.cpsr.irq_disable = true;
    cpu.cpsr.thumb = false;
    cpu.cpsr.mode = 0x12;
    cpu.r[14] = lr;

    if bus.hle_bios {
        // Mimic GBA BIOS IRQ stub:
        //   stmfd sp!, {r0-r3, r12, lr}
        //   ldr r0, =handler ; bx via load from 0x03007FFC
        let mut sp = cpu.r[13].wrapping_sub(6 * 4);
        cpu.r[13] = sp;
        let push = [
            cpu.r[0],
            cpu.r[1],
            cpu.r[2],
            cpu.r[3],
            cpu.r[12],
            cpu.r[14],
        ];
        for v in push {
            bus.write32(sp, v);
            sp = sp.wrapping_add(4);
        }

        // Return path: BIOS epilogue at BIOS_IRQ_RETURN
        cpu.r[14] = BIOS_IRQ_RETURN;

        // Handler pointer (also mirrored at 0x03FFFFFC)
        let handler = bus.read32(0x0300_7FFC);
        if handler == 0 {
            // Nothing installed — immediately return
            let _ = hle_irq_return(cpu, bus);
            return;
        }
        cpu.cpsr.thumb = (handler & 1) != 0;
        cpu.r[15] = handler & !1;
    } else {
        cpu.r[15] = 0x0000_0018;
    }
}

/// HLE of BIOS IRQ epilogue: `ldmfd sp!, {r0-r3,r12,lr}` + `subs pc, lr, #4`.
pub fn hle_irq_return(cpu: &mut Cpu, bus: &mut Bus) -> u32 {
    // Must be in IRQ mode with banked SP
    let mut sp = cpu.r[13];
    cpu.r[0] = bus.read32(sp);
    sp = sp.wrapping_add(4);
    cpu.r[1] = bus.read32(sp);
    sp = sp.wrapping_add(4);
    cpu.r[2] = bus.read32(sp);
    sp = sp.wrapping_add(4);
    cpu.r[3] = bus.read32(sp);
    sp = sp.wrapping_add(4);
    cpu.r[12] = bus.read32(sp);
    sp = sp.wrapping_add(4);
    let lr = bus.read32(sp);
    sp = sp.wrapping_add(4);
    cpu.r[13] = sp;
    cpu.r[14] = lr;

    // subs pc, lr, #4 — restore SPSR → CPSR (mode bank switch)
    let ret = lr.wrapping_sub(4);
    cpu.restore_spsr();
    if cpu.cpsr.thumb {
        cpu.r[15] = ret & !1;
    } else {
        cpu.r[15] = ret & !3;
    }
    3
}
