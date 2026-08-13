//! Present the lantern's light — PPM dump or terminal (chafa).

use crate::ppu::{self, render};
use anyhow::{Context, Result};
use std::io::Write;
use std::path::Path;
use std::process::Command;

/// Sidecar screenshot next to a `.flst` (`stem.ppm`).
pub fn shot_path_for_state(state: &Path) -> std::path::PathBuf {
    state.with_extension("ppm")
}

pub fn write_ppm(path: &Path, frame: &[u16]) -> Result<()> {
    let rgb = render::frame_to_rgb(frame);
    let mut f = std::fs::File::create(path)
        .with_context(|| format!("write {}", path.display()))?;
    write!(f, "P6\n{} {}\n255\n", ppu::WIDTH, ppu::HEIGHT)?;
    f.write_all(&rgb)?;
    Ok(())
}

fn terminal_cols() -> usize {
    if let Ok(out) = Command::new("stty").args(["size"]).output() {
        if let Ok(s) = String::from_utf8(out.stdout) {
            let parts: Vec<_> = s.split_whitespace().collect();
            if parts.len() >= 2 {
                if let Ok(c) = parts[1].parse::<usize>() {
                    return c.max(20);
                }
            }
        }
    }
    std::env::var("COLUMNS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(80)
        .max(20)
}

/// Show frame in terminal via chafa. Returns false if unavailable.
pub fn present_terminal(frame: &[u16]) -> bool {
    let tmp = std::env::temp_dir().join("fairy-lantern-frame.ppm");
    if write_ppm(&tmp, frame).is_err() {
        return false;
    }
    let cols = terminal_cols().saturating_sub(2).max(20);
    let rows = ((cols * ppu::HEIGHT) / ppu::WIDTH).clamp(8, 48);
    let path = tmp.to_string_lossy();

    for fmt in ["kitty", "sixels", "symbols"] {
        let status = Command::new("chafa")
            .args([
                "-f",
                fmt,
                &format!("--size={cols}x{rows}"),
                "--animate=off",
                path.as_ref(),
            ])
            .status();
        if matches!(status, Ok(s) if s.success()) {
            println!();
            let _ = std::io::stdout().flush();
            return true;
        }
    }
    false
}
