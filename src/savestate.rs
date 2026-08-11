//! Simple savestates (.flst) — full machine snapshot.

use crate::emu::Emu;
use anyhow::{bail, Context, Result};
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

const MAGIC: &[u8; 8] = b"FAELST02";

pub fn save(emu: &Emu, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut f = File::create(path).with_context(|| format!("create {}", path.display()))?;
    f.write_all(MAGIC)?;
    // CPU
    for r in &emu.cpu.r {
        f.write_all(&r.to_le_bytes())?;
    }
    f.write_all(&emu.cpu.cpsr.to_u32().to_le_bytes())?;
    f.write_all(&emu.cpu.spsr.to_u32().to_le_bytes())?;
    f.write_all(&emu.cpu.cycles.to_le_bytes())?;
    f.write_all(&[emu.cpu.halted as u8])?;
    // Banked R13/R14 + SPSR (v2)
    f.write_all(&emu.cpu.r13_usr.to_le_bytes())?;
    f.write_all(&emu.cpu.r14_usr.to_le_bytes())?;
    f.write_all(&emu.cpu.r13_irq.to_le_bytes())?;
    f.write_all(&emu.cpu.r14_irq.to_le_bytes())?;
    f.write_all(&emu.cpu.spsr_irq.to_u32().to_le_bytes())?;
    f.write_all(&emu.cpu.r13_svc.to_le_bytes())?;
    f.write_all(&emu.cpu.r14_svc.to_le_bytes())?;
    f.write_all(&emu.cpu.spsr_svc.to_u32().to_le_bytes())?;
    // timers
    for c in &emu.timers.counter {
        f.write_all(&c.to_le_bytes())?;
    }
    for r in &emu.timers.reload {
        f.write_all(&r.to_le_bytes())?;
    }
    // ppu
    f.write_all(&emu.ppu.line.to_le_bytes())?;
    f.write_all(&emu.ppu.line_cycles.to_le_bytes())?;
    // memory blobs
    write_blob(&mut f, &emu.bus.ewram)?;
    write_blob(&mut f, &emu.bus.iwram)?;
    write_blob(&mut f, &emu.bus.io)?;
    write_blob(&mut f, &emu.bus.pal)?;
    write_blob(&mut f, &emu.bus.vram)?;
    write_blob(&mut f, &emu.bus.oam)?;
    write_blob(&mut f, &emu.bus.sram)?;
    if let Some(ref flash) = emu.bus.flash {
        f.write_all(&[1u8])?;
        write_blob(&mut f, &flash.data)?;
        f.write_all(&(flash.bank as u32).to_le_bytes())?;
    } else {
        f.write_all(&[0u8])?;
    }
    // frame buffer (optional continuity)
    for p in emu.ppu.frame.iter() {
        f.write_all(&p.to_le_bytes())?;
    }
    Ok(())
}

pub fn load(emu: &mut Emu, path: &Path) -> Result<()> {
    let mut f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut magic = [0u8; 8];
    f.read_exact(&mut magic)?;
    if &magic != MAGIC {
        bail!("not a Fairy Lantern savestate");
    }
    for r in &mut emu.cpu.r {
        *r = read_u32(&mut f)?;
    }
    emu.cpu.cpsr = crate::cpu::Cpsr::from_u32(read_u32(&mut f)?);
    emu.cpu.spsr = crate::cpu::Cpsr::from_u32(read_u32(&mut f)?);
    emu.cpu.cycles = {
        let mut b = [0u8; 8];
        f.read_exact(&mut b)?;
        u64::from_le_bytes(b)
    };
    let mut h = [0u8; 1];
    f.read_exact(&mut h)?;
    emu.cpu.halted = h[0] != 0;
    emu.cpu.r13_usr = read_u32(&mut f)?;
    emu.cpu.r14_usr = read_u32(&mut f)?;
    emu.cpu.r13_irq = read_u32(&mut f)?;
    emu.cpu.r14_irq = read_u32(&mut f)?;
    emu.cpu.spsr_irq = crate::cpu::Cpsr::from_u32(read_u32(&mut f)?);
    emu.cpu.r13_svc = read_u32(&mut f)?;
    emu.cpu.r14_svc = read_u32(&mut f)?;
    emu.cpu.spsr_svc = crate::cpu::Cpsr::from_u32(read_u32(&mut f)?);
    for c in &mut emu.timers.counter {
        *c = read_u32(&mut f)?;
    }
    for r in &mut emu.timers.reload {
        *r = read_u16(&mut f)?;
    }
    emu.bus.timer_reload = emu.timers.reload;
    // Rebuild the timer ctrl shadow from the restored IO block.
    for i in 0..4 {
        emu.bus.timer_ctrl_prev[i] = emu.bus.read16(0x0400_0102 + i as u32 * 4);
    }
    emu.ppu.line = read_u16(&mut f)?;
    emu.ppu.line_cycles = read_u32(&mut f)?;
    emu.bus.ewram = read_blob(&mut f)?;
    emu.bus.iwram = read_blob(&mut f)?;
    emu.bus.io = read_blob(&mut f)?;
    emu.bus.pal = read_blob(&mut f)?;
    emu.bus.vram = read_blob(&mut f)?;
    emu.bus.oam = read_blob(&mut f)?;
    emu.bus.sram = read_blob(&mut f)?;
    let mut has_flash = [0u8; 1];
    f.read_exact(&mut has_flash)?;
    if has_flash[0] != 0 {
        let data = read_blob(&mut f)?;
        let bank = read_u32(&mut f)? as usize;
        if let Some(ref mut flash) = emu.bus.flash {
            flash.data = data;
            flash.bank = bank;
        }
    }
    for p in emu.ppu.frame.iter_mut() {
        *p = read_u16(&mut f)?;
    }
    emu.bus.save_dirty = true; // battery may have changed mid-state
    Ok(())
}

fn write_blob(f: &mut File, data: &[u8]) -> Result<()> {
    f.write_all(&(data.len() as u32).to_le_bytes())?;
    f.write_all(data)?;
    Ok(())
}

fn read_blob(f: &mut File) -> Result<Vec<u8>> {
    let n = read_u32(f)? as usize;
    let mut buf = vec![0u8; n];
    f.read_exact(&mut buf)?;
    Ok(buf)
}

fn read_u32(f: &mut File) -> Result<u32> {
    let mut b = [0u8; 4];
    f.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn read_u16(f: &mut File) -> Result<u16> {
    let mut b = [0u8; 2];
    f.read_exact(&mut b)?;
    Ok(u16::from_le_bytes(b))
}
