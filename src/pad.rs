//! Host gamepad → GBA `KEYINPUT` bits.
//!
//! Keyboard stays authoritative. A Linux pad is OR-ed into the same 10-bit
//! mask. Missing / vanished device = keyboard only (not a hang).
//!
//! Backends, in order:
//! 1. evdev (`BTN_SOUTH` / hats) — xpad, hid-playstation, hid-nintendo
//! 2. `/dev/input/js*` — older joystick nodes
//! 3. raw USB Xbox-360 report (`045e:028e`) when the kernel bound nothing
//!
//! Default face map is **Nintendo** (east = GBA A, south = GBA B) so an
//! Action Battletron / Switch-layout pad confirms with the right-hand
//! button. `FAIRY_PAD=xbox` swaps that.

use std::fs::{File, OpenOptions, read_dir};
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub const KEY_A: u16 = 1 << 0;
pub const KEY_B: u16 = 1 << 1;
pub const KEY_SELECT: u16 = 1 << 2;
pub const KEY_START: u16 = 1 << 3;
pub const KEY_RIGHT: u16 = 1 << 4;
pub const KEY_LEFT: u16 = 1 << 5;
pub const KEY_UP: u16 = 1 << 6;
pub const KEY_DOWN: u16 = 1 << 7;
pub const KEY_R: u16 = 1 << 8;
pub const KEY_L: u16 = 1 << 9;

const JS_EVENT_BUTTON: u8 = 0x01;
const JS_EVENT_AXIS: u8 = 0x02;
const JS_EVENT_INIT: u8 = 0x80;
const STICK_DEADZONE: i16 = 16_384;
const TRIGGER_ON: i32 = 128; // ABS_Z / ABS_RZ are 0..=255; half-pull to save/load

const EV_KEY: u16 = 1;
const EV_ABS: u16 = 3;
const BTN_SOUTH: u16 = 0x130;
const BTN_EAST: u16 = 0x131;
const BTN_NORTH: u16 = 0x133;
const BTN_WEST: u16 = 0x134;
const BTN_TL: u16 = 0x136;
const BTN_TR: u16 = 0x137;
const BTN_TL2: u16 = 0x138;
const BTN_TR2: u16 = 0x139;
const BTN_SELECT: u16 = 0x13a;
const BTN_START: u16 = 0x13b;
const BTN_MODE: u16 = 0x13c;
const BTN_THUMBL: u16 = 0x13d;
const BTN_THUMBR: u16 = 0x13e;
const ABS_X: u16 = 0;
const ABS_Y: u16 = 1;
const ABS_Z: u16 = 2;
const ABS_RZ: u16 = 5;
const ABS_HAT0X: u16 = 0x10;
const ABS_HAT0Y: u16 = 0x11;

const USB_VID_MS: u16 = 0x045E;
const USB_PID_X360: u16 = 0x028E;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    /// East face = GBA A, south = GBA B (Switch / this Battletron / DS4-as-Nintendo).
    Nintendo,
    /// South face = GBA A, east = GBA B (Xbox).
    Xbox,
}

impl Profile {
    pub fn from_env() -> Self {
        match std::env::var("FAIRY_PAD").ok().as_deref() {
            Some(s) if s.eq_ignore_ascii_case("xbox") => Profile::Xbox,
            _ => Profile::Nintendo,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Profile::Nintendo => "nintendo A=east",
            Profile::Xbox => "xbox A=south",
        }
    }

    fn face_a(self) -> u16 {
        match self {
            Profile::Nintendo => KEY_A, // caller passes east
            Profile::Xbox => KEY_B,
        }
    }

    fn map_south_east(self, south: bool, east: bool, pressed: u16) -> u16 {
        let mut m = pressed & !(KEY_A | KEY_B);
        match self {
            Profile::Nintendo => {
                if east {
                    m |= KEY_A;
                }
                if south {
                    m |= KEY_B;
                }
            }
            Profile::Xbox => {
                if south {
                    m |= KEY_A;
                }
                if east {
                    m |= KEY_B;
                }
            }
        }
        let _ = self.face_a();
        m
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct JsEvent {
    time: u32,
    value: i16,
    typ: u8,
    number: u8,
}

enum Backend {
    None,
    Evdev {
        file: File,
        path: PathBuf,
        name: String,
        south: bool,
        east: bool,
        hat_x: i32,
        hat_y: i32,
        stick_x: i16,
        stick_y: i16,
        sh_l: bool,
        sh_r: bool,
    },
    Js {
        file: File,
        path: PathBuf,
        name: String,
    },
}

pub struct Pad {
    backend: Backend,
    pressed: u16,
    profile: Profile,
    last_try: Instant,
    announced: bool,
    /// Physical X (west). Host-only — not a GBA key.
    host_west: bool,
    /// Physical Y (north). Host-only — not a GBA key.
    host_north: bool,
    /// L2 / LT — not used for savestate (M2 clones R2 on this pad).
    host_l2: bool,
    host_r2: bool,
    /// Stick clicks. Host savestate hold-to-save / hold-to-load.
    host_l3: bool,
    host_r3: bool,
}

impl Pad {
    pub fn open() -> Self {
        let mut pad = Self {
            backend: Backend::None,
            pressed: 0,
            profile: Profile::from_env(),
            last_try: Instant::now() - Duration::from_secs(3),
            announced: false,
            host_west: false,
            host_north: false,
            host_l2: false,
            host_r2: false,
            host_l3: false,
            host_r3: false,
        };
        pad.try_open(true);
        pad
    }

    pub fn profile(&self) -> Profile {
        self.profile
    }

    pub fn describe(&self) -> String {
        match &self.backend {
            Backend::None => format!("none ({})", self.profile.label()),
            Backend::Evdev { path, name, .. } => {
                format!("{name} ({}) {}", path.display(), self.profile.label())
            }
            Backend::Js { path, name, .. } => {
                format!("{name} js {} {}", path.display(), self.profile.label())
            }
        }
    }

    fn try_open(&mut self, log: bool) {
        self.last_try = Instant::now();
        if let Some((file, path, name)) = open_evdev() {
            if log {
                eprintln!("  pad: {name} ({}) {}", path.display(), self.profile.label());
            }
            self.backend = Backend::Evdev {
                file,
                path,
                name,
                south: false,
                east: false,
                hat_x: 0,
                hat_y: 0,
                stick_x: 0,
                stick_y: 0,
                sh_l: false,
                sh_r: false,
            };
            self.announced = true;
            return;
        }
        if let Some((file, path, name)) = open_js() {
            if log {
                eprintln!(
                    "  pad: {name} ({}) {} [js]",
                    path.display(),
                    self.profile.label()
                );
            }
            self.backend = Backend::Js { file, path, name };
            self.announced = true;
            return;
        }
        if log && !self.announced {
            eprintln!("  pad: none (keyboard only) · {}", self.profile.label());
            self.announced = true;
        }
    }

    /// Current pressed-bit mask (1 = down). Drain every pending event.
    pub fn poll(&mut self) -> u16 {
        if matches!(self.backend, Backend::None)
            && self.last_try.elapsed() >= Duration::from_secs(2)
        {
            self.try_open(true);
        }
        let ok = match &mut self.backend {
            Backend::None => true,
            Backend::Evdev {
                file,
                south,
                east,
                hat_x,
                hat_y,
                stick_x,
                stick_y,
                sh_l,
                sh_r,
                ..
            } => match drain_evdev(
                file,
                &mut self.pressed,
                &mut *south,
                &mut *east,
                &mut *hat_x,
                &mut *hat_y,
                &mut *stick_x,
                &mut *stick_y,
                &mut *sh_l,
                &mut *sh_r,
                &mut self.host_west,
                &mut self.host_north,
                &mut self.host_l2,
                &mut self.host_r2,
                &mut self.host_l3,
                &mut self.host_r3,
                self.profile,
            )
            {
                Ok(()) => true,
                Err(e) if e.kind() == io::ErrorKind::BrokenPipe
                    || e.kind() == io::ErrorKind::NotFound
                    || e.raw_os_error() == Some(libc::ENODEV) =>
                {
                    false
                }
                Err(_) => true,
            },
            Backend::Js { file, .. } => match drain_js(
                file,
                &mut self.pressed,
                &mut self.host_west,
                &mut self.host_north,
                &mut self.host_l2,
                &mut self.host_r2,
                &mut self.host_l3,
                &mut self.host_r3,
                self.profile,
            ) {
                Ok(()) => true,
                Err(e) if e.kind() == io::ErrorKind::BrokenPipe
                    || e.kind() == io::ErrorKind::NotFound
                    || e.raw_os_error() == Some(libc::ENODEV) =>
                {
                    false
                }
                Err(_) => true,
            },
        };
        if !ok {
            eprintln!("  pad: gone (keyboard only)");
            self.backend = Backend::None;
            self.pressed = 0;
            self.host_west = false;
            self.host_north = false;
            self.host_l2 = false;
            self.host_r2 = false;
            self.host_l3 = false;
            self.host_r3 = false;
            self.last_try = Instant::now();
        }
        self.pressed
    }

    /// Physical X (west) and Y (north). Used for host turbo, not KEYINPUT.
    pub fn host_xy(&self) -> (bool, bool) {
        (self.host_west, self.host_north)
    }

    pub fn host_triggers(&self) -> (bool, bool) {
        (self.host_l2, self.host_r2)
    }

    /// L3 / R3 stick clicks (host savestate hold).
    pub fn host_sticks(&self) -> (bool, bool) {
        (self.host_l3, self.host_r3)
    }
}

/// Headless listing + optional live event dump (`fairy pad`).
pub fn probe(watch: bool) -> anyhow::Result<()> {
    let profile = Profile::from_env();
    println!("✦ Fairy pad probe · profile {}", profile.label());
    println!("  FAIRY_PAD=nintendo|xbox  (default nintendo — east=A south=B)");
    println!();
    println!("USB 360 clones (045e:028e):");
    let usb = list_usb_x360();
    if usb.is_empty() {
        println!("  (none)");
    } else {
        for u in &usb {
            println!("  {u}");
        }
    }
    println!();
    println!("evdev gamepads:");
    let evs = list_evdev();
    if evs.is_empty() {
        println!("  (none)");
    } else {
        for (path, name, keys) in &evs {
            println!("  {}  {name}  keys={keys}", path.display());
        }
    }
    println!();
    println!("js nodes:");
    let jss = list_js();
    if jss.is_empty() {
        println!("  (none)");
    } else {
        for (path, name, nb, na) in &jss {
            println!("  {}  {name}  buttons={nb} axes={na}", path.display());
        }
    }

    if !watch {
        println!();
        println!("  fairy pad --watch   print live events (Ctrl+C to stop)");
        return Ok(());
    }

    let mut pad = Pad::open();
    if matches!(pad.backend, Backend::None) {
        anyhow::bail!("no pad — plug USB (data cable) or pair, then retry");
    }
    println!();
    println!("watching {} — press A/B/L/R/Start/Select/D-pad/stick (X/Y = turbo host)", pad.describe());
    let mut last = 0u16;
    let mut last_xy = (false, false);
    let mut last_tr = (false, false);
    loop {
        let now = pad.poll();
        let xy = pad.host_xy();
        let tr = pad.host_triggers();
        if now != last || xy != last_xy || tr != last_tr {
            let host = match xy {
                (false, false) => String::new(),
                (w, n) => format!(
                    "  host{}",
                    if w && n {
                        " X+Y"
                    } else if w {
                        " X"
                    } else {
                        " Y"
                    }
                ),
            };
            let (l2, r2) = tr;
            let trig = match (l2, r2) {
                (false, false) => String::new(),
                (true, true) => " L2+R2".into(),
                (true, false) => " L2".into(),
                (false, true) => " R2".into(),
            };
            println!("  KEYINPUT {now:03X}  {}{host}{trig}", mask_label(now));
            last = now;
            last_xy = xy;
            last_tr = tr;
        }
        std::thread::sleep(Duration::from_millis(8));
    }
}

fn mask_label(m: u16) -> String {
    let mut v = Vec::new();
    if m & KEY_A != 0 {
        v.push("A");
    }
    if m & KEY_B != 0 {
        v.push("B");
    }
    if m & KEY_SELECT != 0 {
        v.push("Select");
    }
    if m & KEY_START != 0 {
        v.push("Start");
    }
    if m & KEY_RIGHT != 0 {
        v.push("Right");
    }
    if m & KEY_LEFT != 0 {
        v.push("Left");
    }
    if m & KEY_UP != 0 {
        v.push("Up");
    }
    if m & KEY_DOWN != 0 {
        v.push("Down");
    }
    if m & KEY_R != 0 {
        v.push("R");
    }
    if m & KEY_L != 0 {
        v.push("L");
    }
    if v.is_empty() {
        "(none)".into()
    } else {
        v.join("+")
    }
}

// ---------------------------------------------------------------------------
// evdev
// ---------------------------------------------------------------------------

const fn ioc(dir: u32, typ: u8, nr: u32, size: u32) -> libc::c_ulong {
    ((dir as u64) << 30 | (size as u64) << 16 | (typ as u64) << 8 | nr as u64) as libc::c_ulong
}
const IOC_READ: u32 = 2;
fn eviocgname(len: u32) -> libc::c_ulong {
    ioc(IOC_READ, b'E', 0x06, len)
}
fn eviocgbit(ev: u32, len: u32) -> libc::c_ulong {
    ioc(IOC_READ, b'E', 0x20 + ev, len)
}

fn evdev_name(fd: i32) -> String {
    let mut buf = [0u8; 256];
    let r = unsafe { libc::ioctl(fd, eviocgname(256), buf.as_mut_ptr()) };
    if r < 0 {
        return "unknown".into();
    }
    let n = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

fn evdev_has_bit(fd: i32, ev: u32, code: u16) -> bool {
    let mut bits = [0u8; 96];
    let r = unsafe { libc::ioctl(fd, eviocgbit(ev, bits.len() as u32), bits.as_mut_ptr()) };
    if r < 0 {
        return false;
    }
    let i = code as usize;
    bits[i / 8] & (1 << (i % 8)) != 0
}

fn evdev_key_list(fd: i32) -> String {
    const CODES: &[(u16, &str)] = &[
        (BTN_SOUTH, "SOUTH"),
        (BTN_EAST, "EAST"),
        (BTN_NORTH, "NORTH"),
        (BTN_WEST, "WEST"),
        (BTN_TL, "TL"),
        (BTN_TR, "TR"),
        (BTN_TL2, "TL2"),
        (BTN_TR2, "TR2"),
        (BTN_SELECT, "SELECT"),
        (BTN_START, "START"),
        (BTN_MODE, "MODE"),
    ];
    CODES
        .iter()
        .filter(|(c, _)| evdev_has_bit(fd, EV_KEY as u32, *c))
        .map(|(_, n)| *n)
        .collect::<Vec<_>>()
        .join(",")
}

fn looks_like_gamepad(fd: i32, name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    if n.contains("touchpad") || n.contains("keyboard") || n.contains("mouse") {
        return false;
    }
    if evdev_has_bit(fd, EV_KEY as u32, BTN_SOUTH) {
        return true;
    }
    (n.contains("xbox") || n.contains("pad") || n.contains("joy") || n.contains("game"))
        && evdev_has_bit(fd, EV_ABS as u32, ABS_HAT0X)
}

fn open_evdev() -> Option<(File, PathBuf, String)> {
    for n in 0..32 {
        let path = PathBuf::from(format!("/dev/input/event{n}"));
        let Ok(file) = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)
        else {
            continue;
        };
        let fd = file.as_raw_fd();
        let name = evdev_name(fd);
        if looks_like_gamepad(fd, &name) {
            return Some((file, path, name));
        }
    }
    None
}

fn list_evdev() -> Vec<(PathBuf, String, String)> {
    let mut out = Vec::new();
    for n in 0..32 {
        let path = PathBuf::from(format!("/dev/input/event{n}"));
        let Ok(file) = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)
        else {
            continue;
        };
        let fd = file.as_raw_fd();
        let name = evdev_name(fd);
        if looks_like_gamepad(fd, &name) {
            out.push((path, name, evdev_key_list(fd)));
        }
    }
    out
}

fn drain_evdev(
    file: &mut File,
    pressed: &mut u16,
    south: &mut bool,
    east: &mut bool,
    hat_x: &mut i32,
    hat_y: &mut i32,
    stick_x: &mut i16,
    stick_y: &mut i16,
    sh_l: &mut bool,
    sh_r: &mut bool,
    host_west: &mut bool,
    host_north: &mut bool,
    host_l2: &mut bool,
    host_r2: &mut bool,
    host_l3: &mut bool,
    host_r3: &mut bool,
    profile: Profile,
) -> io::Result<()> {
    let mut buf = [0u8; 24];
    loop {
        match file.read(&mut buf) {
            Ok(24) => {
                let typ = u16::from_le_bytes([buf[16], buf[17]]);
                let code = u16::from_le_bytes([buf[18], buf[19]]);
                let value = i32::from_le_bytes([buf[20], buf[21], buf[22], buf[23]]);
                apply_evdev(
                    pressed, south, east, hat_x, hat_y, stick_x, stick_y, sh_l, sh_r, host_west,
                    host_north, host_l2, host_r2, host_l3, host_r3, profile, typ, code, value,
                );
            }
            Ok(0) => break,
            Ok(_) => break,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
            Err(e) if e.raw_os_error() == Some(libc::EAGAIN) => break,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn apply_evdev(
    pressed: &mut u16,
    south: &mut bool,
    east: &mut bool,
    hat_x: &mut i32,
    hat_y: &mut i32,
    stick_x: &mut i16,
    stick_y: &mut i16,
    sh_l: &mut bool,
    sh_r: &mut bool,
    host_west: &mut bool,
    host_north: &mut bool,
    host_l2: &mut bool,
    host_r2: &mut bool,
    host_l3: &mut bool,
    host_r3: &mut bool,
    profile: Profile,
    typ: u16,
    code: u16,
    value: i32,
) {
    match typ {
        EV_KEY => {
            let down = value != 0;
            match code {
                BTN_SOUTH => {
                    *south = down;
                    *pressed = profile.map_south_east(*south, *east, *pressed);
                }
                BTN_EAST => {
                    *east = down;
                    *pressed = profile.map_south_east(*south, *east, *pressed);
                }
                BTN_WEST => *host_west = down,
                BTN_NORTH => *host_north = down,
                BTN_TL => {
                    *sh_l = down;
                    set_bit(pressed, KEY_L, *sh_l);
                }
                BTN_TR => {
                    *sh_r = down;
                    set_bit(pressed, KEY_R, *sh_r);
                }
                BTN_TL2 => *host_l2 = down,
                BTN_TR2 => *host_r2 = down,
                BTN_THUMBL => *host_l3 = down,
                BTN_THUMBR => *host_r3 = down,
                BTN_SELECT => set_bit(pressed, KEY_SELECT, down),
                BTN_START => set_bit(pressed, KEY_START, down),
                _ => {}
            }
        }
        EV_ABS => {
            match code {
                ABS_HAT0X => *hat_x = value,
                ABS_HAT0Y => *hat_y = value,
                ABS_X => *stick_x = value as i16,
                ABS_Y => *stick_y = value as i16,
                ABS_Z => {
                    *host_l2 = value >= TRIGGER_ON;
                    return;
                }
                ABS_RZ => {
                    *host_r2 = value >= TRIGGER_ON;
                    return;
                }
                _ => return,
            }
            write_dirs(pressed, *hat_x, *hat_y, *stick_x, *stick_y);
        }
        _ => {}
    }
}

fn write_dirs(pressed: &mut u16, hat_x: i32, hat_y: i32, stick_x: i16, stick_y: i16) {
    *pressed &= !(KEY_LEFT | KEY_RIGHT | KEY_UP | KEY_DOWN);
    if hat_x < 0 || stick_x <= -STICK_DEADZONE {
        *pressed |= KEY_LEFT;
    }
    if hat_x > 0 || stick_x >= STICK_DEADZONE {
        *pressed |= KEY_RIGHT;
    }
    if hat_y < 0 || stick_y <= -STICK_DEADZONE {
        *pressed |= KEY_UP;
    }
    if hat_y > 0 || stick_y >= STICK_DEADZONE {
        *pressed |= KEY_DOWN;
    }
}

fn set_bit(mask: &mut u16, bit: u16, down: bool) {
    if down {
        *mask |= bit;
    } else {
        *mask &= !bit;
    }
}

fn apply_axis(pressed: &mut u16, neg: u16, pos: u16, value: i16) {
    *pressed &= !(neg | pos);
    if value <= -STICK_DEADZONE {
        *pressed |= neg;
    } else if value >= STICK_DEADZONE {
        *pressed |= pos;
    }
}

// ---------------------------------------------------------------------------
// joystick API fallback
// ---------------------------------------------------------------------------

fn open_js() -> Option<(File, PathBuf, String)> {
    for n in 0..4 {
        let path = PathBuf::from(format!("/dev/input/js{n}"));
        match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)
        {
            Ok(f) => {
                let _ = unsafe { libc::fcntl(f.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) };
                let name = js_name(f.as_raw_fd()).unwrap_or_else(|| format!("js{n}"));
                return Some((f, path, name));
            }
            Err(_) => continue,
        }
    }
    None
}

fn js_name(fd: i32) -> Option<String> {
    let mut buf = [0u8; 128];
    let req = ioc(IOC_READ, b'j', 0x13, 128);
    let r = unsafe { libc::ioctl(fd, req, buf.as_mut_ptr()) };
    if r < 0 {
        return None;
    }
    let n = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    Some(String::from_utf8_lossy(&buf[..n]).into_owned())
}

fn list_js() -> Vec<(PathBuf, String, u8, u8)> {
    let mut out = Vec::new();
    for n in 0..4 {
        let path = PathBuf::from(format!("/dev/input/js{n}"));
        let Ok(f) = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)
        else {
            continue;
        };
        let fd = f.as_raw_fd();
        let name = js_name(fd).unwrap_or_else(|| format!("js{n}"));
        let mut nb = 0u8;
        let mut na = 0u8;
        unsafe {
            libc::ioctl(fd, ioc(IOC_READ, b'j', 0x12, 1), &mut nb);
            libc::ioctl(fd, ioc(IOC_READ, b'j', 0x11, 1), &mut na);
        }
        out.push((path, name, nb, na));
    }
    out
}

fn drain_js(
    file: &mut File,
    pressed: &mut u16,
    host_west: &mut bool,
    host_north: &mut bool,
    host_l2: &mut bool,
    host_r2: &mut bool,
    host_l3: &mut bool,
    host_r3: &mut bool,
    profile: Profile,
) -> io::Result<()> {
    let mut buf = [0u8; 8];
    loop {
        match file.read(&mut buf) {
            Ok(8) => {
                let ev = JsEvent {
                    time: u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]),
                    value: i16::from_le_bytes([buf[4], buf[5]]),
                    typ: buf[6],
                    number: buf[7],
                };
                apply_js(
                    pressed, host_west, host_north, host_l2, host_r2, host_l3, host_r3, profile, ev,
                );
            }
            Ok(0) => break,
            Ok(_) => break,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
            Err(e) if e.raw_os_error() == Some(libc::EAGAIN) => break,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn apply_js(
    pressed: &mut u16,
    host_west: &mut bool,
    host_north: &mut bool,
    host_l2: &mut bool,
    host_r2: &mut bool,
    host_l3: &mut bool,
    host_r3: &mut bool,
    profile: Profile,
    ev: JsEvent,
) {
    let kind = ev.typ & !JS_EVENT_INIT;
    match kind {
        JS_EVENT_BUTTON => {
            // xpad js numbers: 0=A/south, 1=B/east, 2=X, 3=Y, 4=LB, 5=RB,
            // 6=Back, 7=Start, 8=Guide
            let down = ev.value != 0;
            match ev.number {
                0 => {
                    // south
                    let east = *pressed
                        & if profile == Profile::Nintendo {
                            KEY_A
                        } else {
                            KEY_B
                        }
                        != 0;
                    // reconstruct south/east from current mask is messy;
                    // apply via a tiny state-less map for this button only:
                    match profile {
                        Profile::Nintendo => set_bit(pressed, KEY_B, down),
                        Profile::Xbox => set_bit(pressed, KEY_A, down),
                    }
                    let _ = east;
                }
                1 => match profile {
                    Profile::Nintendo => set_bit(pressed, KEY_A, down),
                    Profile::Xbox => set_bit(pressed, KEY_B, down),
                },
                2 => *host_west = down,  // X
                3 => *host_north = down, // Y
                4 => set_bit(pressed, KEY_L, down),
                5 => set_bit(pressed, KEY_R, down),
                6 | 8 => set_bit(pressed, KEY_SELECT, down),
                7 => set_bit(pressed, KEY_START, down),
                9 => *host_l3 = down,
                10 => *host_r3 = down,
                _ => {}
            }
        }
        JS_EVENT_AXIS => {
            let (neg, pos) = match ev.number {
                0 | 6 => (KEY_LEFT, KEY_RIGHT),
                1 | 7 => (KEY_UP, KEY_DOWN),
                2 => {
                    *host_l2 = ev.value >= STICK_DEADZONE;
                    return;
                }
                5 => {
                    *host_r2 = ev.value >= STICK_DEADZONE;
                    return;
                }
                _ => return,
            };
            apply_axis(pressed, neg, pos, ev.value);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// USB 360 clone discovery + report decoder (for probe + future raw backend)
// ---------------------------------------------------------------------------

fn list_usb_x360() -> Vec<String> {
    let mut out = Vec::new();
    let Ok(dir) = read_dir("/sys/bus/usb/devices") else {
        return out;
    };
    for ent in dir.flatten() {
        let p = ent.path();
        let vid = std::fs::read_to_string(p.join("idVendor")).ok();
        let pid = std::fs::read_to_string(p.join("idProduct")).ok();
        let (Some(vid), Some(pid)) = (vid, pid) else {
            continue;
        };
        let vid = u16::from_str_radix(vid.trim(), 16).unwrap_or(0);
        let pid = u16::from_str_radix(pid.trim(), 16).unwrap_or(0);
        if vid != USB_VID_MS || pid != USB_PID_X360 {
            continue;
        }
        let product = std::fs::read_to_string(p.join("product"))
            .unwrap_or_else(|_| "XBOX 360".into());
        let bus = std::fs::read_to_string(p.join("busnum")).unwrap_or_default();
        let dev = std::fs::read_to_string(p.join("devnum")).unwrap_or_default();
        out.push(format!(
            "{:04x}:{:04x}  {}  usb {}/{}  {}",
            vid,
            pid,
            product.trim(),
            bus.trim(),
            dev.trim(),
            p.display()
        ));
    }
    out
}

/// Wired Xbox 360 / fake-360 20-byte input report → GBA mask.
pub fn decode_xinput360(report: &[u8], profile: Profile) -> u16 {
    if report.len() < 14 {
        return 0;
    }
    let b2 = report[2];
    let b3 = report[3];
    let mut m = 0u16;
    if b2 & 0x01 != 0 {
        m |= KEY_UP;
    }
    if b2 & 0x02 != 0 {
        m |= KEY_DOWN;
    }
    if b2 & 0x04 != 0 {
        m |= KEY_LEFT;
    }
    if b2 & 0x08 != 0 {
        m |= KEY_RIGHT;
    }
    if b2 & 0x10 != 0 {
        m |= KEY_START;
    }
    if b2 & 0x20 != 0 {
        m |= KEY_SELECT;
    }
    if b3 & 0x01 != 0 {
        m |= KEY_L;
    }
    if b3 & 0x02 != 0 {
        m |= KEY_R;
    }
    let south = b3 & 0x10 != 0; // A
    let east = b3 & 0x20 != 0; // B
    m = profile.map_south_east(south, east, m);
    let lx = i16::from_le_bytes([report[6], report[7]]);
    let ly = i16::from_le_bytes([report[8], report[9]]);
    if lx <= -STICK_DEADZONE {
        m |= KEY_LEFT;
    } else if lx >= STICK_DEADZONE {
        m |= KEY_RIGHT;
    }
    if ly <= -STICK_DEADZONE {
        m |= KEY_UP;
    } else if ly >= STICK_DEADZONE {
        m |= KEY_DOWN;
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_device_is_zero() {
        let mut p = Pad {
            backend: Backend::None,
            pressed: 0,
            profile: Profile::Nintendo,
            last_try: Instant::now(),
            announced: true,
            host_west: false,
            host_north: false,
            host_l2: false,
            host_r2: false,
            host_l3: false,
            host_r3: false,
        };
        assert_eq!(p.poll(), 0);
    }

    struct Tap {
        p: u16,
        s: bool,
        e: bool,
        hx: i32,
        hy: i32,
        sx: i16,
        sy: i16,
        sl: bool,
        sr: bool,
        west: bool,
        north: bool,
        l2: bool,
        r2: bool,
        l3: bool,
        r3: bool,
    }
    impl Tap {
        fn new() -> Self {
            Self {
                p: 0,
                s: false,
                e: false,
                hx: 0,
                hy: 0,
                sx: 0,
                sy: 0,
                sl: false,
                sr: false,
                west: false,
                north: false,
                l2: false,
                r2: false,
                l3: false,
                r3: false,
            }
        }
        fn ev(&mut self, profile: Profile, typ: u16, code: u16, value: i32) {
            apply_evdev(
                &mut self.p,
                &mut self.s,
                &mut self.e,
                &mut self.hx,
                &mut self.hy,
                &mut self.sx,
                &mut self.sy,
                &mut self.sl,
                &mut self.sr,
                &mut self.west,
                &mut self.north,
                &mut self.l2,
                &mut self.r2,
                &mut self.l3,
                &mut self.r3,
                profile,
                typ,
                code,
                value,
            );
        }
    }

    #[test]
    fn face_xy_are_host_only() {
        let mut t = Tap::new();
        t.ev(Profile::Nintendo, EV_KEY, BTN_WEST, 1);
        t.ev(Profile::Nintendo, EV_KEY, BTN_NORTH, 1);
        assert_eq!(t.p, 0, "X/Y must not reach KEYINPUT");
        assert!(t.west && t.north);
    }

    #[test]
    fn l2_r2_are_host_only() {
        let mut t = Tap::new();
        t.ev(Profile::Nintendo, EV_ABS, ABS_Z, 200);
        t.ev(Profile::Nintendo, EV_ABS, ABS_RZ, 200);
        assert_eq!(t.p, 0, "L2/R2 must not reach KEYINPUT");
        assert!(t.l2 && t.r2);
        t.ev(Profile::Nintendo, EV_KEY, BTN_TL, 1);
        assert_eq!(t.p & KEY_L, KEY_L, "digital L still GBA L");
    }

    #[test]
    fn stick_clicks_are_host_only() {
        let mut t = Tap::new();
        t.ev(Profile::Nintendo, EV_KEY, BTN_THUMBL, 1);
        t.ev(Profile::Nintendo, EV_KEY, BTN_THUMBR, 1);
        assert_eq!(t.p, 0);
        assert!(t.l3 && t.r3);
    }

    #[test]
    fn nintendo_east_is_gba_a() {
        let mut t = Tap::new();
        t.ev(Profile::Nintendo, EV_KEY, BTN_EAST, 1);
        assert_eq!(t.p & KEY_A, KEY_A);
        assert_eq!(t.p & KEY_B, 0);
        t.ev(Profile::Nintendo, EV_KEY, BTN_SOUTH, 1);
        assert_eq!(t.p & KEY_B, KEY_B);
    }

    #[test]
    fn xbox_south_is_gba_a() {
        let mut t = Tap::new();
        t.ev(Profile::Xbox, EV_KEY, BTN_SOUTH, 1);
        assert_eq!(t.p & KEY_A, KEY_A);
        assert_eq!(t.p & KEY_B, 0);
    }

    #[test]
    fn hat_and_stick() {
        let mut t = Tap::new();
        t.ev(Profile::Nintendo, EV_ABS, ABS_HAT0X, -1);
        assert_eq!(t.p & KEY_LEFT, KEY_LEFT);
        t.ev(Profile::Nintendo, EV_ABS, ABS_HAT0X, 0);
        assert_eq!(t.p & KEY_LEFT, 0);
        t.ev(Profile::Nintendo, EV_ABS, ABS_Y, -32_000);
        assert_eq!(t.p & KEY_UP, KEY_UP);
        t.ev(Profile::Nintendo, EV_ABS, ABS_HAT0X, -1);
        t.ev(Profile::Nintendo, EV_ABS, ABS_X, 0);
        assert_eq!(t.p & KEY_LEFT, KEY_LEFT, "hat survives stick center");
    }

    #[test]
    fn xinput360_nintendo_b_is_east() {
        let mut r = [0u8; 20];
        r[1] = 0x14;
        r[3] = 0x20; // Xbox B = east
        assert_eq!(decode_xinput360(&r, Profile::Nintendo) & KEY_A, KEY_A);
        r[3] = 0x10; // Xbox A = south
        assert_eq!(decode_xinput360(&r, Profile::Nintendo) & KEY_B, KEY_B);
        r[3] = 0x10;
        assert_eq!(decode_xinput360(&r, Profile::Xbox) & KEY_A, KEY_A);
    }

    #[test]
    fn js_button_and_axis_or_into_mask() {
        let mut pressed = 0u16;
        let mut west = false;
        let mut north = false;
        let mut l2 = false;
        let mut r2 = false;
        let mut l3 = false;
        let mut r3 = false;
        apply_js(
            &mut pressed,
            &mut west,
            &mut north,
            &mut l2,
            &mut r2,
            &mut l3,
            &mut r3,
            Profile::Xbox,
            JsEvent {
                time: 0,
                value: 1,
                typ: JS_EVENT_BUTTON,
                number: 0,
            },
        );
        apply_js(
            &mut pressed,
            &mut west,
            &mut north,
            &mut l2,
            &mut r2,
            &mut l3,
            &mut r3,
            Profile::Xbox,
            JsEvent {
                time: 0,
                value: 32767,
                typ: JS_EVENT_AXIS,
                number: 0,
            },
        );
        assert_eq!(pressed & KEY_A, KEY_A);
        assert_eq!(pressed & KEY_RIGHT, KEY_RIGHT);
    }
}
