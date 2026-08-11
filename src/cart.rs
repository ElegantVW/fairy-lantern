//! Cartridge / ROM loading — .gba fables, or a .zip that holds one.

use anyhow::{bail, Context, Result};
use std::fs::File;
use std::io::Read;
use std::path::Path;

#[derive(Clone, Debug)]
pub struct Cart {
    pub data: Vec<u8>,
    pub title: String,
    pub game_code: String,
    pub maker: String,
    pub path: String,
    /// Inner name when loaded from zip (e.g. game.gba)
    pub inner_name: Option<String>,
}

impl Cart {
    pub fn load(path: &Path) -> Result<Self> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();

        let (data, inner) = if ext == "zip" {
            load_gba_from_zip(path)?
        } else {
            let data = std::fs::read(path)
                .with_context(|| format!("read fable {}", path.display()))?;
            (data, None)
        };

        if data.len() < 0xC0 {
            bail!("fable too small ({} bytes) — not a GBA ROM", data.len());
        }
        // Title: 0xA0..0xAC, game code 0xAC..0xB0, maker 0xB0..0xB2
        let title = cstr_field(&data[0xA0..0xAC]);
        let game_code = cstr_field(&data[0xAC..0xB0]);
        let maker = cstr_field(&data[0xB0..0xB2]);
        Ok(Self {
            data,
            title,
            game_code,
            maker,
            path: path.display().to_string(),
            inner_name: inner,
        })
    }

    pub fn size(&self) -> usize {
        self.data.len()
    }

    /// Entry point used by many homebrew ROMs (skip full BIOS boot).
    pub fn entry_pc(&self) -> u32 {
        if self.data.len() >= 4 {
            let op = u32::from_le_bytes(self.data[0..4].try_into().unwrap());
            if (op & 0x0E00_0000) == 0x0A00_0000 {
                let imm = (op & 0x00FF_FFFF) as i32;
                let imm = (imm << 8) >> 8;
                return 0u32.wrapping_add(8).wrapping_add((imm * 4) as u32);
            }
        }
        0x0800_0000
    }
}

/// True for paths we can light: bare .gba or .zip containing one.
pub fn is_fable_path(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(ext.as_str(), "gba" | "zip")
}

/// Pull the best .gba entry from a zip (largest .gba wins if several).
fn load_gba_from_zip(path: &Path) -> Result<(Vec<u8>, Option<String>)> {
    let file = File::open(path).with_context(|| format!("open zip {}", path.display()))?;
    let mut archive =
        zip::ZipArchive::new(file).with_context(|| format!("read zip {}", path.display()))?;

    let mut best_i: Option<usize> = None;
    let mut best_size: u64 = 0;
    let mut best_name = String::new();

    for i in 0..archive.len() {
        let entry = archive
            .by_index(i)
            .with_context(|| format!("zip entry {i}"))?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        let lower = name.to_ascii_lowercase();
        if !lower.ends_with(".gba") {
            continue;
        }
        // skip macOS junk
        if lower.contains("__macosx/") || name.split('/').any(|p| p.starts_with('.')) {
            continue;
        }
        let sz = entry.size();
        if sz >= 0xC0 && sz >= best_size {
            best_size = sz;
            best_i = Some(i);
            best_name = name;
        }
    }

    let Some(i) = best_i else {
        bail!(
            "no .gba fable inside zip {}",
            path.display()
        );
    };

    let mut entry = archive.by_index(i)?;
    let mut data = Vec::with_capacity(best_size as usize);
    entry
        .read_to_end(&mut data)
        .with_context(|| format!("extract {best_name} from zip"))?;
    if data.len() < 0xC0 {
        bail!("extracted {best_name} is too small");
    }
    Ok((data, Some(best_name)))
}

fn cstr_field(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end])
        .trim()
        .chars()
        .filter(|c| c.is_ascii_graphic() || *c == ' ')
        .collect()
}

pub fn print_info(cart: &Cart) {
    println!("✦ Fairy Lantern — fable info");
    println!("  path:   {}", cart.path);
    if let Some(ref inner) = cart.inner_name {
        println!("  zip:    {inner}");
    }
    println!(
        "  title:  {}",
        if cart.title.is_empty() {
            "(none)"
        } else {
            &cart.title
        }
    );
    println!(
        "  code:   {}",
        if cart.game_code.is_empty() {
            "(none)"
        } else {
            &cart.game_code
        }
    );
    println!(
        "  maker:  {}",
        if cart.maker.is_empty() {
            "(none)"
        } else {
            &cart.maker
        }
    );
    println!(
        "  size:   {} bytes ({:.1} KiB)",
        cart.size(),
        cart.size() as f64 / 1024.0
    );
    println!("  entry:  0x{:08X} (homebrew-style)", cart.entry_pc());
}
