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
    println!("  F5 save · F7 load · F6 load autosave · hold L3 0.6s save · hold R3 0.6s load");
    println!("  (L2/R2/M2 do not savestate — M2 is the same HID bit as R2)");
    println!("  audio: DirectSound A+B (mp2k L/R)  ·  FAIRY_DS=a|b  ·  FAIRY_AUDIO=sine fairy  → beep");

    let mut next_frame = Instant::now();
    let mut turbo_on = false;
    let mut turbo_mult = 2u32;
    let mut prev_west = false;
    let mut prev_north = false;
    let mut l3_since: Option<Instant> = None;
    let mut r3_since: Option<Instant> = None;
    let mut l3_fired = false;
    let mut r3_fired = false;
    let mut cue: Option<Cue> = None;
    let mut last_auto = Instant::now();
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
        let (l3, r3) = pad.host_sticks();
        let toggle = window.is_key_pressed(Key::C, KeyRepeat::No) || (west && !prev_west);
        let cycle = window.is_key_pressed(Key::V, KeyRepeat::No) || (north && !prev_north);
        prev_west = west;
        prev_north = north;

        let now = Instant::now();
        match hold_progress(l3, &mut l3_since, &mut l3_fired, now) {
            Hold::Charging(p) => cue = Some(Cue::holding(CueKind::Save, p)),
            Hold::Fire => {
                status = save_slot(emu);
                eprintln!("  {status}");
                cue = Some(Cue::flash(CueKind::Save, now));
            }
            Hold::Idle => {}
        }
        match hold_progress(r3, &mut r3_since, &mut r3_fired, now) {
            Hold::Charging(p) => cue = Some(Cue::holding(CueKind::Load, p)),
            Hold::Fire => {
                status = load_slot(emu);
                eprintln!("  {status}");
                cue = Some(Cue::flash(CueKind::Load, now));
            }
            Hold::Idle => {}
        }
        if window.is_key_pressed(Key::F5, KeyRepeat::No) {
            status = save_slot(emu);
            eprintln!("  {status}");
            cue = Some(Cue::flash(CueKind::Save, now));
        }
        if window.is_key_pressed(Key::F7, KeyRepeat::No) {
            status = load_slot(emu);
            eprintln!("  {status}");
            cue = Some(Cue::flash(CueKind::Load, now));
        }
        if window.is_key_pressed(Key::F6, KeyRepeat::No) {
            status = load_auto_slot(emu);
            eprintln!("  {status}");
            cue = Some(Cue::flash(CueKind::Auto, now));
        }
        if now.duration_since(last_auto) >= AUTO_EVERY && !paused {
            let msg = save_auto_slot(emu);
            if !msg.starts_with("no savestate") {
                eprintln!("  autosave {msg}");
                cue = Some(Cue::flash(CueKind::Auto, now));
                last_auto = now;
            }
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
        if let Some(c) = cue {
            if !draw_cue(&mut fb, c, Instant::now()) {
                cue = None;
            }
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
            format!(" · MIX=AB@{}", emu.bus.sound.stream_rate)
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
    let msg = save_auto_slot(emu);
    if !msg.starts_with("no savestate") {
        eprintln!("  autosave on quit {msg}");
    }
    println!("  lantern snuffed (battery flushed).");
    Ok(())
}

const HOLD_FOR: Duration = Duration::from_millis(600);
const AUTO_EVERY: Duration = Duration::from_secs(120);
const CUE_FLASH: Duration = Duration::from_millis(900);

#[derive(Clone, Copy)]
enum CueKind {
    Save,
    Load,
    Auto,
}

#[derive(Clone, Copy)]
struct Cue {
    kind: CueKind,
    hold: Option<f32>,
    flash_at: Option<Instant>,
}

impl Cue {
    fn holding(kind: CueKind, p: f32) -> Self {
        Self {
            kind,
            hold: Some(p.clamp(0.0, 1.0)),
            flash_at: None,
        }
    }
    fn flash(kind: CueKind, now: Instant) -> Self {
        Self {
            kind,
            hold: None,
            flash_at: Some(now),
        }
    }
}

enum Hold {
    Idle,
    Charging(f32),
    Fire,
}

fn hold_progress(
    down: bool,
    since: &mut Option<Instant>,
    fired: &mut bool,
    now: Instant,
) -> Hold {
    if !down {
        *since = None;
        *fired = false;
        return Hold::Idle;
    }
    let start = *since.get_or_insert(now);
    let p = now.duration_since(start).as_secs_f32() / HOLD_FOR.as_secs_f32();
    if p >= 1.0 {
        if *fired {
            return Hold::Idle;
        }
        *fired = true;
        Hold::Fire
    } else {
        Hold::Charging(p)
    }
}

/// Save hue ~320 (hot pink), load ~280 (violet-pink), auto ~350 (warm rose).
fn cue_rgb(kind: CueKind) -> (u8, u8, u8) {
    match kind {
        CueKind::Save => (255, 77, 154),
        CueKind::Load => (210, 90, 255),
        CueKind::Auto => (255, 140, 150),
    }
}

/// Returns false when the flash has finished (caller may drop the cue).
fn draw_cue(fb: &mut [u32], cue: Cue, now: Instant) -> bool {
    let (cr, cg, cb) = cue_rgb(cue.kind);
    let cx = ppu::WIDTH as i32 / 2;
    let cy = ppu::HEIGHT as i32 / 2 + 18;
    if let Some(p) = cue.hold {
        let r = 20.0;
        draw_ring(fb, cx, cy, r, 1.6, cr, cg, cb, 220);
        draw_disk(fb, cx, cy, r * p, cr, cg, cb, 180);
        // orbiting dots so a still frame still reads as "in progress"
        let spin = p * std::f32::consts::TAU * 2.0;
        for i in 0..6 {
            let a = spin + i as f32 * (std::f32::consts::TAU / 6.0);
            let dx = (a.cos() * (r + 4.0)).round() as i32;
            let dy = (a.sin() * (r + 4.0)).round() as i32;
            draw_disk(fb, cx + dx, cy + dy, 1.8, cr, cg, cb, 255);
        }
        return true;
    }
    let Some(t0) = cue.flash_at else {
        return false;
    };
    let t = now.duration_since(t0);
    if t >= CUE_FLASH {
        return false;
    }
    let u = t.as_secs_f32() / CUE_FLASH.as_secs_f32();
    let alpha = ((1.0 - u) * 230.0) as u8;
    let rad = 16.0 + 10.0 * (u * std::f32::consts::PI).sin();
    draw_disk(fb, cx, cy, rad * 0.72, cr, cg, cb, alpha);
    draw_ring(fb, cx, cy, rad, 2.2, cr, cg, cb, alpha);
    true
}

fn draw_disk(fb: &mut [u32], cx: i32, cy: i32, radius: f32, r: u8, g: u8, b: u8, a: u8) {
    let rad = radius.max(0.5);
    let ir = rad.ceil() as i32;
    let r2 = rad * rad;
    for dy in -ir..=ir {
        for dx in -ir..=ir {
            if (dx * dx + dy * dy) as f32 <= r2 {
                blend_px(fb, cx + dx, cy + dy, r, g, b, a);
            }
        }
    }
}

fn draw_ring(fb: &mut [u32], cx: i32, cy: i32, radius: f32, width: f32, r: u8, g: u8, b: u8, a: u8) {
    let ir = (radius + width).ceil() as i32;
    let outer = (radius + width) * (radius + width);
    let inner = (radius - width).max(0.0);
    let inner = inner * inner;
    for dy in -ir..=ir {
        for dx in -ir..=ir {
            let d2 = (dx * dx + dy * dy) as f32;
            if d2 <= outer && d2 >= inner {
                blend_px(fb, cx + dx, cy + dy, r, g, b, a);
            }
        }
    }
}

fn blend_px(fb: &mut [u32], x: i32, y: i32, r: u8, g: u8, b: u8, a: u8) {
    if x < 0 || y < 0 || x >= ppu::WIDTH as i32 || y >= ppu::HEIGHT as i32 {
        return;
    }
    let i = y as usize * ppu::WIDTH + x as usize;
    let dst = fb[i];
    let dr = ((dst >> 16) & 0xff) as u32;
    let dg = ((dst >> 8) & 0xff) as u32;
    let db = (dst & 0xff) as u32;
    let aa = a as u32;
    let ia = 255 - aa;
    let nr = (r as u32 * aa + dr * ia) / 255;
    let ng = (g as u32 * aa + dg * ia) / 255;
    let nb = (b as u32 * aa + db * ia) / 255;
    fb[i] = (nr << 16) | (ng << 8) | nb;
}

fn save_to(emu: &Emu, path: &std::path::Path) -> String {
    match savestate::save(emu, path) {
        Ok(()) => {
            let shot = crate::video::shot_path_for_state(path);
            let dbg = crate::statedbg::dbg_path_for_state(path);
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

fn save_slot(emu: &Emu) -> String {
    let Some(path) = emu.state_path() else {
        return "no savestate for built-in fable".into();
    };
    save_to(emu, &path)
}

fn save_auto_slot(emu: &Emu) -> String {
    let Some(path) = emu.auto_state_path() else {
        return "no savestate for built-in fable".into();
    };
    save_to(emu, &path)
}

fn load_from(emu: &mut Emu, path: &std::path::Path) -> String {
    match savestate::load(emu, path) {
        Ok(()) => format!("state loaded ← {}", path.display()),
        Err(e) => format!("state load failed: {e}"),
    }
}

fn load_slot(emu: &mut Emu) -> String {
    let Some(path) = emu.state_path() else {
        return "no savestate path".into();
    };
    load_from(emu, &path)
}

fn load_auto_slot(emu: &mut Emu) -> String {
    let Some(path) = emu.auto_state_path() else {
        return "no savestate path".into();
    };
    load_from(emu, &path)
}

fn next_turbo_mult(cur: u32) -> u32 {
    match cur {
        2 => 3,
        3 => 4,
        _ => 2,
    }
}

/// Do not sprint extra GBA frames to feed the speaker. That is the
/// "crushed by sound" loop: ring looks thin → extra frame → never sleep.
/// The host resampler stretches a few percent instead.
fn audio_catchup_extra(fifo_locked: bool, ring_frames: usize, stream_rate: u32) -> u32 {
    let _ = (fifo_locked, ring_frames, stream_rate);
    0
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
    use super::{audio_catchup_extra, hold_progress, next_turbo_mult, Hold};
    use std::time::{Duration, Instant};

    #[test]
    fn no_catchup_when_ring_healthy() {
        assert_eq!(audio_catchup_extra(true, 4000, 32768), 0);
    }

    #[test]
    fn no_sprint_when_starving() {
        assert_eq!(audio_catchup_extra(true, 10, 32768), 0);
        assert_eq!(audio_catchup_extra(true, 10, 13379), 0);
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

    #[test]
    fn hold_fires_once_after_threshold() {
        let t0 = Instant::now();
        let mut since = None;
        let mut fired = false;
        assert!(matches!(
            hold_progress(true, &mut since, &mut fired, t0),
            Hold::Charging(_)
        ));
        let t1 = t0 + Duration::from_millis(700);
        assert!(matches!(
            hold_progress(true, &mut since, &mut fired, t1),
            Hold::Fire
        ));
        assert!(matches!(
            hold_progress(true, &mut since, &mut fired, t1 + Duration::from_millis(10)),
            Hold::Idle
        ));
        assert!(matches!(
            hold_progress(false, &mut since, &mut fired, t1),
            Hold::Idle
        ));
    }
}
