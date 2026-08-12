//! Host audio: pipe raw game-rate samples to aplay / pw-cat.
//! Device resamples to output rate natively. No internal resampler, no fpos.

use super::resample::SampleRing;
use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

pub const SLEEP_MS: u64 = 5;

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

    pub fn start(ring: SampleRing) -> Self {
        let stop = Arc::new(Mutex::new(false));
        let stop2 = Arc::clone(&stop);
        let player_pid = Arc::new(Mutex::new(None));
        let pid2 = Arc::clone(&player_pid);
        let game_rate = Arc::new(Mutex::new(0u32));
        let rate2 = Arc::clone(&game_rate);

        let (backend, spawn_fn): (&'static str, fn(u32) -> Option<Child>) = if which("aplay") {
            ("aplay", spawn_aplay)
        } else if which("pw-cat") {
            ("pw-cat", spawn_pw_cat)
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
            eprintln!("  audio: {backend} (pipes game rate direct; device resamples)");
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
            "-t", "raw", "-f", "S16_LE", "-c", "1",
            "-r", &rate.to_string(),
            "--buffer-time=200000",
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
            "--channels", "1", "--latency", "80ms", "-",
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
    let mut last_rate: u32 = 0;
    let mut primed = false;
    let mut buf_bytes = Vec::with_capacity(4096);

    loop {
        if stop.lock().map(|s| *s).unwrap_or(true) {
            break;
        }

        let want = game_rate.lock().map(|r| *r).unwrap_or(0);
        if want < 8000 {
            thread::sleep(std::time::Duration::from_millis(SLEEP_MS));
            continue;
        }

        // Reopen if rate changes.
        if want != last_rate || child.is_none() {
            drop(stdin.take());
            if let Some(mut c) = child.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
            child = spawn(want);
            if let Some(ref c) = child {
                *player_pid.lock().unwrap() = Some(c.id());
            } else {
                *player_pid.lock().unwrap() = None;
                thread::sleep(std::time::Duration::from_millis(20));
                continue;
            }
            stdin = child.as_mut().and_then(|c| c.stdin.take());
            last_rate = want;
            primed = false;
            eprintln!("  audio: host open @ {want} Hz (game rate direct)");
        }

        // Prime ~80 ms of game audio before first write.
        if !primed {
            let need = (want as usize * 80 / 1000).max(512);
            if ring.len() < need {
                thread::sleep(std::time::Duration::from_millis(2));
                continue;
            }
            primed = true;
        }

        // Drain everything available from the ring (up to 4 KiB).
        let take = 2048usize.min(ring.len());
        if take == 0 {
            thread::sleep(std::time::Duration::from_millis(SLEEP_MS));
            continue;
        }

        buf_bytes.clear();
        let mut underruns = 0usize;
        if let Ok(mut r) = ring.inner.lock() {
            for _ in 0..take {
                match r.pop_front() {
                    Some(s) => buf_bytes.extend_from_slice(&s.to_le_bytes()),
                    None => { underruns += 1; buf_bytes.extend_from_slice(&0i16.to_le_bytes()); }
                }
            }
        }
        let _ = underruns;

        if buf_bytes.is_empty() {
            thread::sleep(std::time::Duration::from_millis(SLEEP_MS));
            continue;
        }

        if let Some(ref mut sin) = stdin {
            if sin.write_all(&buf_bytes).is_err() {
                drop(stdin.take());
                if let Some(mut c) = child.take() {
                    let _ = c.kill();
                    let _ = c.wait();
                }
                *player_pid.lock().unwrap() = None;
                last_rate = 0;
                primed = false;
                if stop.lock().map(|s| *s).unwrap_or(true) {
                    break;
                }
            } else {
                let _ = sin.flush();
            }
        } else {
            thread::sleep(std::time::Duration::from_millis(SLEEP_MS));
        }
    }

    drop(stdin.take());
    if let Some(mut c) = child {
        let _ = c.kill();
        let _ = c.wait();
    }
    *player_pid.lock().unwrap() = None;
}
