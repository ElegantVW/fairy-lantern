//! Plain-text snapshot next to a `.flst` — machine + host resources.

use crate::emu::Emu;
use crate::ppu;
use anyhow::{Context, Result};
use std::io::Write;
use std::path::Path;

/// Sidecar next to a `.flst` (`stem.dbg.txt`).
pub fn dbg_path_for_state(state: &Path) -> std::path::PathBuf {
    state.with_extension("dbg.txt")
}

pub fn write_report(emu: &Emu, state: &Path) -> Result<std::path::PathBuf> {
    let path = dbg_path_for_state(state);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut f = std::fs::File::create(&path)
        .with_context(|| format!("create {}", path.display()))?;
    write_into(emu, state, &mut f)?;
    let latest = std::env::temp_dir().join("fairy-lantern-state.dbg.txt");
    let _ = std::fs::copy(&path, latest);
    Ok(path)
}

fn write_into(emu: &Emu, state: &Path, f: &mut impl Write) -> Result<()> {
    let now = chrono_stamp();
    writeln!(f, "fairy-lantern debug report")?;
    writeln!(f, "written {now}")?;
    writeln!(f, "version {}", env!("CARGO_PKG_VERSION"))?;
    writeln!(f)?;

    writeln!(f, "== paths ==")?;
    writeln!(f, "state   {}", state.display())?;
    writeln!(
        f,
        "shot    {}",
        crate::video::shot_path_for_state(state).display()
    )?;
    if let Some(p) = emu.rom_path.as_ref() {
        writeln!(f, "rom     {}", p.display())?;
    }
    if let Some(p) = emu.bus.save_path.as_ref() {
        writeln!(f, "battery {} ({})", p.display(), emu.bus.save_type.label())?;
    } else {
        writeln!(f, "battery (none) ({})", emu.bus.save_type.label())?;
    }
    writeln!(f, "title   {}", emu.cart_title)?;
    writeln!(f)?;

    writeln!(f, "== host process ==")?;
    write_host(f)?;
    writeln!(f)?;

    writeln!(f, "== env ==")?;
    for key in [
        "FAIRY_DS",
        "FAIRY_AUDIO",
        "FAIRY_AFFINE_COMPAT",
        "FAIRY_ACCURATE_AFFINE",
        "FAIRY_DMA_TRACE",
        "FAIRY_MIX_STAT",
        "FAIRY_DUMP_IWRAM",
        "FAIRY_OAM_STAT",
        "FAIRY_DEBUG",
    ] {
        match std::env::var(key) {
            Ok(v) => writeln!(f, "{key}={v}")?,
            Err(_) => writeln!(f, "{key}=(unset)")?,
        }
    }
    writeln!(f)?;

    writeln!(f, "== cpu ==")?;
    let cpsr = emu.cpu.cpsr.to_u32();
    writeln!(
        f,
        "pc={:08X} lr={:08X} sp={:08X} cpsr={:08X} {}{}{}{} mode={:02X} {} irq_off={} fiq_off={}",
        emu.cpu.pc(),
        emu.cpu.r[14],
        emu.cpu.r[13],
        cpsr,
        if emu.cpu.cpsr.n { 'N' } else { 'n' },
        if emu.cpu.cpsr.z { 'Z' } else { 'z' },
        if emu.cpu.cpsr.c { 'C' } else { 'c' },
        if emu.cpu.cpsr.v { 'V' } else { 'v' },
        emu.cpu.cpsr.mode,
        if emu.cpu.cpsr.thumb { "thumb" } else { "arm" },
        emu.cpu.cpsr.irq_disable as u8,
        emu.cpu.cpsr.fiq_disable as u8,
    )?;
    write!(f, "r")?;
    for (i, r) in emu.cpu.r.iter().enumerate() {
        write!(f, " {i}={r:08X}")?;
    }
    writeln!(f)?;
    writeln!(
        f,
        "cycles={} halted={} halt_wait={} intr_wait={:04X} unk_ops={} last_unk={:08X}",
        emu.cpu.cycles,
        emu.cpu.halted as u8,
        emu.bus.halt_wait as u8,
        emu.bus.intr_wait_mask,
        emu.cpu.unknown_ops,
        emu.cpu.last_unknown,
    )?;
    writeln!(
        f,
        "irq_count={} swi_unk={} last_swi_unk={:02X} hle_bios={}",
        emu.bus.irq_count,
        emu.bus.swi_unknown,
        emu.bus.last_swi_unknown,
        emu.bus.hle_bios as u8,
    )?;
    write!(f, "swi_nonzero")?;
    let mut any = false;
    for (i, c) in emu.bus.swi_counts.iter().enumerate() {
        if *c != 0 {
            write!(f, " {i:02X}={c}")?;
            any = true;
        }
    }
    if !any {
        write!(f, " (none)")?;
    }
    writeln!(f)?;
    writeln!(f)?;

    writeln!(f, "== irq / keys ==")?;
    writeln!(
        f,
        "ie={:04X} if={:04X} ime={:04X} keyinput={:04X} (0=pressed)",
        emu.bus.read16(0x0400_0200),
        emu.bus.read16(0x0400_0202),
        emu.bus.read16(0x0400_0208),
        emu.bus.keyinput,
    )?;
    writeln!(f)?;

    writeln!(f, "== ppu ==")?;
    let dispcnt = emu.bus.dispcnt();
    writeln!(
        f,
        "dispcnt={dispcnt:04X} mode={} bg0={} bg1={} bg2={} bg3={} obj={} map={} win0={} win1={} objwin={} forced_blank={}",
        dispcnt & 7,
        (dispcnt >> 8) & 1,
        (dispcnt >> 9) & 1,
        (dispcnt >> 10) & 1,
        (dispcnt >> 11) & 1,
        (dispcnt >> 12) & 1,
        if dispcnt & (1 << 6) != 0 { "1D" } else { "2D" },
        (dispcnt >> 13) & 1,
        (dispcnt >> 14) & 1,
        (dispcnt >> 15) & 1,
        (dispcnt >> 7) & 1,
    )?;
    writeln!(
        f,
        "dispstat={:04X} vcount={:04X} line={} line_cycles={}",
        emu.bus.read16(0x0400_0004),
        emu.bus.read16(0x0400_0006),
        emu.ppu.line,
        emu.ppu.line_cycles,
    )?;
    writeln!(
        f,
        "winin={:04X} winout={:04X} win0h={:04X} win0v={:04X} win1h={:04X} win1v={:04X} mosaic={:04X}",
        emu.bus.read16(0x0400_0048),
        emu.bus.read16(0x0400_004A),
        emu.bus.read16(0x0400_0040),
        emu.bus.read16(0x0400_0044),
        emu.bus.read16(0x0400_0042),
        emu.bus.read16(0x0400_0046),
        emu.bus.read16(0x0400_004C),
    )?;
    let bldcnt = emu.bus.read16(0x0400_0050);
    let bldalpha = emu.bus.read16(0x0400_0052);
    let bldy = emu.bus.read16(0x0400_0054);
    writeln!(
        f,
        "bldcnt={bldcnt:04X} effect={} eva={} evb={} evy={}",
        (bldcnt >> 6) & 3,
        bldalpha & 0x1F,
        (bldalpha >> 8) & 0x1F,
        bldy & 0x1F,
    )?;
    for bg in 0..4u32 {
        writeln!(
            f,
            "bg{bg}cnt={:04X} hofs={:04X} vofs={:04X}",
            emu.bus.read16(0x0400_0008 + bg * 2),
            emu.bus.read16(0x0400_0010 + bg * 4),
            emu.bus.read16(0x0400_0012 + bg * 4),
        )?;
    }
    let lit = emu
        .ppu
        .frame
        .iter()
        .filter(|p| *p & 0x7FFF != 0)
        .count();
    writeln!(
        f,
        "frame_lit={lit}/{} affine_compat={}",
        ppu::WIDTH * ppu::HEIGHT,
        crate::cpu::affine_compat() as u8,
    )?;
    writeln!(f)?;

    writeln!(f, "== oam live ==")?;
    let mut live = 0u32;
    for i in 0..128 {
        let o = i * 8;
        let a0 = u16::from_le_bytes([emu.bus.oam[o], emu.bus.oam[o + 1]]);
        let a1 = u16::from_le_bytes([emu.bus.oam[o + 2], emu.bus.oam[o + 3]]);
        let a2 = u16::from_le_bytes([emu.bus.oam[o + 4], emu.bus.oam[o + 5]]);
        let y = a0 & 0xFF;
        let x = a1 & 0x1FF;
        let affine = a0 & (1 << 8) != 0;
        let hide = !affine && a0 & (1 << 9) != 0;
        if hide || (y == 160 && x >= 240) {
            continue;
        }
        live += 1;
        writeln!(
            f,
            "oam{i:02} y={y:3} x={x:3} sh={} sz={} gfx={} af={} mos={} c{} prio={} tile={:03X} pal={}",
            (a0 >> 14) & 3,
            (a1 >> 14) & 3,
            (a0 >> 10) & 3,
            affine as u8,
            (a0 >> 12) & 1,
            if a0 & (1 << 13) != 0 { 8 } else { 4 },
            (a2 >> 10) & 3,
            a2 & 0x3FF,
            (a2 >> 12) & 0xF,
        )?;
    }
    writeln!(f, "oam_live={live}")?;
    writeln!(f)?;

    writeln!(f, "== obj palettes in use ==")?;
    let mut used = [false; 16];
    for i in 0..128 {
        let o = i * 8;
        let a0 = u16::from_le_bytes([emu.bus.oam[o], emu.bus.oam[o + 1]]);
        let a1 = u16::from_le_bytes([emu.bus.oam[o + 2], emu.bus.oam[o + 3]]);
        let a2 = u16::from_le_bytes([emu.bus.oam[o + 4], emu.bus.oam[o + 5]]);
        let y = a0 & 0xFF;
        let x = a1 & 0x1FF;
        let affine = a0 & (1 << 8) != 0;
        let hide = !affine && a0 & (1 << 9) != 0;
        if hide || (y == 160 && x >= 240) {
            continue;
        }
        used[((a2 >> 12) & 0xF) as usize] = true;
    }
    for pal in 0..16 {
        if !used[pal] {
            continue;
        }
        write!(f, "objpal{pal:02}")?;
        for i in 0..16 {
            let off = 0x200 + (pal * 16 + i) * 2;
            let c = u16::from_le_bytes([emu.bus.pal[off], emu.bus.pal[off + 1]]) & 0x7FFF;
            write!(
                f,
                " {i:X}={}/{}/{}",
                c & 0x1F,
                (c >> 5) & 0x1F,
                (c >> 10) & 0x1F
            )?;
        }
        writeln!(f)?;
    }
    writeln!(f)?;

    writeln!(f, "== timers ==")?;
    for i in 0..4 {
        writeln!(
            f,
            "tm{i} cnt={:04X} reload={:04X} counter={:04X} frac={} start_prev={:04X}",
            emu.bus.read16(0x0400_0102 + i as u32 * 4),
            emu.timers.reload[i],
            emu.timers.counter[i],
            emu.timers.frac[i],
            emu.bus.timer_ctrl_prev[i],
        )?;
    }
    writeln!(f, "timer_start_mask={:02X}", emu.bus.timer_start_mask)?;
    writeln!(f)?;

    writeln!(f, "== dma ==")?;
    for (i, ch) in emu.bus.dma.ch.iter().enumerate() {
        writeln!(
            f,
            "dma{i} src={:08X} dst={:08X} latch={:08X}/{:08X} count={} ctrl={:04X} active={}",
            ch.src, ch.dst, ch.latch_src, ch.latch_dst, ch.count, ch.ctrl, ch.active as u8,
        )?;
    }
    writeln!(f, "dma_xfer_count={}", emu.bus.dma.xfer_count)?;
    writeln!(f)?;

    writeln!(f, "== sound ==")?;
    writeln!(
        f,
        "sndcnt_l={:04X} sndcnt_h={:04X} sndcnt_x={:04X} bias={:04X}",
        emu.bus.read16(0x0400_0080),
        emu.bus.read16(0x0400_0082),
        emu.bus.read16(0x0400_0084),
        emu.bus.read16(0x0400_0088),
    )?;
    writeln!(
        f,
        "backend={} rate={} fifoA={} fifoB={} peak={} from_fifo={} from_psg={} out={} ring={} locked={}",
        emu.bus.sound.backend_name(),
        emu.bus.sound.stream_rate,
        emu.bus.sound.fifo_a_len(),
        emu.bus.sound.fifo_b_len(),
        emu.bus.sound.peak_abs(),
        emu.bus.sound.samples_from_fifo,
        emu.bus.sound.samples_from_psg(),
        emu.bus.sound.samples_out,
        emu.bus.sound.ring_frames(),
        emu.bus.sound.fifo_locked() as u8,
    )?;
    writeln!(f)?;

    writeln!(f, "== battery / flash ==")?;
    writeln!(
        f,
        "type={} dirty={} sav_bytes={}",
        emu.bus.save_type.label(),
        emu.bus.save_dirty as u8,
        emu.bus.save_type.size(),
    )?;
    if let Some(ref flash) = emu.bus.flash {
        let (mode, step, bank, man, dev) = flash.debug_fsm();
        writeln!(
            f,
            "flash bytes={} bank={bank} mode={mode} cmd_step={step} id={man:02X}/{dev:02X}",
            flash.data.len(),
        )?;
    }
    if let Ok(g) = emu.bus.eeprom.try_borrow() {
        if let Some(ref e) = *g {
            writeln!(f, "eeprom bytes={} dirty={}", e.data.len(), e.dirty as u8)?;
        }
    }
    writeln!(f, "rtc {} {}", emu.bus.rtc.present as u8, emu.bus.rtc.clock_string())?;
    writeln!(f)?;

    writeln!(f, "== memory occupancy ==")?;
    writeln!(
        f,
        "ewram_nz={} / {}  iwram_nz={} / {}  vram_nz={} / {}  pal_nz={} oam_nz={}",
        nz(&emu.bus.ewram),
        emu.bus.ewram.len(),
        nz(&emu.bus.iwram),
        emu.bus.iwram.len(),
        nz(&emu.bus.vram),
        emu.bus.vram.len(),
        nz(&emu.bus.pal),
        nz(&emu.bus.oam),
    )?;
    writeln!(
        f,
        "lz77v last_dst={:08X} last_size={} to_0600={}",
        emu.bus.last_lz77v_dst, emu.bus.last_lz77v_size, emu.bus.last_lz77v_to_0600
    )?;
    Ok(())
}

fn nz(buf: &[u8]) -> usize {
    buf.iter().filter(|&&b| b != 0).count()
}

fn chrono_stamp() -> String {
    // Local wall time from /proc or libc; fall back to seconds.
    let mut tv = libc::timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    unsafe {
        libc::gettimeofday(&mut tv, std::ptr::null_mut());
    }
    let mut tm = unsafe { std::mem::zeroed::<libc::tm>() };
    unsafe {
        libc::localtime_r(&tv.tv_sec, &mut tm);
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

fn write_host(f: &mut impl Write) -> Result<()> {
    let pid = std::process::id();
    writeln!(f, "pid={pid}")?;

    let mut ru = unsafe { std::mem::zeroed::<libc::rusage>() };
    let ru_ok = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) } == 0;
    if ru_ok {
        let ut = ru.ru_utime.tv_sec as f64 + ru.ru_utime.tv_usec as f64 / 1e6;
        let st = ru.ru_stime.tv_sec as f64 + ru.ru_stime.tv_usec as f64 / 1e6;
        // Linux: ru_maxrss is KiB.
        writeln!(
            f,
            "cpu_user_s={ut:.3} cpu_sys_s={st:.3} cpu_total_s={:.3} maxrss_kib={}",
            ut + st,
            ru.ru_maxrss
        )?;
        writeln!(
            f,
            "minflt={} majflt={} nvcsw={} nivcsw={} inblock={} oublock={}",
            ru.ru_minflt, ru.ru_majflt, ru.ru_nvcsw, ru.ru_nivcsw, ru.ru_inblock, ru.ru_oublock
        )?;
    }

    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for key in [
            "VmSize", "VmRSS", "VmData", "VmStk", "VmExe", "Threads", "FDSize", "voluntary_ctxt_switches",
            "nonvoluntary_ctxt_switches",
        ] {
            if let Some(line) = status.lines().find(|l| l.starts_with(key)) {
                writeln!(f, "{line}")?;
            }
        }
    }

    if let Ok(load) = std::fs::read_to_string("/proc/loadavg") {
        writeln!(f, "loadavg {}", load.trim())?;
    }

    let fds = std::fs::read_dir("/proc/self/fd")
        .map(|d| d.count())
        .unwrap_or(0);
    writeln!(f, "open_fds={fds}")?;

    let children = child_cmds(pid);
    if children.is_empty() {
        writeln!(f, "children (none)")?;
    } else {
        writeln!(f, "children")?;
        for (cpid, comm, rss) in children {
            writeln!(f, "  pid={cpid} comm={comm} rss_kib={rss}")?;
        }
    }
    Ok(())
}

fn child_cmds(ppid: u32) -> Vec<(u32, String, String)> {
    let mut out = Vec::new();
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return out;
    };
    for ent in dir.flatten() {
        let name = ent.file_name();
        let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        if pid == ppid {
            continue;
        }
        let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(s) => s,
            Err(_) => continue,
        };
        // ppid is field 4 after comm in parens
        let Some(end) = stat.rfind(')') else {
            continue;
        };
        let rest: Vec<&str> = stat[end + 2..].split_whitespace().collect();
        // rest[0]=state, rest[1]=ppid
        if rest.get(1).and_then(|s| s.parse::<u32>().ok()) != Some(ppid) {
            continue;
        }
        let comm = std::fs::read_to_string(format!("/proc/{pid}/comm"))
            .unwrap_or_else(|_| "?".into())
            .trim()
            .to_string();
        let rss = std::fs::read_to_string(format!("/proc/{pid}/status"))
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("VmRSS:"))
                    .map(|l| l.split_whitespace().nth(1).unwrap_or("?").to_string())
            })
            .unwrap_or_else(|| "?".into());
        out.push((pid, comm, rss));
    }
    out
}