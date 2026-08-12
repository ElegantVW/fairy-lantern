//! Interactive window — light the lantern and play.

use crate::emu::Emu;
use crate::ppu;
use crate::savestate;
use anyhow::{bail, Result};
use minifb::{Key, KeyRepeat, Scale, Window, WindowOptions};
use std::time::{Duration, Instant};

/// GBA KEYINPUT bits
const KEY_A: u16 = 1 << 0;
const KEY_B: u16 = 1 << 1;
const KEY_SELECT: u16 = 1 << 2;
const KEY_START: u16 = 1 << 3;
const KEY_RIGHT: u16 = 1 << 4;
const KEY_LEFT: u16 = 1 << 5;
const KEY_UP: u16 = 1 << 6;
const KEY_DOWN: u16 = 1 << 7;
const KEY_R: u16 = 1 << 8;
const KEY_L: u16 = 1 << 9;

pub fn run_window(emu: &mut Emu, title: &str) -> Result<()> {
    let mut window = Window::new(
        &format!("Fairy Lantern — {title}"),
        ppu::WIDTH,
        ppu::HEIGHT,
        WindowOptions {
            resize: true,
            scale: Scale::X4,
            ..WindowOptions::default()
        },
    )
    .map_err(|e| anyhow::anyhow!("window: {e}"))?;

    // GBA vertical rate ≈ 59.7275 Hz
    const GBA_FPS_NUM: u64 = 16_777_216;
    const GBA_FPS_DEN: u64 = 280_896; // cycles per frame
    let frame_budget = Duration::from_nanos(1_000_000_000 * GBA_FPS_DEN / GBA_FPS_NUM);

    let mut fb = vec![0u32; ppu::WIDTH * ppu::HEIGHT];
    let mut paused = false;
    let mut status = String::new();
    let mut frame_n: u64 = 0;
    let clock_start = Instant::now();

    // DirectSound → speakers (bare `fairy` TUI and `fairy play` both use this path)
    emu.bus.sound.start_host();
    if emu.bus.rtc.present {
        eprintln!("  clock: cartridge RTC present ({})", emu.bus.rtc.clock_string());
    } else {
        eprintln!("  clock: host wall clock ({})", emu.bus.rtc.clock_string());
    }

    println!("✦ Fairy Lantern lit — {title}");
    println!("  arrows/WASD move · Z/Space=A · X=B · Enter=Start · P pause · Esc snuff");
    println!("  F5 savestate · F7 loadstate · battery autosaves to .sav · audio+clock on");

    let mut next_frame = Instant::now();
    while window.is_open() && !window.is_key_down(Key::Escape) {
        if window.is_key_pressed(Key::P, KeyRepeat::No) {
            paused = !paused;
            status = if paused {
                "paused".into()
            } else {
                "resumed".into()
            };
            next_frame = Instant::now();
        }

        // Savestate
        if window.is_key_pressed(Key::F5, KeyRepeat::No) {
            if let Some(path) = emu.state_path() {
                match savestate::save(emu, &path) {
                    Ok(()) => {
                        status = format!("state saved → {}", path.display());
                        eprintln!("  {status}");
                    }
                    Err(e) => {
                        status = format!("state save failed: {e}");
                        eprintln!("  {status}");
                    }
                }
            } else {
                status = "no savestate for built-in fable".into();
            }
        }
        if window.is_key_pressed(Key::F7, KeyRepeat::No) {
            if let Some(path) = emu.state_path() {
                match savestate::load(emu, &path) {
                    Ok(()) => {
                        status = format!("state loaded ← {}", path.display());
                        eprintln!("  {status}");
                    }
                    Err(e) => {
                        status = format!("state load failed: {e}");
                        eprintln!("  {status}");
                    }
                }
            } else {
                status = "no savestate path".into();
            }
        }

        let keys = poll_keys(&window);
        emu.bus.set_keys_pressed(keys);

        if !paused {
            if !emu.run_one_frame() {
                bail!("frame watchdog — CPU stuck (pc=0x{:08X})", emu.cpu.pc());
            }
            frame_n += 1;
            if frame_n == 60 || frame_n == 300 || frame_n == 900 {
                let peak = emu.bus.sound.peak_abs();
                let backend = emu.bus.sound.backend_name();
                let fa = emu.bus.sound.fifo_a_len();
                let fb_ = emu.bus.sound.fifo_b_len();
                let ring = emu.bus.sound.ring_len();
                let from = emu.bus.sound.samples_from_fifo;
                eprintln!(
                    "  audio@{frame_n}: backend={backend} peak={peak} fifoA={fa} fifoB={fb_} ring={ring} from_fifo={from}"
                );
                if frame_n == 300 {
                    let wav = std::env::temp_dir().join("fairy-lantern-audio.wav");
                    if let Err(e) = emu.bus.sound.dump_wav(&wav) {
                        eprintln!("  audio: wav dump failed: {e}");
                    } else {
                        eprintln!(
                            "  audio: wrote {} (play with: aplay {})",
                            wav.display(),
                            wav.display()
                        );
                    }
                }
            }
        }

        for (i, &p) in emu.ppu.frame.iter().enumerate().take(fb.len()) {
            let r = ((p & 0x1F) as u32) << 3;
            let g = (((p >> 5) & 0x1F) as u32) << 3;
            let b = (((p >> 10) & 0x1F) as u32) << 3;
            fb[i] = (r << 16) | (g << 8) | b;
        }

        // Title: fable · RTC/clock · status
        let clk = if frame_n % 30 == 0 || status.is_empty() {
            emu.bus.rtc.clock_string()
        } else {
            emu.bus.rtc.clock_string()
        };
        let elapsed = clock_start.elapsed().as_secs();
        let win_title = if status.is_empty() {
            format!("Fairy Lantern — {title} · {clk} · t={elapsed}s")
        } else {
            format!("Fairy Lantern — {title} · {clk} · {status}")
        };
        window.set_title(&win_title);

        window
            .update_with_buffer(&fb, ppu::WIDTH, ppu::HEIGHT)
            .map_err(|e| anyhow::anyhow!("present: {e}"))?;

        // Pace to GBA frame clock, with light audio water-marks so the host
        // ring neither starves nor grows without bound under load.
        next_frame += frame_budget;
        let now = Instant::now();
        let ring = emu.bus.sound.ring_len();
        // Watermarks in ms; low ≈ 80 ms, high ≈ 200 ms (BT-friendly).
        let rate = emu.bus.sound.stream_rate.max(8000) as usize;
        let ring_low = rate.saturating_mul(80) / 1000;
        let ring_high = rate.saturating_mul(200) / 1000;
        if ring < ring_low {
            // Behind on audio — skip sleep this frame to refill.
            next_frame = now;
        } else if next_frame > now {
            let mut wait = next_frame - now;
            if ring > ring_high {
                // Ahead on audio — allow a little extra sleep (cap +4 ms).
                wait += Duration::from_millis(4);
            }
            std::thread::sleep(wait);
        } else {
            // Fell behind video — resync, but don't hard-reset every frame if
            // only slightly late (keeps audio feed smoother).
            let late = now - next_frame;
            if late > frame_budget * 3 {
                next_frame = now;
            }
        }
    }

    emu.bus.sound.stop_host();
    emu.flush_battery();
    println!("  lantern snuffed (battery flushed).");
    Ok(())
}

fn poll_keys(window: &Window) -> u16 {
    let mut m = 0u16;
    if window.is_key_down(Key::Z) || window.is_key_down(Key::J) || window.is_key_down(Key::Space) {
        m |= KEY_A;
    }
    if window.is_key_down(Key::X) || window.is_key_down(Key::K) {
        m |= KEY_B;
    }
    if window.is_key_down(Key::RightShift) || window.is_key_down(Key::Backspace) {
        m |= KEY_SELECT;
    }
    if window.is_key_down(Key::Enter) {
        m |= KEY_START;
    }
    if window.is_key_down(Key::Right) || window.is_key_down(Key::D) {
        m |= KEY_RIGHT;
    }
    if window.is_key_down(Key::Left) || window.is_key_down(Key::A) {
        m |= KEY_LEFT;
    }
    if window.is_key_down(Key::Up) || window.is_key_down(Key::W) {
        m |= KEY_UP;
    }
    if window.is_key_down(Key::Down) || window.is_key_down(Key::S) {
        m |= KEY_DOWN;
    }
    if window.is_key_down(Key::Q) {
        m |= KEY_L;
    }
    if window.is_key_down(Key::E) {
        m |= KEY_R;
    }
    m
}
