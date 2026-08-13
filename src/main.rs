//! Fairy Lantern — light a fable; play a pocket world (GBA).

mod battery;
mod bios_hle;
mod bus;
mod cart;
mod cpu;
mod dma;
mod emu;
mod fable;
mod irq;
mod play;
mod ppu;
mod recents;
mod rtc;
mod savestate;
mod statedbg;
mod sound;
mod timers;
mod tui;
mod video;

use anyhow::{Context, Result};
use cart::Cart;
use clap::{Parser, Subcommand};
use emu::Emu;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "fairy-lantern",
    about = "Fairy Lantern — GBA emulator from scratch (faeOS)",
    long_about = "Light a fable; play a pocket world.\n\
                  Bare `fairy` / `fairy-lantern` opens the home TUI.\n\
                  From-scratch ARM7TDMI + PPU. No mGBA/libretro."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Commands>,

    /// Fable (.gba) when no subcommand — opens play window
    rom: Option<PathBuf>,

    /// Headless: run N frames then dump (default window when omitted)
    #[arg(long)]
    frames: Option<u32>,

    #[arg(long)]
    dump: Option<PathBuf>,

    #[arg(long)]
    present: bool,

    #[arg(long)]
    bios: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// ROM header
    Info { rom: PathBuf },
    /// Self-tests
    Test,
    /// Diagnose a ROM (PC path + DISPCNT)
    Diagnose {
        rom: std::path::PathBuf,
        #[arg(long, default_value_t = 50000)]
        steps: u64,
    },
    /// Debug spark ROM stepping
    DebugSpark {
        #[arg(long, default_value_t = 50)]
        steps: u32,
    },
    /// Play a fable (window)
    Play {
        rom: Option<PathBuf>,
        #[arg(long)]
        bios: Option<PathBuf>,
    },
    /// Built-in SPARK fable (always playable)
    Spark {
        #[arg(long)]
        bios: Option<PathBuf>,
    },
    /// Re-open the last fable
    Last {
        #[arg(long)]
        bios: Option<PathBuf>,
    },
    /// Headless run
    Run {
        rom: PathBuf,
        #[arg(long, default_value_t = 3)]
        frames: u32,
        #[arg(long)]
        dump: Option<PathBuf>,
        #[arg(long)]
        present: bool,
        #[arg(long)]
        bios: Option<PathBuf>,
        /// Pulse Start+A after boot (advance title → menu on commercial ROMs)
        #[arg(long)]
        auto_input: bool,
        /// Load the ROM's `.flst` before running
        #[arg(long)]
        load_state: bool,
        /// Write `.flst` + shot + debug report after the run
        #[arg(long)]
        save_state: bool,
    },
    /// Home TUI (same as bare command)
    Tui {
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// 440 Hz sine — default uses the in-game ring/resampler; --direct is raw 48 kHz
    Tone {
        #[arg(long, default_value_t = 3.0)]
        seconds: f32,
        /// Skip the ring (48 kHz straight into pw-cat)
        #[arg(long)]
        direct: bool,
    },
}

fn main() {
    if let Err(e) = real_main() {
        eprintln!("fairy-lantern: {e:#}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Some(Commands::Info { rom }) => {
            cart::print_info(&Cart::load(&rom)?);
        }
        Some(Commands::DebugSpark { steps }) => {
            debug_spark(steps);
        }
        Some(Commands::Diagnose { rom, steps }) => {
            diagnose_rom(&rom, steps)?;
        }
        Some(Commands::Test) => {
            let n = run_self_tests();
            println!("✦ Fairy Lantern self-tests: {n} passed");
        }
        Some(Commands::Spark { bios }) => {
            play_spark(bios.as_ref())?;
        }
        Some(Commands::Last { bios }) => {
            play_last(bios.as_ref())?;
        }
        Some(Commands::Play { rom, bios }) => {
            if let Some(rom) = rom {
                play_rom(&rom, bios.as_ref())?;
            } else {
                run_home_tui(bios.as_ref())?;
            }
        }
        Some(Commands::Run {
            rom,
            frames,
            dump,
            present,
            bios,
            auto_input,
            load_state,
            save_state,
        }) => {
            run_rom(
                &rom,
                frames,
                dump.as_ref(),
                present,
                bios.as_ref(),
                auto_input,
                load_state,
                save_state,
            )?;
        }
        Some(Commands::Tui { dir: _ }) => {
            run_home_tui(None)?;
        }
        Some(Commands::Tone { seconds, direct }) => {
            if direct {
                crate::sound::HostAudio::play_tone_direct(seconds);
            } else {
                crate::sound::HostAudio::play_tone(seconds);
            }
        }
        None => {
            if let Some(rom) = cli.rom {
                if let Some(frames) = cli.frames {
                    run_rom(
                        &rom,
                        frames,
                        cli.dump.as_ref(),
                        cli.present,
                        cli.bios.as_ref(),
                        false,
                        false,
                        false,
                    )?;
                } else {
                    play_rom(&rom, cli.bios.as_ref())?;
                }
            } else {
                // bare `fairy` / `fairy-lantern` → home TUI
                run_home_tui(cli.bios.as_ref())?;
            }
        }
    }
    Ok(())
}

fn run_home_tui(bios: Option<&PathBuf>) -> Result<()> {
    match tui::run_home()? {
        tui::Choice::Quit => Ok(()),
        tui::Choice::Spark => play_spark(bios),
        tui::Choice::Rom(p) => play_rom(&p, bios),
    }
}

fn play_last(bios: Option<&PathBuf>) -> Result<()> {
    match recents::last_rom() {
        Some(p) => play_rom(&p, bios),
        None => {
            eprintln!("fairy-lantern: no last fable yet — open one from the TUI or:");
            eprintln!("  fairy-lantern play game.gba");
            eprintln!("  fairy-lantern spark");
            anyhow::bail!("no last fable")
        }
    }
}

fn play_spark(bios: Option<&PathBuf>) -> Result<()> {
    let cart = fable::spark_rom();
    cart::print_info(&cart);
    let mut emu = Emu::from_cart(cart, bios.map(|p| p.as_path()));
    play::run_window(&mut emu, "SPARK (built-in)")
}

fn play_rom(rom: &PathBuf, bios: Option<&PathBuf>) -> Result<()> {
    let cart = Cart::load(rom)?;
    cart::print_info(&cart);
    // remember for "last" / home TUI
    if let Err(e) = recents::remember(rom) {
        eprintln!("fairy-lantern: could not save recents ({e})");
    }
    let mut emu = Emu::from_cart(cart, bios.map(|p| p.as_path()));
    emu.attach_rom_path(rom);
    let title = if emu.cart_title.is_empty() {
        rom.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("fable")
            .to_string()
    } else {
        emu.cart_title.clone()
    };
    play::run_window(&mut emu, &title)
}

fn run_rom(
    rom: &PathBuf,
    frames: u32,
    dump: Option<&PathBuf>,
    present: bool,
    bios: Option<&PathBuf>,
    auto_input: bool,
    load_state: bool,
    save_state: bool,
) -> Result<()> {
    let cart = Cart::load(rom)?;
    cart::print_info(&cart);
    println!("  lighting lantern for {frames} frame(s)…");
    if auto_input {
        println!("  auto-input: Start+A pulses after boot (title advance)");
    }
    let mut emu = Emu::from_cart(cart, bios.map(|p| p.as_path()));
    emu.attach_rom_path(rom);
    if load_state {
        let Some(path) = emu.state_path() else {
            anyhow::bail!("no savestate path for this ROM");
        };
        savestate::load(&mut emu, &path)?;
        println!("  loaded state ← {}", path.display());
    }
    if load_state || save_state {
        emu.bus.sound.start_host();
    }
    let ai = if auto_input {
        Some(emu::AutoInput::title_advance())
    } else {
        None
    };
    let n = emu.run_frames_with_input(frames.max(1), ai);
    let vram_nz = emu.bus.vram.iter().filter(|&&b| b != 0).count();
    let lit = emu.ppu.frame.iter().filter(|&&p| p & 0x7FFF != 0).count();
    println!(
        "  burned {n} frame(s) · cycles {} · pc=0x{:08X} · irqs={} · dispcnt={:04X} · vram_nz={} · lit_px={} · unk_ops={} · swi_unk={} · fifo={}",
        emu.cpu.cycles,
        emu.cpu.pc(),
        emu.bus.irq_count,
        emu.bus.dispcnt(),
        vram_nz,
        lit,
        emu.cpu.unknown_ops,
        emu.bus.swi_unknown,
        emu.bus.sound.samples_out,
    );
    println!(
        "  audio_dbg: peak={} from_fifo={} from_psg={} fifoA={} fifoB={} game_rate={}Hz mix_rate={}Hz out={}",
        emu.bus.sound.peak_abs(),
        emu.bus.sound.samples_from_fifo,
        emu.bus.sound.samples_from_psg(),
        emu.bus.sound.fifo_a_len(),
        emu.bus.sound.fifo_b_len(),
        emu.bus.sound.stream_rate,
        emu.bus.sound.stream_rate,
        emu.bus.sound.samples_out,
    );
    {
        let sndh = emu.bus.read16(0x0400_0082);
        let sndx = emu.bus.read16(0x0400_0084);
        let t0 = emu.bus.timer_reload[0];
        let t1 = emu.bus.timer_reload[1];
        let t0c = emu.bus.read16(0x0400_0102);
        let t1c = emu.bus.read16(0x0400_0106);
        let dma1_sad = emu.bus.read32(0x0400_00BC);
        let dma2_sad = emu.bus.read32(0x0400_00C8);
        let dma1_ctl = emu.bus.read16(0x0400_00C6);
        let dma2_ctl = emu.bus.read16(0x0400_00D2);
        let src1 = emu.bus.dma.ch[1].src;
        let src2 = emu.bus.dma.ch[2].src;
        println!(
            "  audio_hw: sndh={sndh:04X} sndx={sndx:04X} t0={t0:04X}/{t0c:04X} t1={t1:04X}/{t1c:04X} \
             dma1={dma1_sad:08X}/{src1:08X} ctl={dma1_ctl:04X} dma2={dma2_sad:08X}/{src2:08X} ctl={dma2_ctl:04X}"
        );
        let mut si = String::new();
        for i in 0..12 {
            si.push_str(&format!(" {:08X}", emu.bus.read32(0x0300_5F50 + i * 4)));
        }
        println!("  soundinfo:{si}");
        let dump_at = |label: &str, addr: u32| {
            let mut s = format!("  {label} @{addr:08X}:");
            for i in 0..16 {
                s.push_str(&format!(" {:02X}", emu.bus.read8(addr.wrapping_add(i))));
            }
            println!("{s}");
        };
        dump_at("mix", 0x0300_62A0);
        if src1 >= 0x0200_0000 && src1 < 0x0400_0000 {
            dump_at("dma1src", src1);
        }
        if src2 >= 0x0200_0000 && src2 < 0x0400_0000 {
            dump_at("dma2src", src2);
        }
        let tmp = std::env::temp_dir();
        match emu.bus.sound.dump_fifo_traces(&tmp) {
            Ok(()) => {
                let (ta, tb) = emu.bus.sound.fifo_trace();
                println!(
                    "  fifo_trace: A={} B={} → {}/fairy-fifo-{{a,b,ab}}.wav (ab is A=L B=R)",
                    ta.len(),
                    tb.len(),
                    tmp.display()
                );
            }
            Err(e) => eprintln!("  fifo_trace dump failed: {e}"),
        }
    }
    // Always leave a diagnostic capture for `aplay /tmp/fairy-lantern-audio.wav`
    {
        let wav = std::env::temp_dir().join("fairy-lantern-audio.wav");
        match emu.bus.sound.dump_wav(&wav) {
            Ok(()) => eprintln!(
                "  audio: wrote {} (48 kHz stereo)",
                wav.display()
            ),
            Err(e) => eprintln!("  audio: wav dump failed: {e}"),
        }
    }
    if emu.cpu.unknown_ops > 0 {
        println!("  last_unknown_op={:08X}", emu.cpu.last_unknown);
    }
    if emu.bus.swi_unknown > 0 {
        println!("  last_swi_unknown={:02X}", emu.bus.last_swi_unknown);
    }
    // Frequent BIOS decompress SWIs (helps diagnose missing graphics)
    let lz_w = emu.bus.swi_counts[0x11];
    let lz_v = emu.bus.swi_counts[0x12];
    let rl_w = emu.bus.swi_counts[0x14];
    let rl_v = emu.bus.swi_counts[0x15];
    if lz_w | lz_v | rl_w | rl_v != 0 {
        println!("  swi: LZ77W={lz_w} LZ77V={lz_v} RLW={rl_w} RLV={rl_v}");
    }
    // Sound BIOS SWI calls
    let init_c = emu.bus.swi_counts[0x1A];
    let main_c = emu.bus.swi_counts[0x1C];
    let vsync_c = emu.bus.swi_counts[0x1D];
    let mkf_c = emu.bus.swi_counts[0x1F] + emu.bus.swi_counts[0x2B];
    let music_c = emu.bus.swi_counts[0x20] + emu.bus.swi_counts[0x21] + emu.bus.swi_counts[0x22] + emu.bus.swi_counts[0x23] + emu.bus.swi_counts[0x24];
    if init_c | main_c | vsync_c | mkf_c | music_c != 0 {
        println!("  swi_sound: Init={init_c} Mode={m} Main={main_c} VSync={vsync_c} Clear={c} Midi2Freq={mkf_c} Music={music_c}",
            m=emu.bus.swi_counts[0x1B],
            c=emu.bus.swi_counts[0x1E],
        );
    }
    if std::env::var_os("FAIRY_DEBUG").is_some() {
        let b = &emu.bus;
        print!("  bg:");
        for bg in 0..4u32 {
            let cnt = b.read16(0x0400_0008 + bg * 2);
            let hofs = b.read16(0x0400_0010 + bg * 4);
            let vofs = b.read16(0x0400_0012 + bg * 4);
            print!(" [{bg}]cnt={cnt:04X} h={hofs:03X} v={vofs:03X}");
        }
        println!();
        println!(
            "  win: in={:04X} out={:04X} bld={:04X} mosaic={:04X}",
            b.read16(0x0400_0048),
            b.read16(0x0400_004A),
            b.read16(0x0400_0050),
            b.read16(0x0400_004C),
        );
        // Affine BG2 params
        println!(
            "  bg2aff: pa={:04X} pb={:04X} pc={:04X} pd={:04X} x={:08X} y={:08X}",
            b.read16(0x0400_0020),
            b.read16(0x0400_0022),
            b.read16(0x0400_0024),
            b.read16(0x0400_0026),
            b.read32(0x0400_0028),
            b.read32(0x0400_002C),
        );
        // Active OAM entries
        let mut n_obj = 0u32;
        for i in 0..128 {
            let o = i * 8;
            let a0 = b.read16(0x0700_0000 + o as u32);
            let a1 = b.read16(0x0700_0000 + o as u32 + 2);
            let a2 = b.read16(0x0700_0000 + o as u32 + 4);
            let aff = a0 & (1 << 8) != 0;
            let dis = a0 & (1 << 9) != 0;
            if !aff && dis {
                continue;
            }
            let gfx = (a0 >> 10) & 3;
            if gfx == 2 {
                continue;
            }
            let shape = (a0 >> 14) & 3;
            let size = (a1 >> 14) & 3;
            let y = a0 & 0xFF;
            let x = a1 & 0x1FF;
            let tile = a2 & 0x3FF;
            let pal = (a2 >> 12) & 0xF;
            let prio = (a2 >> 10) & 3;
            let c256 = a0 & (1 << 13) != 0;
            let ap = (a1 >> 9) & 0x1F;
            if n_obj < 16 {
                println!(
                    "  obj{i}: y={y} x={x} sh={shape} sz={size} tile={tile} pal={pal} prio={prio} aff={aff} 256={c256} gfx={gfx} ap={ap} a0={a0:04X} a1={a1:04X} a2={a2:04X}"
                );
            }
            n_obj += 1;
        }
        println!("  active_objs={n_obj}");
        // Affine param 0
        let pa = b.read16(0x0700_0006);
        let pb = b.read16(0x0700_000E);
        let pc = b.read16(0x0700_0016);
        let pd = b.read16(0x0700_001E);
        println!("  oam_aff0: pa={pa:04X} pb={pb:04X} pc={pc:04X} pd={pd:04X}");

        // BG2 affine map sample (Oak trainer pic lives here in mode 1)
        let scr = ((b.read16(0x0400_000C) >> 8) & 0x1F) as usize * 0x800;
        let mut nz = 0u32;
        let mut samples = Vec::new();
        for i in 0..32*32 {
            let t = b.vram.get(scr + i).copied().unwrap_or(0);
            if t != 0 {
                nz += 1;
                if samples.len() < 8 { samples.push((i, t)); }
            }
        }
        let mut tnz = 0u32;
        let mut tnz_full = 0u32;
        for i in 0..64*8 {
            if b.vram.get(0x600 + i).copied().unwrap_or(0) != 0 { tnz += 1; }
        }
        for i in 0..0x1800 {
            if b.vram.get(0x600 + i).copied().unwrap_or(0) != 0 { tnz_full += 1; }
        }
        // also sample raw bytes at 0x600
        let sample: Vec<u8> = (0..16).map(|i| b.vram.get(0x600+i).copied().unwrap_or(0)).collect();
        println!("  bg2map@{:X} nz={}/1024 samples={:?} vram600_8tiles_nz={} oakrange_nz={}/0x1800 head={:02X?}", scr, nz, samples, tnz, tnz_full, sample);
        println!(
            "  last_lz77v dst={:08X} size={:X} hits_0600={}",
            b.last_lz77v_dst, b.last_lz77v_size, b.last_lz77v_to_0600
        );

    }
    let dump_path = dump
        .cloned()
        .unwrap_or_else(|| std::env::temp_dir().join("fairy-lantern-last.ppm"));
    video::write_ppm(&dump_path, &emu.ppu.frame)
        .with_context(|| format!("dump {}", dump_path.display()))?;
    println!("  frame → {}", dump_path.display());
    if present && !video::present_terminal(&emu.ppu.frame) {
        println!("  (chafa unavailable)");
    }
    if save_state {
        let Some(path) = emu.state_path() else {
            anyhow::bail!("no savestate path for this ROM");
        };
        savestate::save(&emu, &path)?;
        println!(
            "  saved state → {} + {} + {}",
            path.display(),
            crate::video::shot_path_for_state(&path).display(),
            crate::statedbg::dbg_path_for_state(&path).display()
        );
        emu.bus.sound.stop_host();
    }
    Ok(())
}


fn diagnose_rom(rom: &PathBuf, max_steps: u64) -> Result<()> {
    let cart = Cart::load(rom)?;
    cart::print_info(&cart);
    let mut emu = Emu::from_cart(cart, None);
    emu.attach_rom_path(rom);
    let mut last_valid = emu.cpu.pc();
    let mut invalid_at = None;
    for step in 0..max_steps {
        let pc = emu.cpu.pc();
        let valid = (pc < 0x4000)
            || (0x0200_0000..0x0204_0000).contains(&pc)
            || (0x0300_0000..0x0300_8000).contains(&pc)
            || (0x0800_0000..0x0E00_0000).contains(&pc);
        if !valid {
            invalid_at = Some((step, pc, last_valid, emu.cpu.r, emu.cpu.cpsr.thumb, emu.bus.dispcnt()));
            break;
        }
        last_valid = pc;
        let c = if emu.bus.halt_wait {
            // honor halt
            if emu.step_cycles(64) {}
            continue;
        } else {
            emu.cpu.step(&mut emu.bus)
        };
        emu.timers.reload = emu.bus.timer_reload;
        crate::timers::step(&mut emu.timers, &mut emu.bus, c);
        emu.bus.timer_reload = emu.timers.reload;
        emu.ppu.step(&mut emu.bus, c);
        crate::irq::check(&mut emu.cpu, &mut emu.bus);
        if step < 40 || step >= 70 {
            let op = if emu.cpu.cpsr.thumb {
                emu.bus.read16(pc) as u32
            } else {
                emu.bus.read32(pc)
            };
            println!(
                "{:6} pc={:08X} op={:08X} thumb={} r0={:08X} r1={:08X} r14={:08X} sp={:08X} dispcnt={:04X}",
                step, pc, op, emu.cpu.cpsr.thumb, emu.cpu.r[0], emu.cpu.r[1], emu.cpu.r[14],
                emu.cpu.r[13], emu.bus.dispcnt()
            );
        }
    }
    if let Some((step, pc, last, r, thumb, dc)) = invalid_at {
        println!("INVALID at step {step}: pc={pc:08X} last_valid={last:08X} thumb={thumb} dispcnt={dc:04X}");
        println!("  r0-7  {:08X?}", &r[0..8]);
        println!("  r8-15 {:08X?}", &r[8..16]);
    } else {
        println!("survived {max_steps} steps pc={:08X} dispcnt={:04X}", emu.cpu.pc(), emu.bus.dispcnt());
    }
    let ie = emu.bus.read16(0x0400_0200);
    let if_ = emu.bus.read16(0x0400_0202);
    let ime = emu.bus.read16(0x0400_0208);
    println!(
        "  IE={ie:04X} IF={if_:04X} IME={ime:04X} irq_dis={} mode={:02X} handler={:08X} irqs={}",
        emu.cpu.cpsr.irq_disable,
        emu.cpu.cpsr.mode,
        emu.bus.read32(0x0300_7FFC),
        emu.bus.irq_count,
    );
    Ok(())
}

fn debug_spark(steps: u32) {
    let cart = fable::spark_rom();
    cart::print_info(&cart);
    let mut emu = Emu::new(&cart, None);
    println!("start pc={:08X}", emu.cpu.pc());
    for i in 0..steps {
        let pc = emu.cpu.pc();
        let op = emu.bus.read32(pc);
        let c = emu.cpu.step(&mut emu.bus);
        emu.ppu.step(&mut emu.bus, c);
        let npc = emu.cpu.pc();
        // dump around wait leave / erase / draw
        if i < 20 || (0x08000134..=0x08000180).contains(&pc) || (0x08000134..=0x08000180).contains(&npc) {
            println!(
                "{:5} pc={:08X} op={:08X} -> {:08X} r0={:08X} r1={:08X} r4={} r5={} r8={:04X} sp={:08X} lr={:08X} vcnt={}",
                i, pc, op, npc, emu.cpu.r[0], emu.cpu.r[1], emu.cpu.r[4], emu.cpu.r[5],
                emu.cpu.r[8], emu.cpu.r[13], emu.cpu.r[14], emu.bus.read16(0x04000006)
            );
        }
    }
    let lit = emu.ppu.frame.iter().filter(|&&p| p != 0).count();
    println!("lit pixels after {} steps: {}", steps, lit);
    println!("vram[0..4]={:02x?}", &emu.bus.vram[0..8]);
    // center pixel offset
    let off = (80 * 240 + 120) * 2;
    println!("vram center={:02x}{:02x}", emu.bus.vram[off], emu.bus.vram[off+1]);
}

fn run_self_tests() -> usize {
    let mut passed = 0;

    {
        let mut rom = vec![0u8; 0x200];
        rom[0..4].copy_from_slice(&0xE3A0_0001u32.to_le_bytes());
        rom[4..8].copy_from_slice(&0xE280_0002u32.to_le_bytes());
        rom[8..12].copy_from_slice(&0xEAFF_FFFEu32.to_le_bytes());
        let cart = Cart {
            data: rom,
            title: "t".into(),
            game_code: "T".into(),
            maker: "00".into(),
            path: "m".into(),
            inner_name: None,
        };
        let mut emu = Emu::new(&cart, None);
        emu.cpu.set_pc(0x0800_0000);
        emu.cpu.step(&mut emu.bus);
        emu.cpu.step(&mut emu.bus);
        assert_eq!(emu.cpu.r[0], 3);
        passed += 1;
    }

    {
        let cart = Cart {
            data: vec![0u8; 0x200],
            title: "t".into(),
            game_code: "T".into(),
            maker: "00".into(),
            path: "m".into(),
            inner_name: None,
        };
        let mut emu = Emu::new(&cart, None);
        emu.bus.write16(0x0300_0000, 0x2005);
        emu.bus.write16(0x0300_0002, 0x3003);
        emu.cpu.cpsr.thumb = true;
        emu.cpu.set_pc(0x0300_0000);
        emu.cpu.step(&mut emu.bus);
        emu.cpu.step(&mut emu.bus);
        assert_eq!(emu.cpu.r[0], 8);
        passed += 1;
    }

    {
        let cart = fable::spark_rom();
        let mut emu = Emu::new(&cart, None);
        let n = emu.run_frames(3);
        assert!(n >= 1, "spark produces frames");
        // Mode 3 should be on; spark near center should be lit
        let dc = emu.bus.dispcnt();
        assert_eq!(dc & 7, 3, "DISPCNT mode3, got {dc:#x}");
        // scan for any bright pixel in framebuffer
        let lit = emu.ppu.frame.iter().any(|&p| p & 0x7FFF != 0);
        assert!(lit, "spark should draw at least one pixel");
        passed += 1;
    }

    {
        let cart = Cart {
            data: vec![0u8; 0x200],
            title: "p".into(),
            game_code: "P".into(),
            maker: "00".into(),
            path: "m".into(),
            inner_name: None,
        };
        let mut emu = Emu::new(&cart, None);
        emu.bus.write16(0x0400_0000, 0x0003);
        emu.bus.write16(0x0600_0000, 0x001F);
        ppu::render::render_scanline(&emu.bus, 0, &mut emu.ppu.frame); // Mode3 test; affine unused
        assert_eq!(emu.ppu.frame[0] & 0x1F, 0x1F);
        passed += 1;
    }

    {
        let mut rom = vec![0u8; 0x400];
        rom[0x100..0x106].copy_from_slice(b"SRAM_V");
        assert!(matches!(battery::detect(&rom), battery::SaveType::Sram(_)));
        rom = vec![0u8; 0x400];
        rom[0x100..0x108].copy_from_slice(b"FLASH_V ");
        assert!(matches!(battery::detect(&rom), battery::SaveType::Flash64 | battery::SaveType::Flash128) || matches!(battery::detect(&rom), battery::SaveType::Flash64));
        // round-trip sav
        let cart = Cart { data: { let mut r=vec![0u8;0x400]; r[0x100..0x106].copy_from_slice(b"SRAM_V"); r }, title:"b".into(), game_code:"B".into(), maker:"00".into(), path:"m".into(), inner_name: None };
        let mut emu = Emu::new(&cart, None);
        let sav = std::env::temp_dir().join("fairy-bat-test.sav");
        let _ = std::fs::remove_file(&sav);
        emu.bus.load_battery(sav.clone());
        emu.bus.write8(0x0E00_0000, 0x42);
        assert!(emu.bus.save_dirty);
        emu.flush_battery();
        let data = std::fs::read(&sav).expect("sav written");
        assert_eq!(data[0], 0x42);
        let mut emu2 = Emu::new(&cart, None);
        emu2.bus.load_battery(sav);
        assert_eq!(emu2.bus.read8(0x0E00_0000), 0x42);
        passed += 1;
    }

    // EEPROM serial: write 8 bytes then read them back (6-bit address device)
    {
        let mut chip = battery::EepromChip::new(512);
        // Write cmd to addr 0: start=1, cmd=10, addr=000000, then 64 data bits
        // Data = 0x0123456789ABCDEF
        let data: u64 = 0x0123_4567_89AB_CDEF;
        let mut stream: Vec<u16> = vec![1, 1, 0]; // start + write cmd
        for i in (0..6).rev() {
            stream.push((0u16 >> i) & 1); // addr 0
        }
        for i in (0..64).rev() {
            stream.push(((data >> i) & 1) as u16);
        }
        stream.push(0); // stop
        for b in stream {
            chip.write_bit(b);
        }
        assert!(chip.dirty, "eeprom write should dirty");
        assert_eq!(chip.data[0], 0x01);
        assert_eq!(chip.data[7], 0xEF);
        // Read back
        let mut rstream: Vec<u16> = vec![1, 1, 1]; // start + read
        for _ in 0..6 {
            rstream.push(0);
        }
        for b in rstream {
            chip.write_bit(b);
        }
        let mut out = 0u64;
        // 4 dummy + 64 data
        for _ in 0..4 {
            let _ = chip.read_serial();
        }
        for _ in 0..64 {
            out = (out << 1) | (chip.read_serial() as u64 & 1);
        }
        assert_eq!(out, data, "eeprom readback");
        assert!(matches!(
            battery::detect(b"....EEPROM_V123...."),
            battery::SaveType::Eeprom8K | battery::SaveType::Eeprom512
        ));
        passed += 1;
    }

    // ARM LDR [PC, #imm] uses PC+8 (not PC+12)
    {
        let mut rom = vec![0u8; 0x200];
        // 08000000: LDR r0, [PC, #0]  → loads from 08000008
        rom[0..4].copy_from_slice(&0xE59F_0000u32.to_le_bytes());
        // 08000004: pad
        rom[4..8].copy_from_slice(&0xE1A0_0000u32.to_le_bytes());
        // 08000008: literal 0xDEADBEEF
        rom[8..12].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
        let cart = Cart {
            data: rom,
            title: "pc".into(),
            game_code: "P".into(),
            maker: "00".into(),
            path: "m".into(),
            inner_name: None,
        };
        let mut emu = Emu::new(&cart, None);
        emu.cpu.set_pc(0x0800_0000);
        emu.cpu.step(&mut emu.bus);
        assert_eq!(emu.cpu.r[0], 0xDEAD_BEEF, "ARM LDR [PC,#0] must use PC+8");
        passed += 1;
    }

    // Thumb BL long: return address is insn after the pair; target uses PC+4 on high half
    {
        let mut mem = vec![0u8; 0x100];
        // at 0: BL high F000 (imm=0), at 2: BL low F801 → target = (0+4) + 2 = 6
        // F000 = high imm 0; F801 = low imm 1 → +2
        mem[0..2].copy_from_slice(&0xF000u16.to_le_bytes());
        mem[2..4].copy_from_slice(&0xF801u16.to_le_bytes());
        // at 6: MOV r0, #0x42 (2042)
        mem[6..8].copy_from_slice(&0x2042u16.to_le_bytes());
        let cart = Cart {
            data: vec![0u8; 0x200],
            title: "bl".into(),
            game_code: "B".into(),
            maker: "00".into(),
            path: "m".into(),
            inner_name: None,
        };
        let mut emu = Emu::new(&cart, None);
        emu.bus.iwram[..mem.len()].copy_from_slice(&mem);
        emu.cpu.cpsr.thumb = true;
        emu.cpu.set_pc(0x0300_0000);
        emu.cpu.step(&mut emu.bus); // BL high
        emu.cpu.step(&mut emu.bus); // BL low
        assert_eq!(emu.cpu.pc(), 0x0300_0006, "BL target");
        assert_eq!(emu.cpu.r[14] & !1, 0x0300_0004, "BL LR = next after pair");
        assert_eq!(emu.cpu.r[14] & 1, 1, "BL LR thumb bit");
        emu.cpu.step(&mut emu.bus); // MOV r0,#0x42
        assert_eq!(emu.cpu.r[0], 0x42);
        passed += 1;
    }

    // ARM MSR #imm must change CPSR (mask includes bit 25)
    {
        let mut rom = vec![0u8; 0x200];
        rom[0..4].copy_from_slice(&0xE321_F01Fu32.to_le_bytes());
        let cart = Cart {
            data: rom,
            title: "msr".into(),
            game_code: "M".into(),
            maker: "00".into(),
            path: "m".into(),
            inner_name: None,
        };
        let mut emu = Emu::new(&cart, None);
        emu.cpu.set_mode(0x13);
        emu.cpu.cpsr.irq_disable = true;
        emu.cpu.set_pc(0x0800_0000);
        emu.cpu.step(&mut emu.bus);
        assert_eq!(emu.cpu.cpsr.mode, 0x1F, "MSR CPSR_c, #0x1F");
        assert!(!emu.cpu.cpsr.irq_disable);
        assert_eq!(emu.cpu.unknown_ops, 0);
        passed += 1;
    }

    passed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_self_tests() {
        assert_eq!(run_self_tests(), 9);
    }
}
