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
    let mut pad = crate::pad::Pad::open();
    if emu.bus.rtc.present {
        eprintln!("  clock: cartridge RTC present ({})", emu.bus.rtc.clock_string());
    } else {
        eprintln!("  clock: host wall clock ({})", emu.bus.rtc.clock_string());
    }

    println!("✦ Fairy Lantern lit — {title}");
    println!("  arrows/WASD move · Z/Space=A · X=B · Enter=Start · P pause · Esc snuff");
    println!("  pad: {} (keyboard still ORs · FAIRY_PAD=nintendo|xbox)", pad.describe());
    println!("  turbo: C / pad X on-off · V / pad Y cycle 2× 3× 4× (audio mutes while on)");
    println!("  F5 / pad L2 savestate+shot+dbg · F7 / pad R2 load · F8 OAM dump · battery .sav");
    println!("  audio: DirectSound A+B (mp2k L/R)  ·  FAIRY_DS=a|b  ·  FAIRY_AUDIO=sine fairy  → beep");

    let mut next_frame = Instant::now();
    let mut turbo_on = false;
    let mut turbo_mult = 2u32;
    let mut prev_west = false;
    let mut prev_north = false;
    let mut prev_l2 = false;
    let mut prev_r2 = false;
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

        // Savestate (F5 / L2 save · F7 / R2 load) — handled after pad.poll so
        // trigger edges are fresh. Keyboard F5/F7 stay.
        if window.is_key_pressed(Key::F8, KeyRepeat::No) {
            crate::emu::dump_oam_stat(&emu.bus, frame_n as u32);
            let ppm = std::env::temp_dir().join("fairy-lantern-oam.ppm");
            match crate::video::write_ppm(&ppm, &emu.ppu.frame) {
                Ok(()) => {
                    status = format!("OAM dump → stderr + {}", ppm.display());
                    eprintln!("  {status}");
                }
                Err(e) => status = format!("OAM ppm failed: {e}"),
            }
        }

        let keys = poll_keys(&window) | pad.poll();
        emu.bus.set_keys_pressed(keys);

        let (west, north) = pad.host_xy();
        let (l2, r2) = pad.host_triggers();
        let toggle = window.is_key_pressed(Key::C, KeyRepeat::No) || (west && !prev_west);
        let cycle = window.is_key_pressed(Key::V, KeyRepeat::No) || (north && !prev_north);
        let do_save = window.is_key_pressed(Key::F5, KeyRepeat::No) || (l2 && !prev_l2);
        let do_load = window.is_key_pressed(Key::F7, KeyRepeat::No) || (r2 && !prev_r2);
        prev_west = west;
        prev_north = north;
        prev_l2 = l2;
        prev_r2 = r2;
        if do_save {
            status = save_slot(emu);
            eprintln!("  {status}");
        }
        if do_load {
            status = load_slot(emu);
            eprintln!("  {status}");
        }
        if toggle {
            turbo_on = !turbo_on;
            emu.bus.sound.set_emit_host(!turbo_on);
            next_frame = Instant::now();
            status = if turbo_on {
                format!("turbo {turbo_mult}×")
            } else {
                "turbo off".into()
            };
            eprintln!("  {status}");
        }
        if cycle {
            turbo_mult = next_turbo_mult(turbo_mult);
            status = if turbo_on {
                format!("turbo {turbo_mult}×")
            } else {
                format!("turbo {turbo_mult}× (off)")
            };
            eprintln!("  {status}");
        }

        if !paused {
            // One video frame per present at 1×. Turbo runs N GBA frames
            // then presents the last. A hitch used to chase wall-clock
            // (up to 6 GBA frames) and never sleep again — that is the
            // "slow and it never came back" report. Extra work only if the
            // audio ring is actually starving (and turbo is off).
            let run_n = if turbo_on { turbo_mult } else { 1 };
            for _ in 0..run_n {
                if !run_frame(emu, &mut frame_n)? {
                    bail!("frame watchdog — CPU stuck (pc=0x{:08X})", emu.cpu.pc());
                }
            }
            if !turbo_on {
                let extra = audio_catchup_extra(
                    emu.bus.sound.fifo_locked(),
                    emu.bus.sound.ring_frames(),
                    emu.bus.sound.stream_rate,
                );
                for _ in 0..extra {
                    if !run_frame(emu, &mut frame_n)? {
                        bail!("frame watchdog — CPU stuck (pc=0x{:08X})", emu.cpu.pc());
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
        let clk = emu.bus.rtc.clock_string();
        let elapsed = clock_start.elapsed().as_secs();
        let unk = if emu.cpu.unknown_ops > 0 {
            format!(
                " · unk_ops={} last={:08X}",
                emu.cpu.unknown_ops, emu.cpu.last_unknown
            )
        } else {
            String::new()
        };
        let mix = if turbo_on {
            format!(" · TURBO {turbo_mult}×")
        } else if std::env::var("FAIRY_AUDIO")
            .map(|v| v.eq_ignore_ascii_case("sine"))
            .unwrap_or(false)
        {
            " · MIX=SINE".into()
        } else {
            " · MIX=AB@32k".into()
        };
        let win_title = if status.is_empty() {
            format!("Fairy Lantern — {title} · {clk} · t={elapsed}s{unk}{mix}")
        } else {
            format!("Fairy Lantern — {title} · {clk} · {status}{unk}{mix}")
        };
        window.set_title(&win_title);

        window
            .update_with_buffer(&fb, ppu::WIDTH, ppu::HEIGHT)
            .map_err(|e| anyhow::anyhow!("present: {e}"))?;

        // Sleep when ahead. After a hitch, drop the debt so the *next*
        // loop can sleep — do not stay 4+ frames late forever.
        // Turbo skips the wait: we already burned N GBA frames this present.
        if turbo_on {
            next_frame = Instant::now();
        } else {
            next_frame += frame_budget;
            let now = Instant::now();
            if next_frame > now {
                std::thread::sleep(next_frame - now);
            } else if now.duration_since(next_frame) > frame_budget * 2 {
                next_frame = now;
            }
        }
    }

    emu.bus.sound.stop_host();
    emu.flush_battery();
    println!("  lantern snuffed (battery flushed).");
    Ok(())
}

fn save_slot(emu: &Emu) -> String {
    let Some(path) = emu.state_path() else {
        return "no savestate for built-in fable".into();
    };
    match savestate::save(emu, &path) {
        Ok(()) => {
            let shot = crate::video::shot_path_for_state(&path);
            let dbg = crate::statedbg::dbg_path_for_state(&path);
            format!(
                "state saved → {} + {} + {}",
                path.display(),
                shot.display(),
                dbg.display()
            )
        }
        Err(e) => format!("state save failed: {e}"),
    }
}

fn load_slot(emu: &mut Emu) -> String {
    let Some(path) = emu.state_path() else {
        return "no savestate path".into();
    };
    match savestate::load(emu, &path) {
        Ok(()) => format!("state loaded ← {}", path.display()),
        Err(e) => format!("state load failed: {e}"),
    }
}

fn next_turbo_mult(cur: u32) -> u32 {
    match cur {
        2 => 3,
        3 => 4,
        _ => 2,
    }
}

/// At most one extra GBA frame, and only when the host ring has < ~20 ms.
fn audio_catchup_extra(fifo_locked: bool, ring_frames: usize, stream_rate: u32) -> u32 {
    if !fifo_locked {
        return 0;
    }
    let low = (stream_rate.max(8_000) / 50).max(64) as usize;
    u32::from(ring_frames < low)
}

fn run_frame(emu: &mut Emu, frame_n: &mut u64) -> Result<bool> {
    if !emu.run_one_frame() {
        return Ok(false);
    }
    *frame_n += 1;
    if *frame_n == 1 {
        eprintln!("  video: first frame presented");
    }
    log_audio_probe(emu, *frame_n);
    Ok(true)
}

fn log_audio_probe(emu: &Emu, frame_n: u64) {
    if frame_n != 60 && frame_n != 300 && frame_n != 900 {
        return;
    }
    let peak = emu.bus.sound.peak_abs();
    let backend = emu.bus.sound.backend_name();
    let fa = emu.bus.sound.fifo_a_len();
    let fb_ = emu.bus.sound.fifo_b_len();
    let ring = emu.bus.sound.ring_frames();
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
                "  audio: wrote {} (48 kHz stereo — play with: aplay {})",
                wav.display(),
                wav.display()
            );
        }
    }
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

#[cfg(test)]
mod tests {
    use super::{audio_catchup_extra, next_turbo_mult};

    #[test]
    fn no_catchup_when_ring_healthy() {
        assert_eq!(audio_catchup_extra(true, 4000, 32768), 0);
    }

    #[test]
    fn one_frame_when_starving() {
        assert_eq!(audio_catchup_extra(true, 10, 32768), 1);
    }

    #[test]
    fn none_before_fifo_lock() {
        assert_eq!(audio_catchup_extra(false, 0, 32768), 0);
    }

    #[test]
    fn turbo_cycles_2_3_4() {
        assert_eq!(next_turbo_mult(2), 3);
        assert_eq!(next_turbo_mult(3), 4);
        assert_eq!(next_turbo_mult(4), 2);
        assert_eq!(next_turbo_mult(0), 2);
    }
}
