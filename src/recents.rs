//! Last-opened fable + recent list under XDG data.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

const MAX_RECENTS: usize = 12;

pub fn data_dir() -> PathBuf {
    if let Ok(p) = std::env::var("FAIRY_LANTERN_DIR") {
        return PathBuf::from(p);
    }
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share")
        });
    base.join("faeos/fairy-lantern")
}

pub fn roms_dir() -> PathBuf {
    if let Ok(p) = std::env::var("FAIRY_LANTERN_ROMS") {
        return PathBuf::from(p);
    }
    data_dir().join("roms")
}

fn recents_path() -> PathBuf {
    data_dir().join("recents.txt")
}

fn last_path() -> PathBuf {
    data_dir().join("last.txt")
}

pub fn ensure_dirs() -> Result<()> {
    fs::create_dir_all(data_dir()).context("create fairy-lantern data dir")?;
    fs::create_dir_all(roms_dir()).ok();
    Ok(())
}

/// Paths of recent fables (newest first). Missing files are dropped.
pub fn load_recents() -> Vec<PathBuf> {
    let path = recents_path();
    let Ok(text) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in text.lines() {
        let p = PathBuf::from(line.trim());
        if p.is_file() && !out.iter().any(|x: &PathBuf| x == &p) {
            out.push(p);
        }
    }
    out
}

pub fn last_rom() -> Option<PathBuf> {
    let path = last_path();
    let text = fs::read_to_string(path).ok()?;
    let p = PathBuf::from(text.trim());
    if p.is_file() {
        Some(p)
    } else {
        // fall back to first recent
        load_recents().into_iter().next()
    }
}

pub fn remember(rom: &Path) -> Result<()> {
    ensure_dirs()?;
    let abs = fs::canonicalize(rom).unwrap_or_else(|_| rom.to_path_buf());
    fs::write(last_path(), format!("{}\n", abs.display())).context("write last.txt")?;

    let mut rec = load_recents();
    rec.retain(|p| p != &abs);
    rec.insert(0, abs);
    rec.truncate(MAX_RECENTS);
    let body: String = rec
        .iter()
        .map(|p| format!("{}\n", p.display()))
        .collect();
    fs::write(recents_path(), body).context("write recents.txt")?;
    Ok(())
}

pub fn list_roms_dir() -> Vec<PathBuf> {
    let dir = roms_dir();
    let Ok(rd) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| crate::cart::is_fable_path(p))
        .collect();
    paths.sort();
    paths
}
