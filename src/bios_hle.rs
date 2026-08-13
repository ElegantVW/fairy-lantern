//! High-level BIOS SWI emulation (no real BIOS binary required).

use crate::bus::Bus;
use crate::cpu::Cpu;

/// ARM SWI: GBA BIOS uses the low 8 bits of the 24-bit comment field.
pub fn swi_arm(cpu: &mut Cpu, bus: &mut Bus, op: u32) {
    enter_svc(cpu);
    dispatch(cpu, bus, (op & 0xFF) as u8);
    leave_svc(cpu);
}

/// Thumb SWI: low 8 bits.
pub fn swi_thumb(cpu: &mut Cpu, bus: &mut Bus, op: u32) {
    enter_svc(cpu);
    dispatch(cpu, bus, (op & 0xFF) as u8);
    leave_svc(cpu);
}

/// ARMv4: LR_svc = next insn (PC already advanced), SPSR = CPSR, SVC+ARM+I.
fn enter_svc(cpu: &mut Cpu) {
    let lr = cpu.r[15];
    let spsr = cpu.cpsr;
    cpu.set_mode(0x13);
    cpu.spsr = spsr;
    cpu.cpsr.irq_disable = true;
    cpu.cpsr.thumb = false;
    cpu.cpsr.mode = 0x13;
    cpu.r[14] = lr;
}

/// SoftReset etc. leave SVC themselves — do not undo that.
fn leave_svc(cpu: &mut Cpu) {
    if cpu.cpsr.mode & 0x1F != 0x13 {
        return;
    }
    let ret = cpu.r[14];
    cpu.restore_spsr();
    cpu.r[15] = ret;
}

fn dispatch(cpu: &mut Cpu, bus: &mut Bus, num: u8) {
    bus.swi_counts[num as usize] = bus.swi_counts[num as usize].wrapping_add(1);
    match num {
        0x00 => soft_reset(cpu, bus),
        0x01 => register_ram_reset(cpu, bus),
        0x02 => {
            // Halt — wait for IRQ
            bus.halt_wait = true;
        }
        0x03 => {
            // Stop — treat like Halt (deep sleep not needed for games)
            bus.halt_wait = true;
        }
        0x04 => intr_wait(cpu, bus),
        0x05 => {
            // VBlankIntrWait
            cpu.r[0] = 1;
            cpu.r[1] = 1;
            intr_wait(cpu, bus);
        }
        0x06 => div(cpu),
        0x07 => div_arm(cpu),
        0x08 => {
            cpu.r[0] = isqrt_u32(cpu.r[0]);
        }
        0x09 => arctan(cpu),
        0x0A => arctan2(cpu),
        0x0B => cpu_set(cpu, bus),
        0x0C => cpu_fast_set(cpu, bus),
        0x0D => {
            // GetBiosChecksum — fake a stable value
            cpu.r[0] = 0xBAAE_187F;
        }
        0x0E => {
            // BgAffineSet — r0=src, r1=dst, r2=count (simplified identity)
            bg_affine_set(cpu, bus);
        }
        0x0F => {
            // ObjAffineSet
            obj_affine_set(cpu, bus);
        }
        0x10 => bit_unpack(cpu, bus),
        0x11 => lz77_uncomp(cpu, bus, false),
        0x12 => lz77_uncomp(cpu, bus, true),
        0x13 => huff_uncomp(cpu, bus),
        0x14 => rl_uncomp(cpu, bus, false),
        0x15 => rl_uncomp(cpu, bus, true),
        0x16 => diff8_unfilter(cpu, bus, false),
        0x17 => diff8_unfilter(cpu, bus, true),
        0x18 => diff16_unfilter(cpu, bus),
        0x19 => {
            crate::sound::bios::sound_bias(bus, cpu.r[0]);
        }
        // m4a / SoundDriver + MusicPlayer SWIs (0x1A–0x2B)
        0x1A => crate::sound::bios::sound_driver_init(cpu, bus),
        0x1B => crate::sound::bios::sound_driver_mode(bus, cpu.r[0]),
        0x1C => crate::sound::bios::sound_driver_main(bus),
        0x1D => crate::sound::bios::sound_driver_vsync(bus),
        0x1E => crate::sound::bios::sound_channel_clear(bus),
        0x1F => crate::sound::bios::midi_key_freq(cpu, bus),
        0x20 => crate::sound::bios::music_player_open(bus),
        0x21 => crate::sound::bios::music_player_start(bus),
        0x22 => crate::sound::bios::music_player_stop(bus),
        0x23 => crate::sound::bios::music_player_continue(bus),
        0x24 => crate::sound::bios::music_player_fade_out(bus),
        0x28 => crate::sound::bios::sound_driver_vsync(bus),
        0x29 => crate::sound::bios::sound_driver_vsync(bus),
        0x2A => crate::sound::bios::sound_get_jump_list(cpu),
        0x2B => crate::sound::bios::midi_key_freq(cpu, bus),
        _ => {
            bus.swi_unknown = bus.swi_unknown.wrapping_add(1);
            bus.last_swi_unknown = num;
        }
    }
}

/// SWI 0x08 Sqrt — integer square root of r0 (unsigned), result in r0.
/// Bit-by-bit; `f64` can round a near-integer up and then truncate wrong.
fn isqrt_u32(n: u32) -> u32 {
    if n <= 1 {
        return n;
    }
    let mut rem = n;
    let mut root = 0u32;
    let mut bit = 1u32 << 30;
    while bit > rem {
        bit >>= 2;
    }
    while bit != 0 {
        if rem >= root + bit {
            rem -= root + bit;
            root = (root >> 1) + bit;
        } else {
            root >>= 1;
        }
        bit >>= 2;
    }
    root
}

fn soft_reset(cpu: &mut Cpu, bus: &mut Bus) {
    // GBATEK: read 03007FFA first (it lives inside the 200h wipe).
    let flag = bus.read8(0x0300_7FFA);
    for i in 0x7E00..0x8000 {
        if i < bus.iwram.len() {
            bus.iwram[i] = 0;
        }
    }
    bus.halt_wait = false;
    bus.intr_wait_mask = 0;
    for r in cpu.r.iter_mut().take(13) {
        *r = 0;
    }
    cpu.set_mode(0x13);
    cpu.r[13] = 0x0300_7FE0;
    cpu.set_mode(0x12);
    cpu.r[13] = 0x0300_7FA0;
    cpu.set_mode(0x1F);
    cpu.r[13] = 0x0300_7F00;
    cpu.cpsr.thumb = false;
    cpu.cpsr.irq_disable = false;
    let entry = if flag & 1 != 0 {
        0x0200_0000
    } else {
        0x0800_0000
    };
    cpu.set_pc(entry);
}

fn register_ram_reset(cpu: &mut Cpu, bus: &mut Bus) {
    let flags = cpu.r[0];
    if flags & 0x01 != 0 {
        bus.ewram.fill(0);
    }
    if flags & 0x02 != 0 {
        // Last 200h of IWRAM holds IRQ vector + SoftReset flag — keep it.
        let n = bus.iwram.len().min(0x7E00);
        bus.iwram[..n].fill(0);
    }
    if flags & 0x04 != 0 {
        bus.pal.fill(0);
    }
    if flags & 0x08 != 0 {
        bus.vram.fill(0);
    }
    if flags & 0x10 != 0 {
        bus.oam.fill(0);
    }
    if flags & 0x20 != 0 {
        // clear SIO, sound, timers partially — clear IO range
        for i in 0x60..0xB0 {
            if i < bus.io.len() {
                bus.io[i] = 0;
            }
        }
    }
    if flags & 0x40 != 0 {
        for i in 0x00..0x60 {
            if i < bus.io.len() {
                bus.io[i] = 0;
            }
        }
    }
    // always clear some regs
    if flags & 0x80 != 0 {
        for r in cpu.r.iter_mut().take(12) {
            *r = 0;
        }
    }
}

fn intr_wait(cpu: &mut Cpu, bus: &mut Bus) {
    // r0: 0 = return if already set, 1 = discard current and wait
    // r1: interrupt flags to wait for
    let discard = cpu.r[0] != 0;
    let mask = (cpu.r[1] & 0xFFFF) as u16;
    let mask = if mask == 0 { 1 } else { mask }; // default VBlank
    if discard {
        let if_ = bus.read16(0x0400_0202);
        bus.write16_raw(0x0400_0202, if_ & !mask);
        // Clear BIOS IntrWait mirror for those bits
        let idx = 0x7FF8usize;
        if idx + 1 < bus.iwram.len() {
            let cur = u16::from_le_bytes([bus.iwram[idx], bus.iwram[idx + 1]]);
            let n = cur & !mask;
            let b = n.to_le_bytes();
            bus.iwram[idx] = b[0];
            bus.iwram[idx + 1] = b[1];
        }
    } else {
        // Return immediately if already pending
        let if_ = bus.read16(0x0400_0202);
        let bios_flag = bus.read16(0x0300_7FF8);
        if (if_ & mask) != 0 || (bios_flag & mask) != 0 {
            return;
        }
    }
    bus.intr_wait_mask = mask;
    bus.halt_wait = true;
}

fn div(cpu: &mut Cpu) {
    let num = cpu.r[0] as i32;
    let den = cpu.r[1] as i32;
    if den == 0 {
        cpu.r[0] = 0;
        cpu.r[1] = 0;
        cpu.r[3] = 0;
        return;
    }
    let q = num / den;
    let r = num % den;
    cpu.r[0] = q as u32;
    cpu.r[1] = r as u32;
    cpu.r[3] = q.unsigned_abs();
}

fn div_arm(cpu: &mut Cpu) {
    // r1 / r0 → r0=quot r1=rem r3=abs(quot)  (swapped args vs Div)
    let num = cpu.r[1] as i32;
    let den = cpu.r[0] as i32;
    if den == 0 {
        return;
    }
    let q = num / den;
    let r = num % den;
    cpu.r[0] = q as u32;
    cpu.r[1] = r as u32;
    cpu.r[3] = q.unsigned_abs();
}

fn cpu_set(cpu: &mut Cpu, bus: &mut Bus) {
    let src = cpu.r[0];
    let dst = cpu.r[1];
    let ctrl = cpu.r[2];
    let count = ctrl & 0x001F_FFFF;
    let fixed = ctrl & (1 << 24) != 0;
    let word = ctrl & (1 << 26) != 0;
    let mut s = src;
    let mut d = dst;
    if word {
        for _ in 0..count {
            let v = if fixed {
                bus.read32(src)
            } else {
                let v = bus.read32(s);
                s = s.wrapping_add(4);
                v
            };
            bus.write32(d, v);
            d = d.wrapping_add(4);
        }
    } else {
        for _ in 0..count {
            let v = if fixed {
                bus.read16(src) as u32
            } else {
                let v = bus.read16(s) as u32;
                s = s.wrapping_add(2);
                v
            };
            bus.write16(d, v as u16);
            d = d.wrapping_add(2);
        }
    }
}

fn cpu_fast_set(cpu: &mut Cpu, bus: &mut Bus) {
    let src = cpu.r[0];
    let dst = cpu.r[1];
    let ctrl = cpu.r[2];
    let count = ctrl & 0x001F_FFFF; // 32-bit words
    let fill = ctrl & (1 << 24) != 0;
    let mut s = src;
    let mut d = dst;
    // rounds up to multiple of 8 words in real BIOS; we honor count
    for _ in 0..count {
        let v = if fill {
            bus.read32(src)
        } else {
            let v = bus.read32(s);
            s = s.wrapping_add(4);
            v
        };
        bus.write32(d, v);
        d = d.wrapping_add(4);
    }
}

fn lz77_uncomp(cpu: &mut Cpu, bus: &mut Bus, to_vram: bool) {
    let src = cpu.r[0];
    let dst = cpu.r[1];
    let header = bus.read32(src);
    // GBATEK: bits 0-3 reserved, 4-7 type (1=LZ77 → low byte 0x10), bits 8-31 size
    let size = header >> 8;
    if size == 0 {
        return;
    }
    if to_vram {
        bus.last_lz77v_dst = dst;
        bus.last_lz77v_size = size;
        let off = dst & 0x1_FFFF;
        if (0x500..0x2000).contains(&off) {
            bus.last_lz77v_to_0600 = bus.last_lz77v_to_0600.wrapping_add(1);
        }
    }
    // Real BIOS requires halfword-aligned dest for VRAM variant
    let mut s = src.wrapping_add(4);
    let mut d = if to_vram { dst & !1 } else { dst };
    let mut written = 0u32;
    // VRAM path: keep a sliding window in a side buffer for correct lookbehind.
    // GBA VRAM is 16-bit only; pure RMW lookbehind can mis-handle early streams.
    let use_window = to_vram || (dst >> 24) == 0x06;
    let mut window: Vec<u8> = if use_window {
        Vec::with_capacity(size as usize)
    } else {
        Vec::new()
    };
    while written < size {
        let flags = bus.read8(s);
        s = s.wrapping_add(1);
        for bit in (0..8).rev() {
            if written >= size {
                break;
            }
            if flags & (1 << bit) != 0 {
                // compressed: 2-byte block
                let b0 = bus.read8(s) as u32;
                let b1 = bus.read8(s.wrapping_add(1)) as u32;
                s = s.wrapping_add(2);
                let disp = ((b0 & 0xF) << 8) | b1;
                let n = ((b0 >> 4) + 3) as u32;
                for _ in 0..n {
                    if written >= size {
                        break;
                    }
                    let v = if use_window {
                        let idx = window.len().saturating_sub(disp as usize + 1);
                        window.get(idx).copied().unwrap_or(0)
                    } else {
                        bus.read8(d.wrapping_sub(disp + 1))
                    };
                    if use_window {
                        window.push(v);
                    }
                    write_decomp(bus, d, v, to_vram || (d >> 24) == 0x06);
                    d = d.wrapping_add(1);
                    written += 1;
                }
            } else {
                let v = bus.read8(s);
                s = s.wrapping_add(1);
                if use_window {
                    window.push(v);
                }
                write_decomp(bus, d, v, to_vram || (d >> 24) == 0x06);
                d = d.wrapping_add(1);
                written += 1;
            }
        }
    }
}

fn write_decomp(bus: &mut Bus, addr: u32, val: u8, to_vram: bool) {
    if to_vram || (addr >> 24) == 0x06 {
        // VRAM: 16-bit RMW (hardware rejects pure 8-bit writes)
        let a = addr & !1;
        let cur = bus.read16(a);
        let v = if addr & 1 == 0 {
            (cur & 0xFF00) | val as u16
        } else {
            (cur & 0x00FF) | ((val as u16) << 8)
        };
        let off = crate::bus::vram_index(a);
        if off + 1 < bus.vram.len() {
            let bytes = v.to_le_bytes();
            bus.vram[off] = bytes[0];
            bus.vram[off + 1] = bytes[1];
        }
    } else {
        bus.write8(addr, val);
    }
}

/// Huffman decompress (SWI 0x13). Simplified tree walk; writes bytes to dest.
fn huff_uncomp(cpu: &mut Cpu, bus: &mut Bus) {
    let src = cpu.r[0];
    let dst = cpu.r[1];
    let header = bus.read32(src);
    // Bit 0-3 data bit size (4 or 8), bit 4-7 type=2, bit 8-31 uncompressed size
    let bits = header & 0xF;
    let size = header >> 8;
    if size == 0 || (bits != 4 && bits != 8) {
        return;
    }
    let tree_size = (bus.read8(src.wrapping_add(4)) as u32 + 1) * 2;
    let tree_base = src.wrapping_add(5);
    let mut data_s = src.wrapping_add(4).wrapping_add(tree_size);
    if data_s & 1 != 0 {
        data_s = data_s.wrapping_add(1);
    }
    let mut d = dst;
    let mut written = 0u32;
    let mut bitbuf = 0u32;
    let mut bitcnt = 0u32;
    let units = if bits == 4 { size * 2 } else { size };
    let mut nibble_hi = true;
    let mut out_byte = 0u8;

    for _ in 0..units {
        if written >= size {
            break;
        }
        let mut node = 0u32;
        for _ in 0..64 {
            let node_val = bus.read8(tree_base.wrapping_add(node)) as u32;
            if bitcnt == 0 {
                bitbuf = bus.read32(data_s);
                data_s = data_s.wrapping_add(4);
                bitcnt = 32;
            }
            bitcnt -= 1;
            let bit = (bitbuf >> bitcnt) & 1;
            let offset = (node_val & 0x3F) + 1;
            let next = if bit == 0 {
                node + offset * 2
            } else {
                node + offset * 2 + 1
            };
            let is_leaf = if bit == 0 {
                node_val & 0x80 != 0
            } else {
                node_val & 0x40 != 0
            };
            if is_leaf {
                let symbol = bus.read8(tree_base.wrapping_add(next));
                if bits == 4 {
                    if nibble_hi {
                        out_byte = symbol << 4;
                        nibble_hi = false;
                    } else {
                        out_byte |= symbol & 0xF;
                        bus.write8(d, out_byte);
                        d = d.wrapping_add(1);
                        written += 1;
                        nibble_hi = true;
                    }
                } else {
                    bus.write8(d, symbol);
                    d = d.wrapping_add(1);
                    written += 1;
                }
                break;
            }
            node = next;
        }
    }
}

fn rl_uncomp(cpu: &mut Cpu, bus: &mut Bus, to_vram: bool) {
    let src = cpu.r[0];
    let dst = cpu.r[1];
    let header = bus.read32(src);
    // Same header layout as LZ77: type in low byte, size in bits 8-31
    let size = header >> 8;
    let mut s = src.wrapping_add(4);
    let mut d = dst;
    let mut written = 0u32;
    while written < size {
        let flag = bus.read8(s);
        s = s.wrapping_add(1);
        if flag & 0x80 != 0 {
            let n = (flag & 0x7F) as u32 + 3;
            let b = bus.read8(s);
            s = s.wrapping_add(1);
            for _ in 0..n {
                if written >= size {
                    break;
                }
                write_decomp(bus, d, b, to_vram);
                d = d.wrapping_add(1);
                written += 1;
            }
        } else {
            let n = (flag & 0x7F) as u32 + 1;
            for _ in 0..n {
                if written >= size {
                    break;
                }
                let b = bus.read8(s);
                s = s.wrapping_add(1);
                write_decomp(bus, d, b, to_vram);
                d = d.wrapping_add(1);
                written += 1;
            }
        }
    }
}

/// SWI 0x09 ArcTan — r0 = tan (16.16) → r0 = angle
fn arctan(cpu: &mut Cpu) {
    let t = (cpu.r[0] as i32) as f64 / 65536.0;
    let a = t.atan();
    // Return in BIOS-ish fixed point (approx)
    cpu.r[0] = (a * 65536.0 / std::f64::consts::FRAC_PI_2 * 0x4000 as f64) as i32 as u32;
}

/// SWI 0x0A ArcTan2 — r0=x, r1=y → r0=angle
fn arctan2(cpu: &mut Cpu) {
    let x = (cpu.r[0] as i32) as f64;
    let y = (cpu.r[1] as i32) as f64;
    let a = y.atan2(x);
    cpu.r[0] = (a * 65536.0 / std::f64::consts::PI * 0x8000 as f64) as i32 as u32;
}

/// SWI 0x0E BgAffineSet — src 20B → dst 16B (PA..PD + ref x/y).
/// sx/sy are 8.8 fixed; with float cos∈[-1,1], PA = cos*sx (8.8). Do **not** /256.
fn bg_affine_set(cpu: &mut Cpu, bus: &mut Bus) {
    let mut src = cpu.r[0];
    let mut dst = cpu.r[1];
    let count = cpu.r[2];
    for _ in 0..count {
        // cx,cy: s32 with 8-bit fraction already
        let cx = bus.read32(src) as i32;
        let cy = bus.read32(src.wrapping_add(4)) as i32;
        let disp_x = bus.read16(src.wrapping_add(8)) as i16 as i32;
        let disp_y = bus.read16(src.wrapping_add(10)) as i16 as i32;
        let scale_x = bus.read16(src.wrapping_add(12)) as i16 as i32;
        let scale_y = bus.read16(src.wrapping_add(14)) as i16 as i32;
        let angle = bus.read16(src.wrapping_add(16)) as u32;
        let th = (angle as f64) * std::f64::consts::TAU / 65536.0;
        let (s, c) = (th.sin(), th.cos());
        let pa = clamp_s16(c * scale_x as f64);
        let pb = clamp_s16(-s * scale_x as f64);
        let pc = clamp_s16(s * scale_y as f64);
        let pd = clamp_s16(c * scale_y as f64);
        // ref = center - display_offset · matrix  (8.8 throughout)
        let start_x = cx - disp_x * pa - disp_y * pb;
        let start_y = cy - disp_x * pc - disp_y * pd;
        bus.write16(dst, pa as u16);
        bus.write16(dst.wrapping_add(2), pb as u16);
        bus.write16(dst.wrapping_add(4), pc as u16);
        bus.write16(dst.wrapping_add(6), pd as u16);
        bus.write32(dst.wrapping_add(8), start_x as u32);
        bus.write32(dst.wrapping_add(12), start_y as u32);
        src = src.wrapping_add(20);
        dst = dst.wrapping_add(16);
    }
}

/// SWI 0x0F ObjAffineSet — src {sx,sy,angle} → PA..PD every `offset` bytes in OAM.
fn obj_affine_set(cpu: &mut Cpu, bus: &mut Bus) {
    let mut src = cpu.r[0];
    let mut dst = cpu.r[1];
    let count = cpu.r[2];
    let offset = cpu.r[3].max(2) as u32; // usually 8
    for _ in 0..count {
        let scale_x = bus.read16(src) as i16 as i32;
        let scale_y = bus.read16(src.wrapping_add(2)) as i16 as i32;
        let angle = bus.read16(src.wrapping_add(4)) as u32;
        let th = (angle as f64) * std::f64::consts::TAU / 65536.0;
        let (s, c) = (th.sin(), th.cos());
        // Identity: sx=sy=0x100, angle=0 → PA=PD=0x100 (not 0x1!)
        let pa = clamp_s16(c * scale_x as f64) as u16;
        let pb = clamp_s16(-s * scale_x as f64) as u16;
        let pc = clamp_s16(s * scale_y as f64) as u16;
        let pd = clamp_s16(c * scale_y as f64) as u16;
        bus.write16(dst, pa);
        bus.write16(dst.wrapping_add(offset), pb);
        bus.write16(dst.wrapping_add(offset * 2), pc);
        bus.write16(dst.wrapping_add(offset * 3), pd);
        src = src.wrapping_add(8);
        dst = dst.wrapping_add(offset);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cart::Cart;

    fn harness() -> (Cpu, Bus) {
        let cart = Cart {
            data: vec![0u8; 0x200],
            title: "t".into(),
            game_code: "T".into(),
            maker: "00".into(),
            path: "m".into(),
            inner_name: None,
        };
        (Cpu::new(), Bus::new(&cart, None))
    }

    #[test]
    fn swi_enters_svc_then_returns_to_caller() {
        let (mut cpu, mut bus) = harness();
        cpu.set_mode(0x1F);
        cpu.r[13] = 0x0300_7F00;
        cpu.set_mode(0x13);
        cpu.r[13] = 0x0300_7FE0;
        cpu.set_mode(0x1F);
        cpu.cpsr.thumb = true;
        cpu.r[0] = 200;
        cpu.r[15] = 0x0800_0102;
        swi_thumb(&mut cpu, &mut bus, 0xDF08);
        assert_eq!(cpu.r[0], 14);
        assert_eq!(cpu.cpsr.mode & 0x1F, 0x1F, "back in SYS");
        assert!(cpu.cpsr.thumb, "T bit restored from SPSR");
        assert_eq!(cpu.r[15], 0x0800_0102, "PC is next insn");
        assert_eq!(cpu.r[13], 0x0300_7F00, "user SP intact");
        cpu.set_mode(0x13);
        assert_eq!(cpu.r[13], 0x0300_7FE0, "SVC SP not smashed");
    }

    #[test]
    fn sqrt_is_integer_floor() {
        assert_eq!(isqrt_u32(0), 0);
        assert_eq!(isqrt_u32(1), 1);
        assert_eq!(isqrt_u32(3), 1);
        assert_eq!(isqrt_u32(4), 2);
        assert_eq!(isqrt_u32(10), 3);
        assert_eq!(isqrt_u32(0xFFFF), 0xFF);
        assert_eq!(isqrt_u32(0x1_0000), 0x100);
        assert_eq!(isqrt_u32(u32::MAX), 0xFFFF);
        let (mut cpu, mut bus) = harness();
        cpu.r[0] = 200;
        super::dispatch(&mut cpu, &mut bus, 0x08);
        assert_eq!(cpu.r[0], 14);
    }

    #[test]
    fn ram_reset_keeps_irq_vector() {
        let (mut cpu, mut bus) = harness();
        bus.write32(0x0300_7FFC, 0x0800_1234);
        bus.write8(0x0300_1000, 0xAB);
        cpu.r[0] = 0x02;
        register_ram_reset(&mut cpu, &mut bus);
        assert_eq!(bus.read8(0x0300_1000), 0);
        assert_eq!(bus.read32(0x0300_7FFC), 0x0800_1234);
    }

    #[test]
    fn soft_reset_honors_ram_boot_flag() {
        let (mut cpu, mut bus) = harness();
        bus.write8(0x0300_7FFA, 1);
        cpu.r[0] = 0xDEAD;
        soft_reset(&mut cpu, &mut bus);
        assert_eq!(cpu.pc(), 0x0200_0000);
        assert_eq!(cpu.r[0], 0);
        assert_eq!(cpu.cpsr.mode & 0x1F, 0x1F);
        assert_eq!(cpu.r[13], 0x0300_7F00);
        assert_eq!(bus.read8(0x0300_7FFA), 0, "flag region is wiped");
    }
}

#[inline]
fn clamp_s16(v: f64) -> i32 {
    v.round().clamp(-32768.0, 32767.0) as i32
}

// Sound driver SWIs are now implemented in src/sound/bios.rs

/// SWI 0x10 BitUnPack — expand packed source bits into dest units.
/// r0=src, r1=dest, r2=ptr to {u16 src_len, u8 src_width, u8 dest_width, u32 data_offset}
fn bit_unpack(cpu: &mut Cpu, bus: &mut Bus) {
    let src = cpu.r[0];
    let dst = cpu.r[1];
    let info = cpu.r[2];
    let src_len = bus.read16(info) as u32;
    let src_width = bus.read8(info.wrapping_add(2)).max(1) as u32;
    let dest_width = bus.read8(info.wrapping_add(3)).max(1) as u32;
    let data_offset = bus.read32(info.wrapping_add(4));
    let offset_val = data_offset & 0x7FFF_FFFF;
    let zero_flag = data_offset & 0x8000_0000 != 0;

    if src_width > 32 || dest_width > 32 || src_len == 0 {
        return;
    }
    let mut s = src;
    let mut d = dst;
    let mut bitbuf = 0u32;
    let mut bitcnt = 0u32;
    let mut outbuf = 0u32;
    let mut outcnt = 0u32;
    let units = (src_len * 8) / src_width;
    for _ in 0..units {
        // pull src_width bits (LSB first within each source byte stream)
        while bitcnt < src_width {
            bitbuf |= (bus.read8(s) as u32) << bitcnt;
            s = s.wrapping_add(1);
            bitcnt += 8;
        }
        let mut unit = bitbuf & ((1u32 << src_width) - 1);
        bitbuf >>= src_width;
        bitcnt -= src_width;

        if unit != 0 || zero_flag {
            unit = unit.wrapping_add(offset_val);
        }
        if dest_width < 32 {
            unit &= (1u32 << dest_width) - 1;
        }

        outbuf |= unit << outcnt;
        outcnt += dest_width;
        while outcnt >= 8 {
            let b = (outbuf & 0xFF) as u8;
            write_decomp(bus, d, b, (d >> 24) == 0x06);
            d = d.wrapping_add(1);
            outbuf >>= 8;
            outcnt -= 8;
        }
    }
    if outcnt > 0 {
        write_decomp(bus, d, (outbuf & 0xFF) as u8, (d >> 24) == 0x06);
    }
}

/// SWI 0x16 / 0x17 Diff8bit UnFilter (WRAM / VRAM dest).
fn diff8_unfilter(cpu: &mut Cpu, bus: &mut Bus, to_vram: bool) {
    let src = cpu.r[0];
    let dst = cpu.r[1];
    let header = bus.read32(src);
    let size = header >> 8;
    if size == 0 {
        return;
    }
    let mut s = src.wrapping_add(4);
    let mut d = dst;
    let mut acc = 0u8;
    for _ in 0..size {
        acc = acc.wrapping_add(bus.read8(s));
        s = s.wrapping_add(1);
        write_decomp(bus, d, acc, to_vram || (d >> 24) == 0x06);
        d = d.wrapping_add(1);
    }
}

/// SWI 0x18 Diff16bit UnFilter (dest halfword-aligned, typically VRAM).
fn diff16_unfilter(cpu: &mut Cpu, bus: &mut Bus) {
    let src = cpu.r[0];
    let dst = cpu.r[1];
    let header = bus.read32(src);
    let size = header >> 8; // bytes
    if size == 0 {
        return;
    }
    let mut s = src.wrapping_add(4);
    let mut d = dst & !1;
    let mut acc = 0u16;
    let words = size / 2;
    for _ in 0..words {
        let delta = bus.read16(s);
        s = s.wrapping_add(2);
        acc = acc.wrapping_add(delta);
        bus.write16(d, acc);
        d = d.wrapping_add(2);
    }
}
