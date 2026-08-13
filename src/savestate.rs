//! Savestates (.flst) — machine snapshot.
//!
//! `FAELST04` adds FIQ/UND/ABT banks (R8–R14 + SPSR).
//! `FAELST03` still loads (those banks stay at defaults).
//! `FAELST02` still loads (frac/DMA/FIFOs also stay at current values).

use crate::emu::Emu;
use anyhow::{bail, Context, Result};
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

const MAGIC_V2: &[u8; 8] = b"FAELST02";
const MAGIC_V3: &[u8; 8] = b"FAELST03";
const MAGIC_V4: &[u8; 8] = b"FAELST04";

pub fn save(emu: &Emu, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut f = File::create(path).with_context(|| format!("create {}", path.display()))?;
    f.write_all(MAGIC_V4)?;
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
    for frac in &emu.timers.frac {
        f.write_all(&frac.to_le_bytes())?;
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

    f.write_all(&[emu.bus.halt_wait as u8])?;
    f.write_all(&emu.bus.intr_wait_mask.to_le_bytes())?;
    f.write_all(&[emu.bus.timer_start_mask])?;
    for ch in &emu.bus.dma.ch {
        f.write_all(&ch.src.to_le_bytes())?;
        f.write_all(&ch.dst.to_le_bytes())?;
        f.write_all(&ch.latch_src.to_le_bytes())?;
        f.write_all(&ch.latch_dst.to_le_bytes())?;
        f.write_all(&ch.count.to_le_bytes())?;
        f.write_all(&ch.ctrl.to_le_bytes())?;
        f.write_all(&[ch.active as u8])?;
    }
    let snap = emu.bus.sound.snapshot_play();
    write_fifo(&mut f, &snap.fifo_a)?;
    write_fifo(&mut f, &snap.fifo_b)?;
    f.write_all(&[snap.dma_req_a as u8, snap.dma_req_b as u8])?;
    f.write_all(&snap.stream_rate.to_le_bytes())?;
    f.write_all(&snap.samples_out.to_le_bytes())?;
    f.write_all(&snap.samples_from_fifo.to_le_bytes())?;
    f.write_all(&snap.cycle_accum_out.to_le_bytes())?;
    f.write_all(&snap.cycle_accum_psg.to_le_bytes())?;
    f.write_all(&snap.cycle_a.to_le_bytes())?;
    f.write_all(&snap.cycle_b.to_le_bytes())?;
    f.write_all(&snap.cps_a.to_le_bytes())?;
    f.write_all(&snap.cps_b.to_le_bytes())?;
    f.write_all(&snap.cps_out.to_le_bytes())?;
    write_banks_v4(&mut f, &emu.cpu)?;
    Ok(())
}

fn write_banks_v4(f: &mut File, cpu: &crate::cpu::Cpu) -> Result<()> {
    for r in &cpu.r8_usr {
        f.write_all(&r.to_le_bytes())?;
    }
    for r in &cpu.r8_fiq {
        f.write_all(&r.to_le_bytes())?;
    }
    f.write_all(&cpu.r13_fiq.to_le_bytes())?;
    f.write_all(&cpu.r14_fiq.to_le_bytes())?;
    f.write_all(&cpu.spsr_fiq.to_u32().to_le_bytes())?;
    f.write_all(&cpu.r13_abt.to_le_bytes())?;
    f.write_all(&cpu.r14_abt.to_le_bytes())?;
    f.write_all(&cpu.spsr_abt.to_u32().to_le_bytes())?;
    f.write_all(&cpu.r13_und.to_le_bytes())?;
    f.write_all(&cpu.r14_und.to_le_bytes())?;
    f.write_all(&cpu.spsr_und.to_u32().to_le_bytes())?;
    Ok(())
}

fn read_banks_v4(f: &mut File, cpu: &mut crate::cpu::Cpu) -> Result<()> {
    for r in &mut cpu.r8_usr {
        *r = read_u32(f)?;
    }
    for r in &mut cpu.r8_fiq {
        *r = read_u32(f)?;
    }
    cpu.r13_fiq = read_u32(f)?;
    cpu.r14_fiq = read_u32(f)?;
    cpu.spsr_fiq = crate::cpu::Cpsr::from_u32(read_u32(f)?);
    cpu.r13_abt = read_u32(f)?;
    cpu.r14_abt = read_u32(f)?;
    cpu.spsr_abt = crate::cpu::Cpsr::from_u32(read_u32(f)?);
    cpu.r13_und = read_u32(f)?;
    cpu.r14_und = read_u32(f)?;
    cpu.spsr_und = crate::cpu::Cpsr::from_u32(read_u32(f)?);
    Ok(())
}

pub fn load(emu: &mut Emu, path: &Path) -> Result<()> {
    let mut f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut magic = [0u8; 8];
    f.read_exact(&mut magic)?;
    let (v3, v4) = match &magic {
        m if m == MAGIC_V4 => (true, true),
        m if m == MAGIC_V3 => (true, false),
        m if m == MAGIC_V2 => (false, false),
        _ => bail!("not a Fairy Lantern savestate"),
    };
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
    if v3 {
        for frac in &mut emu.timers.frac {
            *frac = read_u32(&mut f)?;
        }
    } else {
        emu.timers.frac = [0; 4];
    }
    emu.bus.timer_reload = emu.timers.reload;
    emu.ppu.line = read_u16(&mut f)?;
    emu.ppu.line_cycles = read_u32(&mut f)?;
    emu.ppu.sync_hblank_from_cycles();
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
    // IO is in place — rebuild timer enable/prescale from the snapshot, not the
    // previous machine (FAELST02 wrote this comment then read IO too early).
    for i in 0..4 {
        emu.bus.timer_ctrl_prev[i] = emu.bus.read16(0x0400_0102 + i as u32 * 4);
    }
    emu.bus.timer_start_mask = 0;

    if v3 {
        let mut hb = [0u8; 1];
        f.read_exact(&mut hb)?;
        emu.bus.halt_wait = hb[0] != 0;
        emu.bus.intr_wait_mask = read_u16(&mut f)?;
        let mut sm = [0u8; 1];
        f.read_exact(&mut sm)?;
        emu.bus.timer_start_mask = sm[0];
        for ch in &mut emu.bus.dma.ch {
            ch.src = read_u32(&mut f)?;
            ch.dst = read_u32(&mut f)?;
            ch.latch_src = read_u32(&mut f)?;
            ch.latch_dst = read_u32(&mut f)?;
            ch.count = read_u32(&mut f)?;
            ch.ctrl = read_u16(&mut f)?;
            let mut a = [0u8; 1];
            f.read_exact(&mut a)?;
            ch.active = a[0] != 0;
        }
        let mut snap = emu.bus.sound.snapshot_play();
        snap.fifo_a = read_fifo(&mut f)?;
        snap.fifo_b = read_fifo(&mut f)?;
        let mut flags = [0u8; 2];
        f.read_exact(&mut flags)?;
        snap.dma_req_a = flags[0] != 0;
        snap.dma_req_b = flags[1] != 0;
        snap.stream_rate = read_u32(&mut f)?;
        snap.samples_out = read_u64(&mut f)?;
        snap.samples_from_fifo = read_u64(&mut f)?;
        snap.cycle_accum_out = read_u32(&mut f)?;
        snap.cycle_accum_psg = read_u32(&mut f)?;
        snap.cycle_a = read_u32(&mut f)?;
        snap.cycle_b = read_u32(&mut f)?;
        snap.cps_a = read_u32(&mut f)?;
        snap.cps_b = read_u32(&mut f)?;
        snap.cps_out = read_u32(&mut f)?;
        emu.bus.sound.restore_play(snap);
        if v4 {
            read_banks_v4(&mut f, &mut emu.cpu)?;
        }
    }

    emu.bus.save_dirty = true; // battery may have changed mid-state
    Ok(())
}

fn write_fifo(f: &mut File, fifo: &crate::sound::Fifo) -> Result<()> {
    let samples = fifo.samples_vec();
    f.write_all(&(samples.len() as u32).to_le_bytes())?;
    let raw: Vec<u8> = samples.iter().map(|s| *s as u8).collect();
    f.write_all(&raw)?;
    f.write_all(&[fifo.hold as u8, fifo.hold_valid as u8, fifo.dma_req as u8])?;
    f.write_all(&fifo.samples_consumed.to_le_bytes())?;
    Ok(())
}

fn read_fifo(f: &mut File) -> Result<crate::sound::Fifo> {
    let n = read_u32(f)? as usize;
    if n > 32 {
        bail!("savestate FIFO length {n} exceeds cap");
    }
    let mut raw = vec![0u8; n];
    f.read_exact(&mut raw)?;
    let samples: Vec<i8> = raw.iter().map(|b| *b as i8).collect();
    let mut meta = [0u8; 3];
    f.read_exact(&mut meta)?;
    let consumed = read_u64(f)?;
    let mut fifo = crate::sound::Fifo::new();
    fifo.restore(&samples, meta[0] as i8, meta[1] != 0, meta[2] != 0, consumed);
    Ok(fifo)
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

fn read_u64(f: &mut File) -> Result<u64> {
    let mut b = [0u8; 8];
    f.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cart::Cart;
    use crate::emu::Emu;

    fn dummy() -> Emu {
        let cart = Cart {
            data: vec![0u8; 0x200],
            title: "t".into(),
            game_code: "T".into(),
            maker: "00".into(),
            path: "m".into(),
            inner_name: None,
        };
        Emu::new(&cart, None)
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("fairy-st-{name}.flst"));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn timer_ctrl_restored_from_saved_io() {
        let mut emu = dummy();
        emu.bus.write16(0x0400_0102, 0x0083);
        emu.bus.timer_ctrl_prev[0] = 0x0083;
        let path = tmp("tmctrl");
        save(&emu, &path).unwrap();

        emu.bus.write16(0x0400_0102, 0);
        emu.bus.timer_ctrl_prev[0] = 0;
        load(&mut emu, &path).unwrap();
        assert_eq!(
            emu.bus.timer_ctrl_prev[0] & 0x80,
            0x80,
            "timer enable must come from snapshot IO, not the live machine"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn frac_halt_dma_fifo_round_trip() {
        let mut emu = dummy();
        emu.timers.frac[2] = 17;
        emu.bus.halt_wait = true;
        emu.bus.intr_wait_mask = 0x0001;
        emu.bus.dma.ch[1].src = 0x0300_1000;
        emu.bus.dma.ch[1].dst = 0x0400_00A0;
        emu.bus.dma.ch[1].count = 4;
        emu.bus.dma.ch[1].ctrl = 0xB600;
        emu.bus.dma.ch[1].active = true;
        emu.bus.sound.push_fifo_a_word(0x0403_0201);
        emu.bus.sound.push_fifo_a_word(0x0807_0605);

        let path = tmp("core");
        save(&emu, &path).unwrap();

        let mut emu2 = dummy();
        load(&mut emu2, &path).unwrap();
        assert_eq!(emu2.timers.frac[2], 17);
        assert!(emu2.bus.halt_wait);
        assert_eq!(emu2.bus.intr_wait_mask, 0x0001);
        assert_eq!(emu2.bus.dma.ch[1].src, 0x0300_1000);
        assert_eq!(emu2.bus.dma.ch[1].count, 4);
        assert!(emu2.bus.dma.ch[1].active);
        assert_eq!(emu2.bus.sound.fifo_a_len(), 8);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn fiq_bank_round_trip() {
        let mut emu = dummy();
        emu.cpu.set_mode(0x11);
        emu.cpu.r[8] = 0xFEED_FACE;
        emu.cpu.r[13] = 0x0300_7F80;
        emu.cpu.set_mode(0x1F); // flush FIQ bank
        let path = tmp("fiq");
        save(&emu, &path).unwrap();
        let mut emu2 = dummy();
        load(&mut emu2, &path).unwrap();
        emu2.cpu.set_mode(0x11);
        assert_eq!(emu2.cpu.r[8], 0xFEED_FACE);
        assert_eq!(emu2.cpu.r[13], 0x0300_7F80);
        let _ = std::fs::remove_file(&path);
    }
}
