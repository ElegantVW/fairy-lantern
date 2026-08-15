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
pub const IRQ_SERIAL: u16 = 1 << 7;
pub const IRQ_DMA0: u16 = 1 << 8;
pub const IRQ_DMA1: u16 = 1 << 9;
pub const IRQ_DMA2: u16 = 1 << 10;
pub const IRQ_DMA3: u16 = 1 << 11;
pub const IRQ_KEYPAD: u16 = 1 << 12;

/// Raise a hardware IRQ source (sets IF bit).
///
/// The BIOS check flags at `0x03007FF8` are **not** updated here. Real
/// hardware only latches them in the BIOS IRQ stub once the exception is
/// taken (`enter_irq`). IntrWait polls that mirror, not raw IF.
pub fn raise(bus: &mut Bus, bit: u16) {
    let if_ = bus.read16(0x0400_0202) | bit;
    bus.write16_raw(0x0400_0202, if_);
}

/// After each CPU / Halt slice: if pending and enabled, enter IRQ.
///
/// GBATEK / ARM7TDMI: the line is sampled after the current instruction, then
/// delayed ~2 clocks. `just_ran` is the slice that just elapsed (Halt burns
/// 64, so a pending IRQ is taken immediately when Halt wakes).
pub fn check(cpu: &mut Cpu, bus: &mut Bus, just_ran: u32) {
    if cpu.cpsr.irq_disable {
        bus.irq_countdown = 0;
        return;
    }
    let ime = bus.read16(0x0400_0208) & 1;
    if ime == 0 {
        bus.irq_countdown = 0;
        return;
    }
    let ie = bus.read16(0x0400_0200);
    let if_ = bus.read16(0x0400_0202);
    if ie & if_ == 0 {
        bus.irq_countdown = 0;
        return;
    }
    if bus.irq_countdown == 0 {
        bus.irq_countdown = 2;
    }
    if just_ran >= bus.irq_countdown as u32 {
        bus.irq_countdown = 0;
        enter_irq(cpu, bus);
    } else {
        bus.irq_countdown -= just_ran as u8;
    }
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
        //   occurred = IE & IF
        //   [03007FF8h] |= occurred   (IntrWait / VBlankIntrWait)
        //   ldr r0, =handler ; bx via 0x03007FFC
        //
        // We do **not** ack IF here. Real BIOS does; many ROM handlers still
        // read IF themselves (libgba, m4a). OR-ing the check flags is the
        // contract IntrWait needs. Leaving IF lets those handlers see the bits.
        let ie = bus.read16(0x0400_0200);
        let if_ = bus.read16(0x0400_0202);
        bus.or_irq_check_flags(ie & if_);

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::Bus;
    use crate::cart::Cart;
    use crate::cpu::Cpu;

    fn harness() -> (Cpu, Bus) {
        let cart = Cart {
            data: vec![0u8; 0x200],
            title: "t".into(),
            game_code: "T".into(),
            maker: "00".into(),
            path: "m".into(),
            inner_name: None,
        };
        let mut cpu = Cpu::new();
        cpu.set_mode(0x1F);
        cpu.cpsr.irq_disable = false;
        cpu.cpsr.thumb = false;
        cpu.set_pc(0x0800_0000);
        cpu.r[13] = 0x0300_7F00;
        let mut bus = Bus::new(&cart, None);
        bus.write32(0x0300_7FFC, 0x0800_1000);
        bus.write16(0x0400_0200, IRQ_VBLANK);
        bus.write16(0x0400_0208, 1);
        (cpu, bus)
    }

    #[test]
    fn irq_waits_two_cycles_after_pending() {
        let (mut cpu, mut bus) = harness();
        raise(&mut bus, IRQ_VBLANK);
        check(&mut cpu, &mut bus, 1);
        assert_eq!(cpu.cpsr.mode & 0x1F, 0x1F, "1 cycle is not enough");
        assert_eq!(cpu.pc(), 0x0800_0000);
        check(&mut cpu, &mut bus, 1);
        assert_eq!(cpu.cpsr.mode & 0x1F, 0x12, "2nd cycle takes IRQ");
        assert_eq!(cpu.pc(), 0x0800_1000);
    }

    #[test]
    fn halt_slice_takes_irq_immediately() {
        let (mut cpu, mut bus) = harness();
        raise(&mut bus, IRQ_VBLANK);
        check(&mut cpu, &mut bus, 64);
        assert_eq!(cpu.cpsr.mode & 0x1F, 0x12);
        assert_eq!(cpu.pc(), 0x0800_1000);
    }

    #[test]
    fn ime_off_resets_delay() {
        let (mut cpu, mut bus) = harness();
        raise(&mut bus, IRQ_VBLANK);
        check(&mut cpu, &mut bus, 1);
        bus.write16(0x0400_0208, 0);
        check(&mut cpu, &mut bus, 1);
        assert_eq!(cpu.cpsr.mode & 0x1F, 0x1F);
        assert_eq!(bus.irq_countdown, 0);
    }

    #[test]
    fn raise_does_not_touch_check_flags() {
        let (_cpu, mut bus) = harness();
        raise(&mut bus, IRQ_VBLANK);
        assert_eq!(bus.irq_check_flags(), 0, "IF raise is not the BIOS latch");
        assert_eq!(bus.read16(0x0400_0202) & IRQ_VBLANK, IRQ_VBLANK);
    }

    #[test]
    fn irq_stub_latches_check_flags() {
        let (mut cpu, mut bus) = harness();
        raise(&mut bus, IRQ_VBLANK);
        check(&mut cpu, &mut bus, 64);
        assert_eq!(bus.irq_check_flags() & IRQ_VBLANK, IRQ_VBLANK);
        assert_eq!(cpu.r[13], 0x0300_7FA0 - 24, "BIOS SP_irq frame");
    }
}
