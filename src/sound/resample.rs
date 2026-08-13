//! Lock-friendly sample ring + linear resampler (game rate → host rate).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Live host ring capacity in interleaved stereo i16s (~1.8 s @ 13.4 kHz).
pub const RING_CAP: usize = 48_000;
/// Rolling capture for WAV dump (~6 s @ 32 kHz).
pub const CAPTURE_CAP: usize = 13_500 * 20; // ~20s @ game rate

/// Shared mono PCM ring. Producer never drops-oldest under load: if full, newest
/// samples are coalesced by skipping push (rare; play loop should pace instead).
#[derive(Clone, Debug)]
pub struct SampleRing {
    pub(crate) inner: Arc<Mutex<VecDeque<i16>>>,
}

impl Default for SampleRing {
    fn default() -> Self {
        Self::new()
    }
}

impl SampleRing {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::with_capacity(RING_CAP))),
        }
    }

    pub fn clone_handle(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }

    pub fn len(&self) -> usize {
        self.inner.lock().map(|r| r.len()).unwrap_or(0)
    }

    pub fn available(&self) -> usize {
        self.inner.lock().map(|r| r.len()).unwrap_or(0)
    }

    /// Stereo frames waiting (L/R pairs).
    pub fn frames(&self) -> usize {
        self.len() / 2
    }

    pub fn pop_frame(&self) -> Option<(i16, i16)> {
        let mut ring = self.inner.lock().ok()?;
        let l = ring.pop_front()?;
        let r = ring.pop_front().unwrap_or(l);
        Some((l, r))
    }

    pub fn push_batch(&self, samples: &[i16]) {
        if samples.is_empty() {
            return;
        }
        if let Ok(mut ring) = self.inner.lock() {
            // Interleaved L,R. Drop a whole frame if we have to drop.
            let mut i = 0;
            while i + 1 < samples.len() {
                if ring.len() + 2 > RING_CAP {
                    // Drop newest, not oldest. Eating the past while the
                    // speaker is still playing it is a skip/stutter.
                    break;
                }
                ring.push_back(samples[i]);
                ring.push_back(samples[i + 1]);
                i += 2;
            }
        }
    }

    /// Pop up to `n` samples; pad with 0 on underrun. Returns (bytes written conceptually, underruns).
    pub fn fill_i16(&self, out: &mut [i16]) -> usize {
        let mut underrun = 0usize;
        if let Ok(mut ring) = self.inner.lock() {
            for d in out.iter_mut() {
                if let Some(s) = ring.pop_front() {
                    *d = s;
                } else {
                    *d = 0;
                    underrun += 1;
                }
            }
        } else {
            out.fill(0);
            underrun = out.len();
        }
        underrun
    }

    /// Cubic-resample `src_rate` ring content into `dst_rate` samples.
    /// `fpos` is fractional read cursor in source samples (mutated).
    /// On underrun, output zero AND advance fpos so the cursor never gets
    /// permanently stuck behind the ring.
    pub fn resample_into(
        &self,
        out: &mut [i16],
        src_rate: u32,
        dst_rate: u32,
        fpos: &mut f64,
    ) -> usize {
        if src_rate == 0 || dst_rate == 0 {
            out.fill(0);
            return out.len();
        }
        let ratio = src_rate as f64 / dst_rate as f64;
        let mut underrun = 0usize;
        if let Ok(mut ring) = self.inner.lock() {
            for d in out.iter_mut() {
                let i = *fpos as usize;
                let t = (*fpos - i as f64) as f32;

                let v1 = ring_read(&ring, i.saturating_sub(1));
                let v2 = ring_read(&ring, i);
                let v3 = ring_read(&ring, i + 1);
                let v4 = ring_read(&ring, i + 2);

                match (v1, v2, v3, v4) {
                    (_, Some(s2), Some(s3), Some(s4)) if i >= 1 => {
                        let s1 = v1.unwrap();
                        *d = cubic_interp(s1, s2, s3, s4, t);
                    }
                    (_, Some(s2), Some(s3), _) => {
                        // Linear fallback at edges or if not enough history
                        *d = if t > 1e-6 {
                            (s2 as f32 + (s3 as f32 - s2 as f32) * t) as i16
                        } else {
                            s2
                        };
                    }
                    (_, Some(s2), None, _) => {
                        // Single sample available — hold
                        *d = s2;
                    }
                    _ => {
                        // Underrun — write silence, but KEEP advancing fpos
                        // so the cursor doesn't get permanently stuck.
                        *d = 0;
                        underrun += 1;
                    }
                }
                *fpos += ratio;
                while *fpos >= 1.0 && !ring.is_empty() {
                    ring.pop_front();
                    *fpos -= 1.0;
                }
            }
        } else {
            out.fill(0);
            underrun = out.len();
        }
        underrun
    }
}
/// Catmull-Rom cubic interpolation: x1,x2,x3,x4 (x2..x3 range, t=0→1).
#[inline]
fn cubic_interp(p1: i16, p2: i16, p3: i16, p4: i16, t: f32) -> i16 {
    let p1 = p1 as f32;
    let p2 = p2 as f32;
    let p3 = p3 as f32;
    let p4 = p4 as f32;
    let t2 = t * t;
    let t3 = t2 * t;
    let a = -0.5 * p1 + 1.5 * p2 - 1.5 * p3 + 0.5 * p4;
    let b = p1 - 2.5 * p2 + 2.0 * p3 - 0.5 * p4;
    let c = -0.5 * p1 + 0.5 * p3;
    let d = p2;
    let y = a * t3 + b * t2 + c * t + d;
    y.clamp(-32768.0, 32767.0) as i16
}

/// Fixed device rate. Host opens aplay once at this rate and never reopens.
pub const HOST_RATE: u32 = 48_000;

/// Game-rate → HOST_RATE linear pull resampler. Stereo frames. Underrun holds.
#[derive(Debug, Default)]
pub struct PullResampler {
    prev_l: i16,
    prev_r: i16,
    next_l: i16,
    next_r: i16,
    frac: f64,
    empty: u32,
}

impl PullResampler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fill interleaved stereo `out` at HOST_RATE from `ring` at `src_rate`.
    pub fn fill(&mut self, ring: &SampleRing, src_rate: u32, out: &mut [i16]) {
        let src = src_rate.max(1) as f64;
        let step = src / HOST_RATE as f64;
        let mut i = 0;
        while i + 1 < out.len() {
            self.frac += step;
            while self.frac >= 1.0 {
                self.frac -= 1.0;
                self.prev_l = self.next_l;
                self.prev_r = self.next_r;
                if let Some((l, r)) = ring.pop_frame() {
                    self.next_l = l;
                    self.next_r = r;
                    self.empty = 0;
                } else {
                    self.empty = self.empty.saturating_add(1);
                    // Hold-last-sample is what sounded like a loop. Silence.
                    self.next_l = 0;
                    self.next_r = 0;
                    self.prev_l = 0;
                    self.prev_r = 0;
                }
            }
            let t = self.frac.clamp(0.0, 1.0);
            out[i] = (self.prev_l as f64 + (self.next_l as f64 - self.prev_l as f64) * t) as i16;
            out[i + 1] =
                (self.prev_r as f64 + (self.next_r as f64 - self.prev_r as f64) * t) as i16;
            i += 2;
        }
    }
}

/// Resample interleaved stereo `src` at `src_rate` to HOST_RATE (for WAV dump).
pub fn resample_stereo_to_host(src: &[i16], src_rate: u32) -> Vec<i16> {
    let frames = src.len() / 2;
    if frames == 0 || src_rate == 0 {
        return Vec::new();
    }
    let n_out = ((frames as u64) * (HOST_RATE as u64) / (src_rate as u64)).max(1) as usize;
    let step = src_rate as f64 / HOST_RATE as f64;
    let mut out = vec![0i16; n_out * 2];
    let mut pos = 0.0f64;
    let last = frames - 1;
    for i in 0..n_out {
        let i0 = (pos as usize).min(last);
        let i1 = (i0 + 1).min(last);
        let t = pos - i0 as f64;
        let l0 = src[i0 * 2] as f64;
        let r0 = src[i0 * 2 + 1] as f64;
        let l1 = src[i1 * 2] as f64;
        let r1 = src[i1 * 2 + 1] as f64;
        out[i * 2] = (l0 + (l1 - l0) * t) as i16;
        out[i * 2 + 1] = (r0 + (r1 - r0) * t) as i16;
        pos += step;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_13378_to_48k_length() {
        let ring = SampleRing::new();
        let src_rate = 13378u32;
        let n_src = src_rate as usize; // 1 s of frames
        let mut src = Vec::with_capacity(n_src * 2);
        for i in 0..n_src {
            let v = ((i % 64) as i16 - 32) * 200;
            src.push(v);
            src.push(v / 2);
        }
        ring.push_batch(&src);
        let n_dst = HOST_RATE as usize;
        let mut dst = vec![0i16; n_dst * 2];
        let mut rs = PullResampler::new();
        rs.fill(&ring, src_rate, &mut dst);
        assert_eq!(dst.len(), n_dst * 2);
        let peak = dst.iter().map(|s| s.abs()).max().unwrap();
        assert!(peak > 100, "resampled signal should not be silent peak={peak}");
    }
}

fn ring_read(ring: &VecDeque<i16>, i: usize) -> Option<i16> {
    let (a, b) = ring.as_slices();
    if i < a.len() {
        Some(a[i])
    } else {
        let j = i - a.len();
        if j < b.len() {
            Some(b[j])
        } else {
            None
        }
    }
}

/// Rolling capture buffer (not drained by host).
#[derive(Debug, Default)]
pub struct Capture {
    buf: VecDeque<i16>,
}

impl Capture {
    pub fn new() -> Self {
        Self {
            buf: VecDeque::with_capacity(CAPTURE_CAP),
        }
    }

    pub fn push_batch(&mut self, samples: &[i16]) {
        for &s in samples {
            if self.buf.len() >= CAPTURE_CAP {
                self.buf.pop_front();
            }
            self.buf.push_back(s);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn to_vec(&self) -> Vec<i16> {
        self.buf.iter().copied().collect()
    }
}
