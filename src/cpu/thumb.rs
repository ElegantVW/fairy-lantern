//! Thumb state decode/execute (subset).

use super::Cpu;
use crate::bus::Bus;

pub fn step(cpu: &mut Cpu, bus: &mut Bus) -> u32 {
    let pc = cpu.r[15] & !1;
    let op = bus.read16(pc) as u32;
    cpu.r[15] = pc.wrapping_add(2);

    let cycles = exec(cpu, bus, op);
    cpu.cycles += cycles as u64;
    cycles
}

fn exec(cpu: &mut Cpu, bus: &mut Bus, op: u32) -> u32 {
    // Software interrupt
    if (op & 0xFF00) == 0xDF00 {
        crate::bios_hle::swi_thumb(cpu, bus, op);
        return 3;
    }

    // Unconditional branch
    if (op & 0xF800) == 0xE000 {
        let imm = (op & 0x7FF) as i32;
        let imm = (imm << 21) >> 21;
        let off = (imm * 2) as u32;
        cpu.r[15] = cpu.r[15].wrapping_add(2).wrapping_add(off); // +4 from insn = (pc after +2)+2
        return 3;
    }

    // BL high / low (two halfwords). High: LR = (A+4) + SignExtend(imm11)<<12
    // After fetch R15 = A+2, so LR = R15+2 + (imm<<12).
    if (op & 0xF800) == 0xF000 {
        let imm = (op & 0x7FF) as i32;
        let imm = (imm << 21) >> 21;
        cpu.r[14] = cpu
            .r[15]
            .wrapping_add(2)
            .wrapping_add((imm << 12) as u32);
        return 1;
    }
    if (op & 0xF800) == 0xF800 {
        let imm = op & 0x7FF;
        let target = cpu.r[14].wrapping_add(imm << 1);
        // After fetch of BL-low at A+2, R15=A+4 = return address
        cpu.r[14] = cpu.r[15] | 1;
        cpu.r[15] = target & !1;
        return 3;
    }

    // Conditional branch
    if (op & 0xF000) == 0xD000 && (op & 0x0F00) != 0x0E00 && (op & 0x0F00) != 0x0F00 {
        let cond = (op >> 8) & 0xF;
        if cond_ok(cpu, cond) {
            let imm = op as i8 as i32;
            let off = (imm * 2) as u32;
            cpu.r[15] = cpu.r[15].wrapping_add(2).wrapping_add(off);
            return 3;
        }
        return 1;
    }

    // PC-relative load: addr = (PC+4) word-aligned + imm*4
    if (op & 0xF800) == 0x4800 {
        let rd = ((op >> 8) & 7) as usize;
        let imm = (op & 0xFF) << 2;
        let addr = (cpu.pc_thumb_read() & !2).wrapping_add(imm);
        cpu.r[rd] = bus.read32(addr);
        return 2;
    }

    // SP-relative load/store
    if (op & 0xF000) == 0x9000 {
        let l = (op & 0x0800) != 0;
        let rd = ((op >> 8) & 7) as usize;
        let imm = (op & 0xFF) << 2;
        let addr = cpu.r[13].wrapping_add(imm);
        if l {
            cpu.r[rd] = bus.read32(addr);
        } else {
            bus.write32(addr, cpu.r[rd]);
        }
        return 2;
    }

    // Load address (ADD rd, PC/SP, #imm)
    if (op & 0xF000) == 0xA000 {
        let sp = (op & 0x0800) != 0;
        let rd = ((op >> 8) & 7) as usize;
        let imm = (op & 0xFF) << 2;
        let base = if sp {
            cpu.r[13]
        } else {
            // ADD rd, PC, #imm — PC+4, bit 1 cleared
            cpu.pc_thumb_read() & !2
        };
        cpu.r[rd] = base.wrapping_add(imm);
        return 1;
    }

    // Add offset to SP
    if (op & 0xFF00) == 0xB000 {
        let imm = (op & 0x7F) << 2;
        if (op & 0x80) != 0 {
            cpu.r[13] = cpu.r[13].wrapping_sub(imm);
        } else {
            cpu.r[13] = cpu.r[13].wrapping_add(imm);
        }
        return 1;
    }

    // Push / pop
    if (op & 0xF600) == 0xB400 {
        let l = (op & 0x0800) != 0;
        let r = (op & 0x0100) != 0;
        let list = op & 0xFF;
        if l {
            // POP
            let mut sp = cpu.r[13];
            for i in 0..8 {
                if list & (1 << i) != 0 {
                    cpu.r[i] = bus.read32(sp);
                    sp = sp.wrapping_add(4);
                }
            }
            if r {
                let v = bus.read32(sp);
                sp = sp.wrapping_add(4);
                cpu.cpsr.thumb = (v & 1) != 0;
                cpu.r[15] = v & !1;
            }
            cpu.r[13] = sp;
        } else {
            // PUSH
            let mut count = list.count_ones();
            if r {
                count += 1;
            }
            let mut sp = cpu.r[13].wrapping_sub(4 * count);
            let base = sp;
            for i in 0..8 {
                if list & (1 << i) != 0 {
                    bus.write32(sp, cpu.r[i]);
                    sp = sp.wrapping_add(4);
                }
            }
            if r {
                bus.write32(sp, cpu.r[14]);
            }
            cpu.r[13] = base;
        }
        return 2;
    }

    // Multiple load/store
    if (op & 0xF000) == 0xC000 {
        let l = (op & 0x0800) != 0;
        let rb = ((op >> 8) & 7) as usize;
        let list = op & 0xFF;
        let start = cpu.r[rb];
        let mut addr = start;
        for i in 0..8 {
            if list & (1 << i) != 0 {
                if l {
                    cpu.r[i] = bus.read32(addr);
                } else {
                    bus.write32(addr, cpu.r[i]);
                }
                addr = addr.wrapping_add(4);
            }
        }
        if list != 0 {
            cpu.r[rb] = addr;
        }
        let n = list.count_ones().max(1);
        return 2 + bus.data_burst_waitstates(start, n);
    }

    // Load/store with imm
    if (op & 0xE000) == 0x6000 {
        let b = (op & 0x1000) != 0;
        let l = (op & 0x0800) != 0;
        let imm = ((op >> 6) & 0x1F) as u32;
        let offset = if b { imm } else { imm << 2 };
        let rb = ((op >> 3) & 7) as usize;
        let rd = (op & 7) as usize;
        let addr = cpu.r[rb].wrapping_add(offset);
        if l {
            cpu.r[rd] = if b {
                bus.read8(addr) as u32
            } else {
                // Unaligned LDR rotates like ARM
                bus.read32(addr & !3).rotate_right((addr & 3) * 8)
            };
        } else if b {
            bus.write8(addr, cpu.r[rd] as u8);
        } else {
            bus.write32(addr & !3, cpu.r[rd]);
        }
        return 2 + bus.data_waitstates(addr, if b { 1 } else { 4 });
    }

    // Load/store halfword imm
    if (op & 0xF000) == 0x8000 {
        let l = (op & 0x0800) != 0;
        let imm = ((op >> 6) & 0x1F) << 1;
        let rb = ((op >> 3) & 7) as usize;
        let rd = (op & 7) as usize;
        let addr = cpu.r[rb].wrapping_add(imm);
        if l {
            cpu.r[rd] = bus.read16(addr & !1) as u32;
        } else {
            bus.write16(addr & !1, cpu.r[rd] as u16);
        }
        return 2 + bus.data_waitstates(addr, 2);
    }

    // Load/store with register offset — all 0101_xxx forms:
    // 000 STR, 001 STRH, 010 STRB, 011 LDRSB, 100 LDR, 101 LDRH, 110 LDRB, 111 LDRSH
    if (op & 0xF000) == 0x5000 {
        let opc = (op >> 9) & 7;
        let ro = ((op >> 6) & 7) as usize;
        let rb = ((op >> 3) & 7) as usize;
        let rd = (op & 7) as usize;
        let addr = cpu.r[rb].wrapping_add(cpu.r[ro]);
        match opc {
            0 => bus.write32(addr & !3, cpu.r[rd]),                 // STR
            1 => bus.write16(addr & !1, cpu.r[rd] as u16),          // STRH
            2 => bus.write8(addr, cpu.r[rd] as u8),                 // STRB
            3 => {
                // LDRSB
                cpu.r[rd] = bus.read8(addr) as i8 as i32 as u32;
            }
            4 => {
                // LDR — unaligned rotates
                cpu.r[rd] = bus.read32(addr & !3).rotate_right((addr & 3) * 8);
            }
            5 => cpu.r[rd] = bus.read16(addr & !1) as u32, // LDRH
            6 => cpu.r[rd] = bus.read8(addr) as u32,       // LDRB
            7 => {
                // LDRSH — unaligned: sign-extend byte pair from aligned half
                let v = bus.read16(addr & !1) as i16 as i32 as u32;
                cpu.r[rd] = if addr & 1 != 0 {
                    // GBA: LDRSH unaligned forces LDRSB of that byte
                    bus.read8(addr) as i8 as i32 as u32
                } else {
                    v
                };
            }
            _ => {}
        }
        let bytes = match opc {
            1 | 5 => 2,
            2 | 3 | 6 | 7 => 1,
            _ => 4,
        };
        return 2 + bus.data_waitstates(addr, bytes);
    }

    // Hi register ops / BX — R15 reads as PC+4
    if (op & 0xFC00) == 0x4400 {
        let op_h = (op >> 8) & 3;
        let rd = (((op >> 4) & 8) | (op & 7)) as usize;
        let rs = ((op >> 3) & 0xF) as usize;
        let rs_v = if rs == 15 {
            cpu.pc_thumb_read()
        } else {
            cpu.r[rs]
        };
        match op_h {
            0 => {
                // ADD
                let d = if rd == 15 {
                    cpu.pc_thumb_read()
                } else {
                    cpu.r[rd]
                };
                let r = d.wrapping_add(rs_v);
                if rd == 15 {
                    cpu.r[15] = r & !1;
                } else {
                    cpu.r[rd] = r;
                }
            }
            1 => {
                // CMP
                let d = if rd == 15 {
                    cpu.pc_thumb_read()
                } else {
                    cpu.r[rd]
                };
                let (r, c) = d.overflowing_sub(rs_v);
                let v = ((d ^ rs_v) & (d ^ r)) >> 31 != 0;
                cpu.cpsr.set_nz(r);
                cpu.cpsr.c = !c;
                cpu.cpsr.v = v;
            }
            2 => {
                // MOV
                if rd == 15 {
                    cpu.r[15] = rs_v & !1;
                } else {
                    cpu.r[rd] = rs_v;
                }
            }
            3 => {
                // BX
                cpu.cpsr.thumb = (rs_v & 1) != 0;
                cpu.r[15] = rs_v & !1;
            }
            _ => {}
        }
        return 1;
    }

    // ALU operations
    if (op & 0xFC00) == 0x4000 {
        let opc = (op >> 6) & 0xF;
        let rs = ((op >> 3) & 7) as usize;
        let rd = (op & 7) as usize;
        let a = cpu.r[rd];
        let b = cpu.r[rs];
        match opc {
            0x0 => {
                cpu.r[rd] = a & b;
                cpu.cpsr.set_nz(cpu.r[rd]);
            }
            0x1 => {
                cpu.r[rd] = a ^ b;
                cpu.cpsr.set_nz(cpu.r[rd]);
            }
            0x2 => {
                // LSL reg
                let s = b & 0xFF;
                if s == 0 {
                    cpu.cpsr.set_nz(a);
                } else if s < 32 {
                    cpu.cpsr.c = (a >> (32 - s)) & 1 != 0;
                    cpu.r[rd] = a << s;
                    cpu.cpsr.set_nz(cpu.r[rd]);
                } else {
                    cpu.cpsr.c = if s == 32 { (a & 1) != 0 } else { false };
                    cpu.r[rd] = 0;
                    cpu.cpsr.set_nz(0);
                }
            }
            0x3 => {
                let s = b & 0xFF;
                if s == 0 {
                    cpu.cpsr.set_nz(a);
                } else if s < 32 {
                    cpu.cpsr.c = (a >> (s - 1)) & 1 != 0;
                    cpu.r[rd] = a >> s;
                    cpu.cpsr.set_nz(cpu.r[rd]);
                } else {
                    cpu.cpsr.c = if s == 32 {
                        (a >> 31) & 1 != 0
                    } else {
                        false
                    };
                    cpu.r[rd] = 0;
                    cpu.cpsr.set_nz(0);
                }
            }
            0x4 => {
                let s = b & 0xFF;
                if s == 0 {
                    cpu.cpsr.set_nz(a);
                } else if s < 32 {
                    cpu.cpsr.c = (a >> (s - 1)) & 1 != 0;
                    cpu.r[rd] = ((a as i32) >> s) as u32;
                    cpu.cpsr.set_nz(cpu.r[rd]);
                } else {
                    let c = (a >> 31) & 1 != 0;
                    cpu.cpsr.c = c;
                    cpu.r[rd] = if c { 0xFFFF_FFFF } else { 0 };
                    cpu.cpsr.set_nz(cpu.r[rd]);
                }
            }
            0x5 => {
                // ADC
                let carry = if cpu.cpsr.c { 1 } else { 0 };
                let (r1, c1) = a.overflowing_add(b);
                let (r, c2) = r1.overflowing_add(carry);
                cpu.r[rd] = r;
                cpu.cpsr.set_nz(r);
                cpu.cpsr.c = c1 || c2;
                cpu.cpsr.v = (!(a ^ b) & (a ^ r)) >> 31 != 0;
            }
            0x6 => {
                let carry = if cpu.cpsr.c { 0 } else { 1 };
                let (r1, c1) = a.overflowing_sub(b);
                let (r, c2) = r1.overflowing_sub(carry);
                cpu.r[rd] = r;
                cpu.cpsr.set_nz(r);
                cpu.cpsr.c = !(c1 || c2);
                cpu.cpsr.v = ((a ^ b) & (a ^ r)) >> 31 != 0;
            }
            0x7 => {
                // ROR
                let s = b & 0xFF;
                if s == 0 {
                    cpu.cpsr.set_nz(a);
                } else {
                    let s = s & 31;
                    if s == 0 {
                        cpu.cpsr.c = (a >> 31) & 1 != 0;
                        cpu.r[rd] = a;
                    } else {
                        cpu.cpsr.c = (a >> (s - 1)) & 1 != 0;
                        cpu.r[rd] = a.rotate_right(s);
                    }
                    cpu.cpsr.set_nz(cpu.r[rd]);
                }
            }
            0x8 => {
                // TST
                let r = a & b;
                cpu.cpsr.set_nz(r);
            }
            0x9 => {
                // NEG
                let (r, c) = (0u32).overflowing_sub(b);
                cpu.r[rd] = r;
                cpu.cpsr.set_nz(r);
                cpu.cpsr.c = !c;
                cpu.cpsr.v = (b & r) >> 31 != 0;
            }
            0xA => {
                let (r, c) = a.overflowing_sub(b);
                cpu.cpsr.set_nz(r);
                cpu.cpsr.c = !c;
                cpu.cpsr.v = ((a ^ b) & (a ^ r)) >> 31 != 0;
            }
            0xB => {
                let (r, c) = a.overflowing_add(b);
                cpu.cpsr.set_nz(r);
                cpu.cpsr.c = c;
                cpu.cpsr.v = (!(a ^ b) & (a ^ r)) >> 31 != 0;
            }
            0xC => {
                cpu.r[rd] = a | b;
                cpu.cpsr.set_nz(cpu.r[rd]);
            }
            0xD => {
                cpu.r[rd] = a.wrapping_mul(b);
                cpu.cpsr.set_nz(cpu.r[rd]);
            }
            0xE => {
                cpu.r[rd] = a & !b;
                cpu.cpsr.set_nz(cpu.r[rd]);
            }
            0xF => {
                cpu.r[rd] = !b;
                cpu.cpsr.set_nz(cpu.r[rd]);
            }
            _ => {}
        }
        return 1;
    }

    // Move shifted register / add-sub / imm / ALU already partial
    // Format 1: move shifted register
    if (op & 0xE000) == 0x0000 {
        let opc = (op >> 11) & 3;
        if opc != 3 {
            let offset = (op >> 6) & 0x1F;
            let rs = ((op >> 3) & 7) as usize;
            let rd = (op & 7) as usize;
            let val = cpu.r[rs];
            match opc {
                0 => {
                    // LSL
                    if offset == 0 {
                        cpu.r[rd] = val;
                    } else {
                        cpu.cpsr.c = (val >> (32 - offset)) & 1 != 0;
                        cpu.r[rd] = val << offset;
                    }
                    cpu.cpsr.set_nz(cpu.r[rd]);
                }
                1 => {
                    // LSR
                    let a = if offset == 0 { 32 } else { offset };
                    if a < 32 {
                        cpu.cpsr.c = (val >> (a - 1)) & 1 != 0;
                        cpu.r[rd] = val >> a;
                    } else {
                        cpu.cpsr.c = (val >> 31) & 1 != 0;
                        cpu.r[rd] = 0;
                    }
                    cpu.cpsr.set_nz(cpu.r[rd]);
                }
                2 => {
                    let a = if offset == 0 { 32 } else { offset };
                    if a < 32 {
                        cpu.cpsr.c = (val >> (a - 1)) & 1 != 0;
                        cpu.r[rd] = ((val as i32) >> a) as u32;
                    } else {
                        let c = (val >> 31) & 1 != 0;
                        cpu.cpsr.c = c;
                        cpu.r[rd] = if c { 0xFFFF_FFFF } else { 0 };
                    }
                    cpu.cpsr.set_nz(cpu.r[rd]);
                }
                _ => {}
            }
            return 1;
        }
        // Format 2: add/sub
        let i = (op & 0x0400) != 0;
        let sub = (op & 0x0200) != 0;
        let rn_imm = (op >> 6) & 7;
        let rs = ((op >> 3) & 7) as usize;
        let rd = (op & 7) as usize;
        let b = if i {
            rn_imm
        } else {
            cpu.r[rn_imm as usize]
        };
        let a = cpu.r[rs];
        let (r, c, v) = if sub {
            let (r, c) = a.overflowing_sub(b);
            let v = ((a ^ b) & (a ^ r)) >> 31 != 0;
            (r, !c, v)
        } else {
            let (r, c) = a.overflowing_add(b);
            let v = (!(a ^ b) & (a ^ r)) >> 31 != 0;
            (r, c, v)
        };
        cpu.r[rd] = r;
        cpu.cpsr.set_nz(r);
        cpu.cpsr.c = c;
        cpu.cpsr.v = v;
        return 1;
    }

    // mov/cmp/add/sub imm
    if (op & 0xE000) == 0x2000 {
        let opc = (op >> 11) & 3;
        let rd = ((op >> 8) & 7) as usize;
        let imm = op & 0xFF;
        match opc {
            0 => {
                cpu.r[rd] = imm;
                cpu.cpsr.set_nz(imm);
            }
            1 => {
                let (r, c) = cpu.r[rd].overflowing_sub(imm);
                cpu.cpsr.set_nz(r);
                cpu.cpsr.c = !c;
                cpu.cpsr.v = ((cpu.r[rd] ^ imm) & (cpu.r[rd] ^ r)) >> 31 != 0;
            }
            2 => {
                let a = cpu.r[rd];
                let (r, c) = a.overflowing_add(imm);
                cpu.r[rd] = r;
                cpu.cpsr.set_nz(r);
                cpu.cpsr.c = c;
                cpu.cpsr.v = (!(a ^ imm) & (a ^ r)) >> 31 != 0;
            }
            3 => {
                let a = cpu.r[rd];
                let (r, c) = a.overflowing_sub(imm);
                cpu.r[rd] = r;
                cpu.cpsr.set_nz(r);
                cpu.cpsr.c = !c;
                cpu.cpsr.v = ((a ^ imm) & (a ^ r)) >> 31 != 0;
            }
            _ => {}
        }
        return 1;
    }

    cpu.note_unknown(op, true);
    1
}

fn cond_ok(cpu: &Cpu, cond: u32) -> bool {
    let n = cpu.cpsr.n;
    let z = cpu.cpsr.z;
    let c = cpu.cpsr.c;
    let v = cpu.cpsr.v;
    match cond {
        0x0 => z,
        0x1 => !z,
        0x2 => c,
        0x3 => !c,
        0x4 => n,
        0x5 => !n,
        0x6 => v,
        0x7 => !v,
        0x8 => c && !z,
        0x9 => !c || z,
        0xA => n == v,
        0xB => n != v,
        0xC => !z && n == v,
        0xD => z || n != v,
        _ => true,
    }
}
