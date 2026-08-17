//! Host audio: resample game-rate PCM to 48 kHz and pipe to aplay / pw-cat.
//! Device is opened once and never restarted.

use super::resample::{PullResampler, SampleRing, HOST_RATE};
use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub const SLEEP_MS: u64 = 1;

pub struct HostAudio {
    join: Option<JoinHandle<()>>,
    stop: Arc<Mutex<bool>>,
    player_pid: Arc<Mutex<Option<u32>>>,
    game_rate: Arc<Mutex<u32>>,
    backend: &'static str,
}

impl std::fmt::Debug for HostAudio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostAudio")
            .field("backend", &self.backend)
            .finish_non_exhaustive()
    }
}

impl HostAudio {
    pub fn backend(&self) -> &'static str {
        self.backend
    }

    pub fn set_game_rate(&self, rate: u32) {
        if let Ok(mut r) = self.game_rate.lock() {
            *r = rate;
        }
    }

    pub fn current_rate(&self) -> u32 {
        self.game_rate.lock().map(|r| *r).unwrap_or(0)
    }

    /// 440 Hz sine. Default goes through the **same ring + 13.4 kHz→48 kHz
    /// resampler** as a ROM. `--direct` writes 48 kHz straight to the device.
    pub fn play_tone(seconds: f32) {
        Self::play_tone_via_ring(seconds);
    }

    pub fn play_tone_direct(seconds: f32) {
        let spawn: fn(u32) -> Option<Child> = if which("pw-cat") {
            spawn_pw_cat
        } else {
            spawn_aplay
        };
        let mut child = match spawn(HOST_RATE) {
            Some(c) => c,
            None => {
                eprintln!("  audio: cannot open pw-cat/aplay");
                return;
            }
        };
        let Some(mut sin) = child.stdin.take() else {
            let _ = child.kill();
            return;
        };
        eprintln!(
            "  audio: tone 440 Hz stereo {}s @ {HOST_RATE} Hz — if this is clean, the pipe is fine",
            seconds
        );
        let frames = (HOST_RATE as f32 * seconds.max(0.5)) as usize;
        let chunk = HOST_RATE as usize / 50; // 20 ms
        let mut i = 0usize;
        let two_pi = std::f64::consts::TAU;
        while i < frames {
            let n = chunk.min(frames - i);
            let mut buf = Vec::with_capacity(n * 4);
            for k in 0..n {
                let t = (i + k) as f64 / f64::from(HOST_RATE);
                let s = (t * 440.0 * two_pi).sin() * 9000.0;
                let v = s as i16;
                buf.extend_from_slice(&v.to_le_bytes());
                buf.extend_from_slice(&v.to_le_bytes());
            }
            if sin.write_all(&buf).is_err() {
                break;
            }
            i += n;
            std::thread::sleep(Duration::from_millis(20));
        }
        drop(sin);
        let _ = child.kill();
        let _ = child.wait();
        eprintln!("  audio: tone done");
    }

    pub fn play_tone_via_ring(seconds: f32) {
        let ring = SampleRing::new();
        let host = Self::start(ring.clone_handle());
        const SRC: u32 = 13378;
        host.set_game_rate(SRC);
        eprintln!(
            "  audio: tone 440 Hz via ring+resampler ({}s @ {SRC}→{HOST_RATE}) — this is the in-game path",
            seconds
        );
        let frames = (SRC as f32 * seconds.max(0.5)) as usize;
        let start = Instant::now();
        let two_pi = std::f64::consts::TAU;
        let mut i = 0usize;
        while i < frames {
            while ring.frames() > SRC as usize / 6 {
                thread::sleep(Duration::from_millis(4));
            }
            let n = 256.min(frames - i);
            let mut batch = Vec::with_capacity(n * 2);
            for k in 0..n {
                let t = (i + k) as f64 / f64::from(SRC);
                let s = (t * 440.0 * two_pi).sin() * 9000.0;
                let v = s as i16;
                batch.push(v);
                batch.push(v);
            }
            ring.push_batch(&batch);
            i += n;
            let target = Duration::from_secs_f64(i as f64 / f64::from(SRC));
            let now = start.elapsed();
            if target > now {
                thread::sleep(target - now);
            }
        }
        thread::sleep(Duration::from_millis(250));
        host.stop();
        eprintln!("  audio: tone done");
    }

    pub fn start(ring: SampleRing) -> Self {
        let stop = Arc::new(Mutex::new(false));
        let stop2 = Arc::clone(&stop);
        let player_pid = Arc::new(Mutex::new(None));
        let pid2 = Arc::clone(&player_pid);
        let game_rate = Arc::new(Mutex::new(0u32));
        let rate2 = Arc::clone(&game_rate);

        // pw-cat first: aplay-on-PipeWire repeats the last period on underrun
        // (the "it loops" the play window was producing). The pull resampler
        // holds the last frame; it must not change speed with ring depth.
        let (backend, spawn_fn): (&'static str, fn(u32) -> Option<Child>) = if which("pw-cat") {
            ("pw-cat", spawn_pw_cat)
        } else if which("aplay") {
            ("aplay", spawn_aplay)
        } else {
            ("none", |_| None)
        };

        let join = thread::Builder::new()
            .name("fairy-audio".into())
            .spawn(move || host_pipe_loop(ring, stop2, spawn_fn, pid2, rate2))
            .ok();

        if backend == "none" {
            eprintln!("  audio: FAILED — need aplay or pw-cat");
        } else {
            eprintln!("  audio: {backend} (48 kHz stereo; open after prebuffer)");
        }

        Self {
            join,
            stop,
            player_pid,
            game_rate,
            backend,
        }
    }

    pub fn stop(mut self) {
        if let Ok(mut s) = self.stop.lock() {
            *s = true;
        }
        if let Ok(guard) = self.player_pid.lock() {
            if let Some(pid) = *guard {
                let _ = Command::new("kill")
                    .args(["-TERM", &pid.to_string()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        }
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

fn which(bin: &str) -> bool {
    Command::new("which")
        .arg(bin)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn spawn_aplay(rate: u32) -> Option<Child> {
    Command::new("aplay")
        .args([
            "-t", "raw", "-f", "S16_LE", "-c", "2",
            "-r", &rate.to_string(),
            "--buffer-time=250000",
            "--period-time=20000",
            "-q", "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()
}

fn spawn_pw_cat(rate: u32) -> Option<Child> {
    Command::new("pw-cat")
        .args([
            "--playback", "--raw", "--format", "s16",
            "--rate", &rate.to_string(),
            "--channels", "2", "--latency", "200ms", "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()
}

fn host_pipe_loop(
    ring: SampleRing,
    stop: Arc<Mutex<bool>>,
    spawn: fn(u32) -> Option<Child>,
    player_pid: Arc<Mutex<Option<u32>>>,
    game_rate: Arc<Mutex<u32>>,
) {
    let mut child: Option<Child> = None;
    let mut stdin: Option<std::process::ChildStdin> = None;
    let mut rs = PullResampler::new();
    // 20 ms of stereo frames at 48 kHz → 960 frames × 2 i16
    let frames = (HOST_RATE / 50) as usize;
    let mut out = vec![0i16; frames * 2];
    let mut buf_bytes = Vec::with_capacity(out.len() * 2);
    let chunk = Duration::from_nanos(1_000_000_000 * frames as u64 / HOST_RATE as u64);
    let mut deadline = Instant::now();

    loop {
        if stop.lock().map(|s| *s).unwrap_or(true) {
            break;
        }

        let src_rate = game_rate.lock().map(|r| *r).unwrap_or(0);
        // Do not open the device (or invent a rate) until the FIFO timer has
        // locked AND the ring has a fat prebuffer. Opening aplay/pw-cat at
        // boot and starving it made PipeWire loop the last period forever.
        // ~250 ms. Overworld frames hitch on m4a SoundVSync (every 7
        // VBlanks). A 125 ms ring went empty and hold-last gated the BGM.
        let prebuffer = (src_rate as usize / 4).max(1024);
        if child.is_none() {
            if src_rate < 4000 || ring.frames() < prebuffer {
                thread::sleep(Duration::from_millis(10));
                continue;
            }
            child = spawn(HOST_RATE);
            if let Some(ref c) = child {
                *player_pid.lock().unwrap() = Some(c.id());
                eprintln!(
                    "  audio: host open @ {HOST_RATE} Hz stereo ({} frames buffered)",
                    ring.frames()
                );
            } else {
                *player_pid.lock().unwrap() = None;
                thread::sleep(Duration::from_millis(50));
                continue;
            }
            stdin = child.as_mut().and_then(|c| c.stdin.take());
            deadline = Instant::now();
        }

        if src_rate < 4000 {
            deadline += chunk;
            let now = Instant::now();
            if deadline > now {
                thread::sleep(deadline - now);
            }
            continue;
        }

        rs.fill(&ring, src_rate, &mut out);
        buf_bytes.clear();
        for s in &out {
            buf_bytes.extend_from_slice(&s.to_le_bytes());
        }

        if let Some(ref mut sin) = stdin {
            if sin.write_all(&buf_bytes).is_err() {
                drop(stdin.take());
                if let Some(mut c) = child.take() {
                    let _ = c.kill();
                    let _ = c.wait();
                }
                *player_pid.lock().unwrap() = None;
                if stop.lock().map(|s| *s).unwrap_or(true) {
                    break;
                }
            }
        }

        // Exactly 10 ms of audio per loop. A shorter sleep ate the ring at 1.25×
        // and the play window then skipped frame waits to refill — chopped music.
        deadline += chunk;
        let now = Instant::now();
        if deadline > now {
            thread::sleep(deadline - now);
        } else if now.duration_since(deadline) > chunk * 3 {
            deadline = now;
        }
    }

    drop(stdin.take());
    if let Some(mut c) = child {
        let _ = c.kill();
        let _ = c.wait();
    }
    *player_pid.lock().unwrap() = None;
}
