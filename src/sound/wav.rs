//! Diagnostic WAV writer (interleaved stereo i16).

use std::io::Write;
use std::path::Path;

pub fn write_wav_stereo(path: &Path, rate: u32, samples: &[i16]) -> std::io::Result<()> {
    use std::fs::File;
    let mut f = File::create(path)?;
    let n = (samples.len() & !1) as u32;
    let data_len = n * 2;
    let rate = rate.max(8000);
    let ch = 2u16;
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_len).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?; // PCM
    f.write_all(&ch.to_le_bytes())?;
    f.write_all(&rate.to_le_bytes())?;
    f.write_all(&(rate * 4).to_le_bytes())?;
    f.write_all(&4u16.to_le_bytes())?;
    f.write_all(&16u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_len.to_le_bytes())?;
    for s in samples.iter().take(n as usize) {
        f.write_all(&s.to_le_bytes())?;
    }
    Ok(())
}
