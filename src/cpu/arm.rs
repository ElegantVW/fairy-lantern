//! ARM state decode/execute (subset → expand over phases).

use super::Cpu;
use crate::bus::Bus;

pub fn step(cpu: &mut Cpu, bus: &mut Bus) -> u32 {
    let pc = cpu.r[15];
    let op = bus.read32(pc);
    cpu.r[15] = pc.wrapping_add(4);

    if !cond_ok(cpu, op >> 28) {
        cpu.cycles += 1;
        return 1;
    }

    let cycles = exec(cpu, bus, op);
    cpu.cycles += cycles as u64;
    cycles
}

fn cond_ok(cpu: &Cpu, cond: u32) -> bool {
    let n = cpu.cpsr.n;
    let z = cpu.cpsr.z;
    let c = cpu.cpsr.c;
    let v = cpu.cpsr.v;
    match cond {
        0x0 => z,                          // EQ
        0x1 => !z,                         // NE
        0x2 => c,                          // CS/HS
        0x3 => !c,                         // CC/LO
        0x4 => n,                          // MI
        0x5 => !n,                         // PL
        0x6 => v,                          // VS
        0x7 => !v,                         // VC
        0x8 => c && !z,                    // HI
        0x9 => !c || z,                    // LS
        0xA => n == v,                     // GE
        0xB => n != v,                     // LT
        0xC => !z && n == v,               // GT
        0xD => z || n != v,                // LE
        0xE => true,                       // AL
        0xF => false,                      // NV (ARMv4 rarely)
        _ => true,
    }
}

fn exec(cpu: &mut Cpu, bus: &mut Bus, op: u32) -> u32 {
    // Branch and exchange
    if (op & 0x0FFF_FFF0) == 0x012F_FF10 {
        let rm = (op & 0xF) as usize;
        let v = cpu.r[rm];
        cpu.cpsr.thumb = (v & 1) != 0;
        cpu.r[15] = v & !1;
        return 3;
    }

    // B / BL
    if (op & 0x0E00_0000) == 0x0A00_0000 {
        let link = (op & (1 << 24)) != 0;
        let imm = (op & 0x00FF_FFFF) as i32;
        let imm = (imm << 8) >> 8;
        let offset = (imm * 4) as u32;
        // PC already advanced by 4; ARM says offset from PC+8 = (PC_after_fetch)+4
        // r15 already advanced to next insn (A+4). Branch target uses A+8 = r15+4.
        let target = cpu.r[15].wrapping_add(4).wrapping_add(offset);
        if link {
            // LR = address of next instruction (already in r15 after fetch advance)
            cpu.r[14] = cpu.r[15];
        }
        cpu.r[15] = target;
        return 3;
    }

    // SWI
    if (op & 0x0F00_0000) == 0x0F00_0000 {
        crate::bios_hle::swi_arm(cpu, bus, op);
        return 3;
    }

    // MRS
    if (op & 0x0FBF_0FFF) == 0x010F_0000 {
        let rd = ((op >> 12) & 0xF) as usize;
        let spsr = (op & (1 << 22)) != 0;
        let v = if spsr {
            cpu.spsr.to_u32()
        } else {
            cpu.cpsr.to_u32()
        };
        if rd != 15 {
            cpu.r[rd] = v;
        }
        return 1;
    }

    // MSR (reg)
    if (op & 0x0FB0_FFF0) == 0x0120_F000 {
        let rm = (op & 0xF) as usize;
        let spsr = (op & (1 << 22)) != 0;
        let v = cpu.r[rm];
        let mask = (op >> 16) & 0xF;
        apply_msr(cpu, spsr, mask, v);
        return 1;
    }

    // MSR (imm): xxxx00110_R10_field_1111_rot_imm
    // Mask must include bit 25 (I=1). 0x0DB0_F000 made the compare impossible.
    if (op & 0x0FB0_F000) == 0x0320_F000 {
        let spsr = (op & (1 << 22)) != 0;
        let mask = (op >> 16) & 0xF;
        let imm = op & 0xFF;
        let rot = ((op >> 8) & 0xF) * 2;
        let v = imm.rotate_right(rot);
        apply_msr(cpu, spsr, mask, v);
        return 1;
    }

    // Multiply: xxxx0000_00AS_...._1001  (MUL/MLA)
    if (op & 0x0FC0_00F0) == 0x0000_0090 {
        let rd = ((op >> 16) & 0xF) as usize;
        let rn = ((op >> 12) & 0xF) as usize;
        let rs = ((op >> 8) & 0xF) as usize;
        let rm = (op & 0xF) as usize;
        let a = (op & (1 << 21)) != 0;
        let s = (op & (1 << 20)) != 0;
        let mut result = cpu.r[rm].wrapping_mul(cpu.r[rs]);
        if a {
            result = result.wrapping_add(cpu.r[rn]);
        }
        if rd != 15 {
            cpu.r[rd] = result;
        }
        if s {
            cpu.cpsr.set_nz(result);
        }
        return 2;
    }

    // Long multiply: xxxx0000_1UAS_...._1001  (UMULL/UMLAL/SMULL/SMLAL)
    if (op & 0x0F80_00F0) == 0x0080_0090 {
        let rd_hi = ((op >> 16) & 0xF) as usize;
        let rd_lo = ((op >> 12) & 0xF) as usize;
        let rs = ((op >> 8) & 0xF) as usize;
        let rm = (op & 0xF) as usize;
        let signed = (op & (1 << 22)) == 0; // U bit 0 = signed
        let accumulate = (op & (1 << 21)) != 0;
        let s = (op & (1 << 20)) != 0;
        let product = if signed {
            (cpu.r[rm] as i32 as i64).wrapping_mul(cpu.r[rs] as i32 as i64) as u64
        } else {
            (cpu.r[rm] as u64).wrapping_mul(cpu.r[rs] as u64)
        };
        let mut result = product;
        if accumulate {
            let acc = ((cpu.r[rd_hi] as u64) << 32) | (cpu.r[rd_lo] as u64);
            result = result.wrapping_add(acc);
        }
        if rd_lo != 15 {
            cpu.r[rd_lo] = result as u32;
        }
        if rd_hi != 15 {
            cpu.r[rd_hi] = (result >> 32) as u32;
        }
        if s {
            cpu.cpsr.n = (result >> 63) != 0;
            cpu.cpsr.z = result == 0;
        }
        return 3;
    }

    // SWP / SWPB
    if (op & 0x0FB0_0FF0) == 0x0100_0090 {
        let b = (op & (1 << 22)) != 0;
        let rn = ((op >> 16) & 0xF) as usize;
        let rd = ((op >> 12) & 0xF) as usize;
        let rm = (op & 0xF) as usize;
        let addr = cpu.r[rn];
        if b {
            let old = bus.read8(addr) as u32;
            bus.write8(addr, cpu.r[rm] as u8);
            if rd != 15 {
                cpu.r[rd] = old;
            }
        } else {
            let old = bus.read32(addr & !3).rotate_right((addr & 3) * 8);
            bus.write32(addr & !3, cpu.r[rm]);
            if rd != 15 {
                cpu.r[rd] = old;
            }
        }
        return 2;
    }

    // Halfword / signed byte transfer: ....000P_UWI_L...._1SH1
    // (must not be confused with data-processing; bit7=1 bit4=1)
    if (op & 0x0E00_0090) == 0x0000_0090 {
        return ldrh_strh(cpu, bus, op);
    }

    // Single data transfer LDR/STR
    if (op & 0x0C00_0000) == 0x0400_0000 {
        return ldr_str(cpu, bus, op);
    }

    // Block data transfer LDM/STM
    if (op & 0x0E00_0000) == 0x0800_0000 {
        return ldm_stm(cpu, bus, op);
    }

    // Data processing
    if (op & 0x0C00_0000) == 0x0000_0000 {
        // exclude multiplies already handled (bit4=1 bit7=1)
        if (op & 0x0FC0_00F0) == 0x0000_0090 {
            return 1;
        }
        return data_processing(cpu, bus, op);
    }

    // Unknown — soft NOP
    cpu.note_unknown(op, false);
    1
}

fn apply_msr(cpu: &mut Cpu, spsr: bool, field_mask: u32, v: u32) {
    if spsr {
        if field_mask & 8 != 0 {
            cpu.spsr.n = v & (1 << 31) != 0;
            cpu.spsr.z = v & (1 << 30) != 0;
            cpu.spsr.c = v & (1 << 29) != 0;
            cpu.spsr.v = v & (1 << 28) != 0;
        }
        if field_mask & 1 != 0 {
            cpu.spsr.mode = (v & 0x1F) as u8;
            cpu.spsr.thumb = v & (1 << 5) != 0;
            cpu.spsr.fiq_disable = v & (1 << 6) != 0;
            cpu.spsr.irq_disable = v & (1 << 7) != 0;
        }
        return;
    }
    if field_mask & 8 != 0 {
        cpu.cpsr.n = v & (1 << 31) != 0;
        cpu.cpsr.z = v & (1 << 30) != 0;
        cpu.cpsr.c = v & (1 << 29) != 0;
        cpu.cpsr.v = v & (1 << 28) != 0;
    }
    if field_mask & 1 != 0 {
        // USR cannot write the control field (mode / I / F).
        if cpu.cpsr.mode & 0x1F == 0x10 {
            return;
        }
        let new_mode = (v & 0x1F) as u8;
        cpu.set_mode(new_mode);
        // ARM7TDMI: T is not writable via MSR. BX / exception return only.
        cpu.cpsr.fiq_disable = v & (1 << 6) != 0;
        cpu.cpsr.irq_disable = v & (1 << 7) != 0;
    }
}

fn barrel_shift(cpu: &Cpu, op: u32, carry_in: bool) -> (u32, bool) {
    // bit 25: immediate
    if (op & (1 << 25)) != 0 {
        let imm = op & 0xFF;
        let rot = ((op >> 8) & 0xF) * 2;
        if rot == 0 {
            return (imm, carry_in);
        }
        let v = imm.rotate_right(rot);
        let c = (v & (1 << 31)) != 0;
        return (v, c);
    }
    // register
    let rm = (op & 0xF) as usize;
    let val = if rm == 15 {
        cpu.pc_arm_read() + 4
    } else {
        cpu.r[rm]
    };
    let shift_imm = (op & (1 << 4)) == 0;
    let shift_type = (op >> 5) & 3;
    let (amount, _) = if shift_imm {
        (((op >> 7) & 0x1F), false)
    } else {
        let rs = ((op >> 8) & 0xF) as usize;
        (cpu.r[rs] & 0xFF, true)
    };

    match shift_type {
        0 => {
            // LSL
            if amount == 0 {
                (val, carry_in)
            } else if amount < 32 {
                let c = (val >> (32 - amount)) & 1 != 0;
                (val << amount, c)
            } else if amount == 32 {
                (0, (val & 1) != 0)
            } else {
                (0, false)
            }
        }
        1 => {
            // LSR
            if !shift_imm && amount == 0 {
                return (val, carry_in);
            }
            let a = if shift_imm && amount == 0 { 32 } else { amount };
            if a == 0 {
                (val, carry_in)
            } else if a < 32 {
                let c = (val >> (a - 1)) & 1 != 0;
                (val >> a, c)
            } else if a == 32 {
                (0, (val >> 31) & 1 != 0)
            } else {
                (0, false)
            }
        }
        2 => {
            // ASR
            let a = if shift_imm && amount == 0 { 32 } else { amount };
            if a == 0 {
                (val, carry_in)
            } else if a < 32 {
                let c = (val >> (a - 1)) & 1 != 0;
                (((val as i32) >> a) as u32, c)
            } else {
                let c = (val >> 31) & 1 != 0;
                let v = if c { 0xFFFF_FFFF } else { 0 };
                (v, c)
            }
        }
        3 => {
            // ROR / RRX
            if shift_imm && amount == 0 {
                // RRX
                let c = (val & 1) != 0;
                let v = (val >> 1) | if carry_in { 1 << 31 } else { 0 };
                (v, c)
            } else {
                let a = amount & 31;
                if amount == 0 {
                    (val, carry_in)
                } else if a == 0 {
                    // ROR by 32/64/… : result unchanged, C = bit 31
                    let c = (val >> 31) & 1 != 0;
                    (val, c)
                } else {
                    let c = (val >> (a - 1)) & 1 != 0;
                    (val.rotate_right(a), c)
                }
            }
        }
        _ => (val, carry_in),
    }
}

fn data_processing(cpu: &mut Cpu, _bus: &mut Bus, op: u32) -> u32 {
    let opcode = (op >> 21) & 0xF;
    let s = (op & (1 << 20)) != 0;
    let rn_i = ((op >> 16) & 0xF) as usize;
    let rd = ((op >> 12) & 0xF) as usize;
    let rn = if rn_i == 15 {
        cpu.pc_arm_read()
    } else {
        cpu.r[rn_i]
    };
    let (oper2, sh_c) = barrel_shift(cpu, op, cpu.cpsr.c);

    let (result, write, set_c, set_v, c_out, v_out) = match opcode {
        0x0 => (rn & oper2, true, false, false, sh_c, cpu.cpsr.v), // AND
        0x1 => (rn ^ oper2, true, false, false, sh_c, cpu.cpsr.v), // EOR
        0x2 => {
            // SUB
            let (r, c) = rn.overflowing_sub(oper2);
            let v = ((rn ^ oper2) & (rn ^ r)) >> 31 != 0;
            (r, true, true, true, !c, v) // ARM C = !borrow
        }
        0x3 => {
            // RSB
            let (r, c) = oper2.overflowing_sub(rn);
            let v = ((oper2 ^ rn) & (oper2 ^ r)) >> 31 != 0;
            (r, true, true, true, !c, v)
        }
        0x4 => {
            // ADD
            let (r, c) = rn.overflowing_add(oper2);
            let v = (!(rn ^ oper2) & (rn ^ r)) >> 31 != 0;
            (r, true, true, true, c, v)
        }
        0x5 => {
            // ADC
            let carry = if cpu.cpsr.c { 1u32 } else { 0 };
            let (r1, c1) = rn.overflowing_add(oper2);
            let (r, c2) = r1.overflowing_add(carry);
            let v = (!(rn ^ oper2) & (rn ^ r)) >> 31 != 0;
            (r, true, true, true, c1 || c2, v)
        }
        0x6 => {
            // SBC
            let carry = if cpu.cpsr.c { 0u32 } else { 1 }; // borrow
            let (r1, c1) = rn.overflowing_sub(oper2);
            let (r, c2) = r1.overflowing_sub(carry);
            let v = ((rn ^ oper2) & (rn ^ r)) >> 31 != 0;
            (r, true, true, true, !(c1 || c2), v)
        }
        0x7 => {
            // RSC
            let carry = if cpu.cpsr.c { 0u32 } else { 1 };
            let (r1, c1) = oper2.overflowing_sub(rn);
            let (r, c2) = r1.overflowing_sub(carry);
            let v = ((oper2 ^ rn) & (oper2 ^ r)) >> 31 != 0;
            (r, true, true, true, !(c1 || c2), v)
        }
        0x8 => (rn & oper2, false, false, false, sh_c, cpu.cpsr.v), // TST
        0x9 => (rn ^ oper2, false, false, false, sh_c, cpu.cpsr.v), // TEQ
        0xA => {
            // CMP
            let (r, c) = rn.overflowing_sub(oper2);
            let v = ((rn ^ oper2) & (rn ^ r)) >> 31 != 0;
            (r, false, true, true, !c, v)
        }
        0xB => {
            // CMN
            let (r, c) = rn.overflowing_add(oper2);
            let v = (!(rn ^ oper2) & (rn ^ r)) >> 31 != 0;
            (r, false, true, true, c, v)
        }
        0xC => (rn | oper2, true, false, false, sh_c, cpu.cpsr.v), // ORR
        0xD => (oper2, true, false, false, sh_c, cpu.cpsr.v),      // MOV
        0xE => (rn & !oper2, true, false, false, sh_c, cpu.cpsr.v), // BIC
        0xF => (!oper2, true, false, false, sh_c, cpu.cpsr.v),     // MVN
        _ => (0, false, false, false, sh_c, cpu.cpsr.v),
    };

    if write && rd != 15 {
        cpu.r[rd] = result;
    } else if write && rd == 15 {
        // MOV/SUB/… to PC. With S bit: restore SPSR (IRQ/SVC return).
        if s {
            let thumb = cpu.spsr.thumb;
            cpu.restore_spsr();
            if thumb {
                cpu.r[15] = result & !1;
            } else {
                cpu.r[15] = result & !3;
            }
        } else {
            cpu.r[15] = result & !3;
        }
        return 3;
    }

    if s && rd != 15 {
        cpu.cpsr.set_nz(result);
        if set_c {
            cpu.cpsr.c = c_out;
        } else {
            cpu.cpsr.c = sh_c;
        }
        if set_v {
            cpu.cpsr.v = v_out;
        }
    } else if s && !write {
        // TST/TEQ/CMP/CMN always set flags
        cpu.cpsr.set_nz(result);
        if set_c {
            cpu.cpsr.c = c_out;
        } else {
            cpu.cpsr.c = sh_c;
        }
        if set_v {
            cpu.cpsr.v = v_out;
        }
    }

    1
}

fn ldrh_strh(cpu: &mut Cpu, bus: &mut Bus, op: u32) -> u32 {
    let p = (op & (1 << 24)) != 0;
    let u = (op & (1 << 23)) != 0;
    let i = (op & (1 << 22)) != 0; // imm offset when set for halfword form
    let w = (op & (1 << 21)) != 0;
    let l = (op & (1 << 20)) != 0;
    let s = (op & (1 << 6)) != 0;
    let h = (op & (1 << 5)) != 0;
    let rn_i = ((op >> 16) & 0xF) as usize;
    let rd = ((op >> 12) & 0xF) as usize;
    // Rn=PC → PC+8 (not PC+12; that applies only to Rm in shifts)
    let base = if rn_i == 15 {
        cpu.pc_arm_read()
    } else {
        cpu.r[rn_i]
    };
    let offset = if i {
        ((op >> 4) & 0xF0) | (op & 0xF)
    } else {
        let rm = (op & 0xF) as usize;
        cpu.r[rm]
    };
    let offset = if u {
        offset
    } else {
        (0u32).wrapping_sub(offset)
    };
    let addr = if p {
        base.wrapping_add(offset)
    } else {
        base
    };

    if l {
        let val = if h && !s {
            // LDRH — unaligned: aligned halfword ROR 8 (GBATEK)
            let hw = bus.read16(addr & !1) as u32;
            if addr & 1 != 0 {
                hw.rotate_right(8)
            } else {
                hw
            }
        } else if !h && s {
            // LDRSB
            bus.read8(addr) as i8 as i32 as u32
        } else if h && s {
            // LDRSH — unaligned is LDRSB of that byte
            if addr & 1 != 0 {
                bus.read8(addr) as i8 as i32 as u32
            } else {
                bus.read16(addr) as i16 as i32 as u32
            }
        } else {
            bus.read8(addr) as u32
        };
        if rd != 15 {
            cpu.r[rd] = val;
        }
    } else if h && !s {
        // STRH — Rd=PC stores PC+12
        let val = if rd == 15 {
            cpu.pc_arm_read().wrapping_add(4)
        } else {
            cpu.r[rd]
        };
        bus.write16(addr & !1, val as u16);
    }

    if (w || !p) && rn_i != 15 {
        if p {
            cpu.r[rn_i] = addr;
        } else {
            cpu.r[rn_i] = base.wrapping_add(offset);
        }
    }
    let bytes = if h { 2 } else { 1 };
    2 + bus.data_waitstates(addr, bytes)
}

/// ARM addressing mode 2 offset (LDR/STR/LDRB/STRB).
/// Bit 25 = 0 → 12-bit immediate. Bit 25 = 1 → Rm shifted by an immediate.
fn addr_mode2_offset(cpu: &Cpu, op: u32) -> u32 {
    if op & (1 << 25) == 0 {
        return op & 0xFFF;
    }
    let rm = (op & 0xF) as usize;
    let val = if rm == 15 {
        cpu.pc_arm_read().wrapping_add(4)
    } else {
        cpu.r[rm]
    };
    let shift_type = (op >> 5) & 3;
    let amount = (op >> 7) & 0x1F;
    match shift_type {
        0 => {
            // LSL #n (n=0 → no shift)
            if amount == 0 {
                val
            } else {
                val << amount
            }
        }
        1 => {
            // LSR #n (n=0 → LSR #32)
            if amount == 0 {
                0
            } else {
                val >> amount
            }
        }
        2 => {
            // ASR #n (n=0 → ASR #32)
            let a = if amount == 0 { 32 } else { amount };
            ((val as i32) >> a.min(31)) as u32
        }
        _ => {
            // ROR #n (n=0 → RRX)
            if amount == 0 {
                (val >> 1) | if cpu.cpsr.c { 1 << 31 } else { 0 }
            } else {
                val.rotate_right(amount)
            }
        }
    }
}

fn ldr_str(cpu: &mut Cpu, bus: &mut Bus, op: u32) -> u32 {
    let p = (op & (1 << 24)) != 0; // pre
    let u = (op & (1 << 23)) != 0; // up
    let b = (op & (1 << 22)) != 0; // byte
    let w = (op & (1 << 21)) != 0; // writeback
    let l = (op & (1 << 20)) != 0; // load
    let rn_i = ((op >> 16) & 0xF) as usize;
    let rd = ((op >> 12) & 0xF) as usize;
    // Rn=PC → PC+8
    let base = if rn_i == 15 {
        cpu.pc_arm_read()
    } else {
        cpu.r[rn_i]
    };

    // Addressing mode 2: bit 25 is "reg offset", the opposite of data-processing
    // bit 25 ("imm operand"). Feeding this opcode to barrel_shift treats
    // Rm as an 8-bit rotated immediate — STRB [r5, r6] then writes r5+6
    // instead of r5+r6. m4a reverb is `strb r0, [r5, r6]` with r6=1584.
    let offset = addr_mode2_offset(cpu, op);

    let offset = if u {
        offset
    } else {
        (0u32).wrapping_sub(offset)
    };

    let addr = if p {
        base.wrapping_add(offset)
    } else {
        base
    };

    if l {
        let val = if b {
            bus.read8(addr) as u32
        } else {
            bus.read32(addr & !3).rotate_right((addr & 3) * 8)
        };
        if rd == 15 {
            // ARMv4: LDR PC stays in ARM; bit 0 does not select Thumb (use BX).
            cpu.r[15] = val & !3;
        } else {
            cpu.r[rd] = val;
        }
    } else {
        // STR Rd=PC stores PC+12
        let val = if rd == 15 {
            cpu.pc_arm_read().wrapping_add(4)
        } else {
            cpu.r[rd]
        };
        if b {
            bus.write8(addr, val as u8);
        } else {
            bus.write32(addr & !3, val);
        }
    }

    if (w || !p) && rn_i != 15 {
        if p {
            cpu.r[rn_i] = addr;
        } else {
            cpu.r[rn_i] = base.wrapping_add(offset);
        }
    }
    2 + bus.data_waitstates(addr, if b { 1 } else { 4 })
}

fn ldm_stm(cpu: &mut Cpu, bus: &mut Bus, op: u32) -> u32 {
    let p = (op & (1 << 24)) != 0;
    let u = (op & (1 << 23)) != 0;
    let s = (op & (1 << 22)) != 0; // PSR / user-bank
    let w = (op & (1 << 21)) != 0;
    let l = (op & (1 << 20)) != 0;
    let rn = ((op >> 16) & 0xF) as usize;
    let mut list = (op & 0xFFFF) as u16;
    // Empty rlist: transfer R15 only, writeback ±0x40 (ARMv4 empty-list quirk)
    let empty = list == 0;
    if empty {
        list = 1 << 15;
    }
    let count = if empty { 16 } else { list.count_ones() };
    let addr = cpu.r[rn];
    // IB/IA/DB/DA start address
    let start = match (p, u) {
        (true, true) => addr.wrapping_add(4),                           // IB
        (false, true) => addr,                                          // IA
        (true, false) => addr.wrapping_sub(4 * count),                  // DB
        (false, false) => addr.wrapping_sub(4 * count).wrapping_add(4), // DA
    };
    let mut a = start;
    let load_pc = l && (list & (1 << 15)) != 0;
    // S=1 and R15 not in list: transfer user-bank R8–R14 (GBATEK / ARM ARM).
    let user_bank = s && !load_pc;
    let base_in_list = !empty && (list & (1 << rn)) != 0;
    let lowest = list.trailing_zeros() as usize;
    let final_base = if u {
        addr.wrapping_add(4 * count)
    } else {
        addr.wrapping_sub(4 * count)
    };
    for i in 0..16 {
        if list & (1 << i) != 0 {
            if l {
                let v = bus.read32(a);
                if i == 15 {
                    // Exception return: LDM with S and PC restores SPSR
                    if s {
                        let thumb = cpu.spsr.thumb;
                        cpu.restore_spsr();
                        if thumb {
                            cpu.r[15] = v & !1;
                        } else {
                            cpu.r[15] = v & !3;
                        }
                    } else {
                        // ARMv4 LDM PC stays in ARM
                        cpu.r[15] = v & !3;
                    }
                } else if user_bank && (8..=14).contains(&i) {
                    cpu.set_user_reg(i, v);
                } else {
                    cpu.r[i] = v;
                }
            } else {
                let v = if i == 15 {
                    // STM PC stores PC+12
                    cpu.pc_arm_read().wrapping_add(4)
                } else if i == rn && base_in_list && i != lowest {
                    // ARM7: Rn in list, not first → store writeback value
                    final_base
                } else if user_bank && (8..=14).contains(&i) {
                    cpu.user_reg(i)
                } else {
                    cpu.r[i]
                };
                bus.write32(a, v);
            }
            a = a.wrapping_add(4);
        }
    }
    if w && rn != 15 && !user_bank && !(l && base_in_list) {
        cpu.r[rn] = final_base;
    }
    count + 1 + bus.data_burst_waitstates(start, count)
}
