//! GBA BIOS audio SWIs (0x19–0x2B).
//!
//! SoundBias (0x19) and MidiKey2Freq (0x1F/0x2B) do real work.
//! SoundDriverMain / MusicPlayer* are **stubs**: they must not write a fake
//! PCM buffer into IWRAM. ROM-side mixers (mp2k, Smsh, …) own that memory.
//! Games that rely on the BIOS mixer will be silent until a real MP2K HLE exists.

use crate::bus::Bus;
use crate::cpu::Cpu;

// ---- m4a memory layout ------------------------------------------------

/// SoundArea (pointed to by SWI 0x1A r0).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
#[allow(dead_code)]
struct SoundAreaLayout {
    ident: u32,       // +00 magic e.g. 0x70536D61 (varies)
    song_count: u32,  // +04
    songs: u32,       // +08 song array ptr
    max_tn: u32,      // +0C max simultaneous channels
    reverb: u32,      // +10
    sample_rate: u32, // +14 plays frequency (Hz or base)
    freq_list: u32,   // +18
    player_list: u32, // +1C
    _pad: [u32; 4],
    ident2: u32,      // +30 second magic
}

/// SoundInfo (initialized by SoundDriverInit, used by Main).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
#[allow(dead_code)]
struct SoundInfoLayout {
    ident: u32,       // +00
    status: u32,      // +04
    channels: u32,    // +08 ptr to channel array (null = none)
    max_channels: u32,// +0C
    volume: u32,      // +10 master volume
    reverb: u32,      // +14
    da_flags: u32,    // +18 SOUNDCNT_H DMA config bits
    buffer_len: u32,  // +1C PWM/DMA buffer length in bytes
    buffer_ptr: u32,  // +20 DMA destination buffer (FIFO src in IWRAM)
    buffer_alloc: u32,// +24 total allocated bytes
    _pad: [u32; 2],
}

/// Channel header (m4a standard). 64 bytes per DS channel.
/// SOUNDCNT_H bits 10/14 configure which timer → DMA → FIFO.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
#[allow(dead_code)]
struct DsChannelLayout {
    status: u32,       // +00 0=free, 1=active
    typ: u32,          // +04 0=PCM8, 1=PCM8 signed, 2=ADPCM, 0x10.. coded
    sample_ptr: u32,   // +08 current read position
    sample_end: u32,   // +0C end-of-sample ptr (or length)
    loop_ptr: u32,     // +10 loop start
    freq: u32,         // +14 playback freq (Hz) × 256 or 1024
    volume: u32,       // +18 envelope volume 0–256
    envelope: [u8; 16],// +1C ADSR envelope (simplified)
    pan: u32,          // +2C pan 0-128 (0=left, 64=center, 128=right)
    // ... additional fields up to 64 bytes
}

/// SongTrack (used by MusicPlayer). Simplified: header + positions.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
#[allow(dead_code)]
struct TrackLayout {
    flags: u32,    // +00
    status: u32,   // +04 0=stopped
    ch_count: u8,  // +08
    _pad0: [u8; 3],
    song_ptr: u32, // +0C current song header
    pos: u32,      // +10 read offset in song data
    wait: u32,     // +14 wait-ticks remaining
    volume: u16,   // +18
    fade: u16,     // +1A fade counter
    _pad1: u32,
}

// ---- Sound driver state (held in emulator, not on GBA bus) -------

/// Per-game sound driver context tracked by HLE.
#[derive(Debug, Clone)]
pub struct SoundDriver {
    pub init_count: u32,
    pub mode_count: u32,
    pub main_count: u32,
    pub vsync_count: u32,
    pub music_open: u32,
    pub bias_desired: u16,
    pub bias_current: u16,
    /// Last known SoundInfo address (for Main mixing).
    pub info_addr: u32,
    /// Game requests DA flags (SOUNDCNT_H bits 2/3/8-15).
    pub da_flags: u16,
    /// Tracked MusicPlayer states: addr → simple state.
    #[allow(dead_code)]
    music_players: Vec<MusicPlayerState>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct MusicPlayerState {
    addr: u32,
    active: bool,
    track_count: u32,
    tracks_ptr: u32,
}

impl SoundDriver {
    pub fn new() -> Self {
        Self {
            init_count: 0,
            mode_count: 0,
            main_count: 0,
            vsync_count: 0,
            music_open: 0,
            bias_desired: 0x200,
            bias_current: 0x200,
            info_addr: 0,
            da_flags: 0,
            music_players: Vec::new(),
        }
    }
}

// ---- SWI entry points -----------------------------------------------

pub fn sound_bias(bus: &mut Bus, level: u32) {
    // ramped: each call drifts toward target; instant on first call.
    if bus.sound_driver.init_count == 0 {
        bus.sound_driver.bias_current = bus.sound_driver.bias_desired;
    }
    bus.sound_driver.bias_desired = if level != 0 { 0x200u16 } else { 0 };
    let cur = bus.sound_driver.bias_current as i32;
    let tgt = bus.sound_driver.bias_desired as i32;
    // coarse step = ~2% of the 0..0x200 range; cap at 1 step
    let next = if cur < tgt {
        (cur + (tgt - cur + 31) / 32).min(tgt)
    } else if cur > tgt {
        (cur - (cur - tgt + 31) / 32).max(tgt)
    } else {
        cur
    };
    bus.sound_driver.bias_current = next as u16;
    bus.write16_raw(0x0400_0088, bus.sound_driver.bias_current);
}

pub fn sound_driver_init(cpu: &mut Cpu, bus: &mut Bus) {
    bus.sound_driver.init_count = bus.sound_driver.init_count.wrapping_add(1);
    let area = cpu.r[0];
    if area >= 0x0300_0000 && area < 0x0400_0000 {
        bus.sound_driver.info_addr = area;
    }
}

pub fn sound_driver_mode(bus: &mut Bus, mode: u32) {
    bus.sound_driver.mode_count = bus.sound_driver.mode_count.wrapping_add(1);
    let m = mode as u16;
    // Bits 0–1: PSG volume to SOUNDCNT_H[1:0]
    // Bits 2–7: reverb bits → game-specific
    let cur = bus.read16(0x0400_0082);
    let new_h = (cur & !3) | (m & 3);
    bus.write16_raw(0x0400_0082, new_h);
    // If the game is changing DA frequency config bits, note them.
    if m & 0xFC00 != 0 {
        bus.sound_driver.da_flags = m;
    }
}

pub fn sound_driver_main(bus: &mut Bus) {
    // Stub: do not invent PCM into IWRAM. ROM mixers own the DMA source.
    bus.sound_driver.main_count = bus.sound_driver.main_count.wrapping_add(1);
    let _ = bus.sound_driver.info_addr;
}

pub fn sound_driver_vsync(bus: &mut Bus) {
    bus.sound_driver.vsync_count = bus.sound_driver.vsync_count.wrapping_add(1);
}

pub fn sound_channel_clear(bus: &mut Bus) {
    let _ = bus;
}

pub fn midi_key_freq(cpu: &mut Cpu, bus: &mut Bus) {
    let wave = cpu.r[0];
    let key = cpu.r[1];
    let fine = cpu.r[2] as i32;
    // m4a MidiKey2Freq: read wave's base frequency (Hz) from the header.
    // wave header in m4a: r0 points to { u32 type; u32 status; u32 freq, … }
    let base_hz = if wave >= 0x0300_0000 && wave < 0x0A00_0000 {
        bus.read32(wave + 8).max(1)
    } else {
        1u32
    };
    // f = base * 2^((key - 60) / 12 + fine / 256)
    let freq = if base_hz == 0 || base_hz > 1_000_000 {
        1u32
    } else {
        let note = key as f64;
        let fine_f = fine as f64 / 256.0;
        let exp = (note - 60.0) / 12.0 + fine_f;
        let mul = 2f64.powf(exp);
        (base_hz as f64 * mul) as u32
    };
    cpu.r[0] = freq.max(1);
}

pub fn music_player_open(bus: &mut Bus) {
    bus.sound_driver.music_open = bus.sound_driver.music_open.wrapping_add(1);
    // r0 = MusicPlayerInfo ptr; r1 = track array ptr; r2 = count
    // In the current CPU state we don't have direct r2 access from SWI,
    // but m4a passes count via a struct or via separate register conventions.
    // Standard m4a MusicPlayerOpen: r0 = player, r1 = tracks_ptr
    // Track count is often at (player+0x4) or passed via r2 in ARM.
    let _ = bus;
    // Record the player was opened — MusicPlayerStart/Stop use this addr.
    // We don't have r2 here (SWI dispatch doesn't expose it), but the
    // game typically sets up the count in the player struct itself.
    // Minimal: write "opened" flag.
    let addr = 0u32; // placeholder; set in dispatch from cpu context
    let _ = addr;
    // The players are tracked via a MusicPlayerInfo struct that the
    // game manages. We track via addresses in the driver state.
}

pub fn music_player_start(bus: &mut Bus) {
    // r0 = MusicPlayerInfo ptr; r1 = song header ptr
    // Mark the player as playing.
    let _ = bus;
    let _ = &bus.sound_driver;
}

pub fn music_player_stop(bus: &mut Bus) {
    let _ = bus;
    let _ = &bus.sound_driver;
}

pub fn music_player_continue(bus: &mut Bus) {
    let _ = bus;
    let _ = &bus.sound_driver;
}

pub fn music_player_fade_out(bus: &mut Bus) {
    let _ = bus;
    let _ = &bus.sound_driver;
}

pub fn sound_get_jump_list(cpu: &mut Cpu) {
    // Games call this to get the BIOS sound driver jump table address.
    // m4a games expect this at a known IWRAM address (typically 0x03007FF0
    // or the SoundArea address). Return the game's own area non-null.
    // r0 = ptr to jump-table list in IWRAM (8 or so entries, each 4 bytes).
    // We return 0 — games fall back to direct calls; documented as safe.
    cpu.r[0] = 0;
}
