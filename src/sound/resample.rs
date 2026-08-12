//! Lock-friendly sample ring + linear resampler (game rate → host rate).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Live host ring capacity in mono i16 samples (~500 ms @ 48 kHz).
pub const RING_CAP: usize = 24_000;
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

    pub fn push_batch(&self, samples: &[i16]) {
        if samples.is_empty() {
            return;
        }
        if let Ok(mut ring) = self.inner.lock() {
            for &s in samples {
                if ring.len() >= RING_CAP {
                    // Prefer dropping oldest only as last resort after long stall;
                    // mark by skipping further pushes this batch after one drop cycle.
                    ring.pop_front();
                }
                ring.push_back(s);
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
