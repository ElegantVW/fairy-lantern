//! Xbox-360-clone LED / RGB probe.
//!
//! `xpad0` is the player-ring command, not RGB. Vendor OUT reports on the
//! extra interfaces might move the Battletron rings — this module tries
//! known 360 LED packets and a small set of RGB candidates. Watch the pad.

use anyhow::{bail, Context, Result};
use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

const VID: u16 = 0x045E;
const PID: u16 = 0x028E;

const IOC_NONE: u32 = 0;
const IOC_WRITE: u32 = 1;
const IOC_READ: u32 = 2;
const IOC_RDWR: u32 = IOC_READ | IOC_WRITE;

const fn ioc(dir: u32, typ: u8, nr: u32, size: u32) -> libc::c_ulong {
    ((dir as u64) << 30 | (size as u64) << 16 | (typ as u64) << 8 | nr as u64) as libc::c_ulong
}

// linux/usbdevice_fs.h — sizes for 64-bit
const USBDEVFS_BULK: libc::c_ulong = ioc(IOC_RDWR, b'U', 2, 24);
const USBDEVFS_CLAIMINTERFACE: libc::c_ulong = ioc(IOC_READ, b'U', 15, 4);
const USBDEVFS_RELEASEINTERFACE: libc::c_ulong = ioc(IOC_READ, b'U', 16, 4);
const USBDEVFS_IOCTL: libc::c_ulong = ioc(IOC_RDWR, b'U', 18, 16);
const USBDEVFS_DISCONNECT: libc::c_ulong = ioc(IOC_NONE, b'U', 22, 0);
const USBDEVFS_CONNECT: libc::c_ulong = ioc(IOC_NONE, b'U', 23, 0);

#[repr(C)]
struct UsbBulk {
    ep: u32,
    len: u32,
    timeout: u32,
    data: *mut u8,
}

#[repr(C)]
struct UsbIoctl {
    ifno: i32,
    ioctl_code: i32,
    data: *mut libc::c_void,
}

pub fn probe_led(unbind: bool) -> Result<()> {
    println!("✦ Fairy pad LED probe");
    println!("  watch the Battletron rings / guide light.");
    println!("  xpad0 is the 360 *player* LED, not RGB.");
    println!();

    let usb = find_x360()?;
    println!(
        "USB  {:04x}:{:04x}  bus {} dev {}  {}",
        VID,
        PID,
        usb.bus,
        usb.dev,
        usb.sys.display()
    );
    println!("node {}", usb.devnode.display());
    if let Some(ref p) = usb.xpad_led {
        println!("led  {}", p.display());
    } else {
        println!("led  (no /sys/class/leds/xpad*)");
    }
    println!();

    step_xpad_sysfs(usb.xpad_led.as_deref());
    println!();

    let mut file = match open_usb(&usb.devnode) {
        Ok(f) => f,
        Err(e) => {
            println!("usbfs: cannot open {} ({e})", usb.devnode.display());
            println!("  tip: sudo cp scripts/99-fairy-pad.rules /etc/udev/rules.d/");
            println!("       sudo udevadm control --reload && replug the pad");
            return Ok(());
        }
    };

    if unbind {
        println!("--- unbind xpad (input will drop for a moment) ---");
        for iface in 0..2u32 {
            let _ = usb_disconnect(&file, iface);
        }
        thread::sleep(Duration::from_millis(200));
    }

    println!("--- Xbox LED reports on iface 0 ep 0x02 (01 03 mode) ---");
    for mode in [0u8, 2, 3, 6, 10] {
        let pkt = [0x01, 0x03, mode];
        send_out(&mut file, 0, 0x02, &pkt, unbind);
        thread::sleep(Duration::from_millis(700));
    }
    // restore player-1-ish
    let _ = send_out(&mut file, 0, 0x02, &[0x01, 0x03, 2], unbind);
    println!();

    println!("--- RGB candidate OUT packets (red / green / blue / wisp-pink) ---");
    let colors: [(&str, u8, u8, u8); 4] = [
        ("red", 255, 0, 0),
        ("green", 0, 255, 0),
        ("blue", 0, 0, 255),
        ("wisp-pink", 255, 77, 154),
    ];
    let templates: &[(&str, fn(u8, u8, u8) -> Vec<u8>)] = &[
        ("rgb", |r, g, b| vec![r, g, b]),
        ("00 rgb", |r, g, b| vec![0x00, r, g, b]),
        ("02 rgb", |r, g, b| vec![0x02, r, g, b]),
        ("03 rgb", |r, g, b| vec![0x03, r, g, b]),
        ("0a rgb", |r, g, b| vec![0x0a, r, g, b]),
        ("02 08 rgb", |r, g, b| vec![0x02, 0x08, r, g, b, 0, 0, 0]),
        ("0c rgb", |r, g, b| vec![0x0c, r, g, b]),
    ];

    for iface in [0u32, 1] {
        let ep = if iface == 0 { 0x02u8 } else { 0x04 };
        println!("iface {iface} ep {ep:#04x}");
        for (tname, build) in templates {
            for (cname, r, g, b) in colors {
                let pkt = build(r, g, b);
                print!("  {tname} {cname:9} ");
                send_out(&mut file, iface, ep, &pkt, unbind);
                thread::sleep(Duration::from_millis(450));
            }
        }
    }

    if unbind {
        for iface in 0..2u32 {
            let _ = usb_connect(&file, iface);
        }
        println!("xpad rebound (if the kernel still has the device)");
    }

    println!();
    println!("tell me what moved:");
    println!("  • RGB rings changed on a named packet → we keep that report");
    println!("  • only the 360 player/guide LED moved → host cannot paint RGB");
    println!("  • nothing → rerun with  fairy pad --led --unbind");
    Ok(())
}

/// Poll `~/.config/wisp/current.json` and print the color we would send.
/// Does not pretend xpad0 is RGB.
pub fn follow_wisp() -> Result<()> {
    let path = wisp_current_path();
    println!("✦ follow Wisp  {}", path.display());
    println!("  no RGB OUT report is proven yet — printing the sidecar only.");
    println!("  run fairy pad --led and say which packet (if any) painted the rings.");
    println!("  Ctrl+C to stop");
    let mut last = String::new();
    loop {
        match fs::read_to_string(&path) {
            Ok(s) if s != last => {
                last = s.clone();
                match parse_current(&s) {
                    Ok(c) => println!(
                        "  wisp on={} hue={:?} sat={:?} temp={:?} rgb=#{:02x}{:02x}{:02x}",
                        c.on, c.hue, c.sat, c.temp, c.r, c.g, c.b
                    ),
                    Err(e) => println!("  bad current.json: {e}"),
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if last != "<missing>" {
                    println!("  (no {} yet — change a color in wisp)", path.display());
                    last = "<missing>".into();
                }
            }
            _ => {}
        }
        thread::sleep(Duration::from_millis(300));
    }
}

pub fn wisp_current_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".config/wisp/current.json")
}

struct Current {
    on: bool,
    hue: Option<u16>,
    sat: Option<u8>,
    temp: Option<u16>,
    r: u8,
    g: u8,
    b: u8,
}

fn parse_current(s: &str) -> Result<Current> {
    // tiny parser — avoid a serde dep. fields we care about are numbers/bools.
    let on = s.contains("\"on\": true") || s.contains("\"on\":true");
    let hue = json_u16(s, "hue");
    let sat = json_u16(s, "saturation").map(|v| v.min(100) as u8);
    let temp = json_u16(s, "color_temp");
    let (r, g, b) = if !on {
        (0, 0, 0)
    } else if temp.unwrap_or(0) > 0 {
        kelvin_to_rgb(temp.unwrap_or(3000))
    } else {
        hsv_to_rgb(hue.unwrap_or(0), sat.unwrap_or(100))
    };
    Ok(Current {
        on,
        hue,
        sat,
        temp,
        r,
        g,
        b,
    })
}

fn json_u16(s: &str, key: &str) -> Option<u16> {
    let pat = format!("\"{key}\"");
    let i = s.find(&pat)?;
    let rest = &s[i + pat.len()..];
    let rest = rest.trim_start().trim_start_matches(':').trim_start();
    let num: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    num.parse().ok()
}

/// HSV (hue 0–360, sat 0–100, value 100) → 8-bit RGB.
pub fn hsv_to_rgb(hue: u16, sat: u8) -> (u8, u8, u8) {
    let h = (hue % 360) as f32;
    let s = (sat.min(100) as f32) / 100.0;
    let v = 1.0;
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r1, g1, b1) = match h {
        n if n < 60.0 => (c, x, 0.0),
        n if n < 120.0 => (x, c, 0.0),
        n if n < 180.0 => (0.0, c, x),
        n if n < 240.0 => (0.0, x, c),
        n if n < 300.0 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    (
        ((r1 + m) * 255.0).round() as u8,
        ((g1 + m) * 255.0).round() as u8,
        ((b1 + m) * 255.0).round() as u8,
    )
}

/// Rough Tanner-Helland Kelvin → RGB, 2500–6500.
pub fn kelvin_to_rgb(k: u16) -> (u8, u8, u8) {
    let t = (k.clamp(1000, 10_000) as f32) / 100.0;
    let r = if t <= 66.0 {
        255.0
    } else {
        (329.698_73 * (t - 60.0).powf(-0.133_204_76)).clamp(0.0, 255.0)
    };
    let g = if t <= 66.0 {
        (99.470_8 * t.ln() - 161.119_57).clamp(0.0, 255.0)
    } else {
        (288.122_16 * (t - 60.0).powf(-0.075_514_85)).clamp(0.0, 255.0)
    };
    let b = if t >= 66.0 {
        255.0
    } else if t <= 19.0 {
        0.0
    } else {
        (138.517_73 * (t - 10.0).ln() - 305.044_8).clamp(0.0, 255.0)
    };
    (r.round() as u8, g.round() as u8, b.round() as u8)
}

struct X360 {
    bus: u32,
    dev: u32,
    sys: PathBuf,
    devnode: PathBuf,
    xpad_led: Option<PathBuf>,
}

fn find_x360() -> Result<X360> {
    let dir = fs::read_dir("/sys/bus/usb/devices").context("usb sysfs")?;
    for ent in dir.flatten() {
        let p = ent.path();
        let vid = fs::read_to_string(p.join("idVendor")).ok();
        let pid = fs::read_to_string(p.join("idProduct")).ok();
        let (Some(vid), Some(pid)) = (vid, pid) else {
            continue;
        };
        let vid = u16::from_str_radix(vid.trim(), 16).unwrap_or(0);
        let pid = u16::from_str_radix(pid.trim(), 16).unwrap_or(0);
        if vid != VID || pid != PID {
            continue;
        }
        let bus: u32 = fs::read_to_string(p.join("busnum"))
            .unwrap_or_default()
            .trim()
            .parse()
            .unwrap_or(0);
        let dev: u32 = fs::read_to_string(p.join("devnum"))
            .unwrap_or_default()
            .trim()
            .parse()
            .unwrap_or(0);
        let devnode = PathBuf::from(format!("/dev/bus/usb/{bus:03}/{dev:03}"));
        let xpad_led = find_xpad_led();
        return Ok(X360 {
            bus,
            dev,
            sys: p,
            devnode,
            xpad_led,
        });
    }
    bail!("no 045e:028e on USB — plug the Battletron data cable")
}

fn find_xpad_led() -> Option<PathBuf> {
    let dir = fs::read_dir("/sys/class/leds").ok()?;
    for ent in dir.flatten() {
        let name = ent.file_name();
        if name.to_string_lossy().starts_with("xpad") {
            return Some(ent.path().join("brightness"));
        }
    }
    None
}

fn step_xpad_sysfs(path: Option<&Path>) {
    println!("--- /sys/class/leds/xpad* brightness (player LED) ---");
    let Some(path) = path else {
        println!("  skip (no node)");
        return;
    };
    let orig = fs::read_to_string(path).unwrap_or_else(|_| "?".into());
    println!("  current {}", orig.trim());
    for v in [0u8, 2, 3, 6, 10] {
        print!("  brightness={v} ");
        match fs::write(path, v.to_string()) {
            Ok(()) => println!("ok — look at the guide / player light"),
            Err(e) => println!("FAIL ({e})"),
        }
        thread::sleep(Duration::from_millis(700));
    }
    let _ = fs::write(path, orig.trim());
}

fn open_usb(path: &Path) -> Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .with_context(|| format!("open {}", path.display()))
}

fn send_out(file: &mut File, iface: u32, ep: u8, pkt: &[u8], claimed: bool) {
    print!("{:02X?} ", pkt);
    if !claimed {
        if let Err(e) = usb_claim(file, iface) {
            println!("claim if{iface}: {e}");
            return;
        }
    }
    let r = usb_bulk(file, ep, pkt);
    if !claimed {
        let _ = usb_release(file, iface);
    }
    match r {
        Ok(n) => println!("sent {n}"),
        Err(e) => println!("{e}"),
    }
}

fn usb_claim(file: &File, iface: u32) -> Result<()> {
    let mut ifn = iface;
    let rc = unsafe { libc::ioctl(file.as_raw_fd(), USBDEVFS_CLAIMINTERFACE, &mut ifn) };
    if rc < 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EBUSY) {
            bail!("EBUSY (xpad owns iface {iface}; try --unbind)");
        }
        bail!("{err}");
    }
    Ok(())
}

fn usb_release(file: &File, iface: u32) -> Result<()> {
    let mut ifn = iface;
    let rc = unsafe { libc::ioctl(file.as_raw_fd(), USBDEVFS_RELEASEINTERFACE, &mut ifn) };
    if rc < 0 {
        bail!("{}", std::io::Error::last_os_error());
    }
    Ok(())
}

fn usb_disconnect(file: &File, iface: u32) -> Result<()> {
    let mut io = UsbIoctl {
        ifno: iface as i32,
        ioctl_code: USBDEVFS_DISCONNECT as i32,
        data: std::ptr::null_mut(),
    };
    let rc = unsafe { libc::ioctl(file.as_raw_fd(), USBDEVFS_IOCTL, &mut io) };
    if rc < 0 {
        bail!("disconnect if{iface}: {}", std::io::Error::last_os_error());
    }
    Ok(())
}

fn usb_connect(file: &File, iface: u32) -> Result<()> {
    let mut io = UsbIoctl {
        ifno: iface as i32,
        ioctl_code: USBDEVFS_CONNECT as i32,
        data: std::ptr::null_mut(),
    };
    let rc = unsafe { libc::ioctl(file.as_raw_fd(), USBDEVFS_IOCTL, &mut io) };
    if rc < 0 {
        bail!("connect if{iface}: {}", std::io::Error::last_os_error());
    }
    Ok(())
}

fn usb_bulk(file: &File, ep: u8, pkt: &[u8]) -> Result<usize> {
    let mut buf = pkt.to_vec();
    let mut xfer = UsbBulk {
        ep: ep as u32,
        len: buf.len() as u32,
        timeout: 200,
        data: buf.as_mut_ptr(),
    };
    let rc = unsafe { libc::ioctl(file.as_raw_fd(), USBDEVFS_BULK, &mut xfer) };
    if rc < 0 {
        bail!("{}", std::io::Error::last_os_error());
    }
    Ok(rc as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsv_pink_is_magenta_side() {
        let (r, g, b) = hsv_to_rgb(320, 70);
        assert!(r > 200 && b > 80 && g < r, "got #{r:02x}{g:02x}{b:02x}");
    }

    #[test]
    fn kelvin_warm_is_yellowish() {
        let (r, g, b) = kelvin_to_rgb(2700);
        assert!(r >= g && g > b);
    }

    #[test]
    fn parse_wisp_sidecar() {
        let s = r#"{"on":true,"hue":320,"saturation":70,"color_temp":0,"brightness":80}"#;
        let c = parse_current(s).unwrap();
        assert!(c.on);
        assert_eq!(c.hue, Some(320));
        assert_eq!(c.sat, Some(70));
        assert!(c.r > 0 && c.b > 0);
    }
}
