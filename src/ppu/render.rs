//! Scanline renderers — Mode 0–5 + OBJ with priority compositing and blend.

use super::{HEIGHT, WIDTH};
use crate::bus::Bus;

/// Affine refs: (bg2x, bg2y, bg3x, bg3y) in 28.8 fixed point.
pub type AffineRefs = (i32, i32, i32, i32);

// Layer ranks for same-priority ties (lower draws in front):
// OBJ=0, BG0=1, BG1=2, BG2=3, BG3=4, BD=5
const LAYER_OBJ: u8 = 0;
const LAYER_BD: u8 = 5;

#[derive(Clone, Copy)]
struct Slot {
    color: u16,
    /// Display priority 0–3 (0 = front). Backdrop uses 4.
    prio: u8,
    layer: u8,
    /// Semi-transparent OBJ (attr0 mode 1) forces alpha blend.
    semi: bool,
}

impl Slot {
    fn backdrop(color: u16) -> Self {
        Self {
            color,
            prio: 4,
            layer: LAYER_BD,
            semi: false,
        }
    }
}

/// True if `a` should be drawn in front of `b`.
#[inline]
fn in_front(a: Slot, b: Slot) -> bool {
    if a.prio != b.prio {
        a.prio < b.prio
    } else {
        a.layer < b.layer
    }
}

pub fn render_scanline(bus: &Bus, y: usize, frame: &mut [u16]) {
    render_scanline_affine((0, 0, 0, 0), bus, y, frame);
}

pub fn render_scanline_affine(affine: AffineRefs, bus: &Bus, y: usize, frame: &mut [u16]) {
    if y >= HEIGHT {
        return;
    }
    let dispcnt = bus.dispcnt();
    let mode = dispcnt & 7;
    if dispcnt & 0x80 != 0 {
        fill_row(frame, y, 0);
        return;
    }

    let backdrop = pal_color(bus, 0);
    let win_mask = build_win_mask(bus, y, dispcnt);

    // Top + bottom layer per pixel for blend.
    let mut top = [Slot::backdrop(backdrop); WIDTH];
    let mut bot = [Slot::backdrop(backdrop); WIDTH];

    match mode {
        0 => {
            for bg in 0..4u16 {
                if dispcnt & (1 << (8 + bg)) == 0 {
                    continue;
                }
                let cnt = bg_cnt(bus, bg);
                composite_text_bg(bus, bg, y, cnt, &win_mask, &mut top, &mut bot);
            }
        }
        1 => {
            for bg in 0..2u16 {
                if dispcnt & (1 << (8 + bg)) == 0 {
                    continue;
                }
                let cnt = bg_cnt(bus, bg);
                composite_text_bg(bus, bg, y, cnt, &win_mask, &mut top, &mut bot);
            }
            if dispcnt & (1 << 10) != 0 {
                composite_affine_bg(affine, bus, 2, y, &win_mask, &mut top, &mut bot);
            }
        }
        2 => {
            if dispcnt & (1 << 10) != 0 {
                composite_affine_bg(affine, bus, 2, y, &win_mask, &mut top, &mut bot);
            }
            if dispcnt & (1 << 11) != 0 {
                composite_affine_bg(affine, bus, 3, y, &win_mask, &mut top, &mut bot);
            }
        }
        3 => {
            mode3_into(bus, y, &mut top);
        }
        4 => {
            mode4_into(bus, y, dispcnt, &mut top);
        }
        5 => {
            mode5_into(bus, y, dispcnt, &mut top);
        }
        _ => {}
    }

    if dispcnt & (1 << 12) != 0 {
        composite_sprites(bus, y, dispcnt, &win_mask, &mut top, &mut bot);
    }

    let bldcnt = bus.read16(0x0400_0050);
    let bldalpha = bus.read16(0x0400_0052);
    let bldy = bus.read16(0x0400_0054);
    let effect = (bldcnt >> 6) & 3;
    let eva = (bldalpha & 0x1F).min(16) as u32;
    let evb = ((bldalpha >> 8) & 0x1F).min(16) as u32;
    let evy = (bldy & 0x1F).min(16) as u32;

    let row = &mut frame[y * WIDTH..(y + 1) * WIDTH];
    for x in 0..WIDTH {
        row[x] = resolve_pixel(top[x], bot[x], bldcnt, effect, eva, evb, evy, win_mask[x]);
    }
}

fn put_layer(top: &mut [Slot; WIDTH], bot: &mut [Slot; WIDTH], x: usize, s: Slot) {
    if in_front(s, top[x]) {
        bot[x] = top[x];
        top[x] = s;
    } else if in_front(s, bot[x]) {
        bot[x] = s;
    }
}

fn resolve_pixel(
    top: Slot,
    bot: Slot,
    bldcnt: u16,
    effect: u16,
    eva: u32,
    evb: u32,
    evy: u32,
    win_en: u8,
) -> u16 {
    // WININ/OUT bit 5 = Color Special Effect enable for this pixel.
    // No-window mask is 0x3F (all layers + blend).
    let blend_ok = (win_en & (1 << 5)) != 0;

    let first = |layer: u8| -> bool { bldcnt & (1 << layer_to_bld_bit(layer)) != 0 };
    let second = |layer: u8| -> bool { bldcnt & (1 << (8 + layer_to_bld_bit(layer))) != 0 };

    // Semi-transparent OBJ (gfx mode 1): always alpha with 2nd target when blend allowed.
    // If blend bit is off for the window region, still force-blend (GBATEK: semi-OBJ
    // ignores the “special effect disable” in some cases — many games rely on this
    // for battle UI translucency). Prefer blending whenever 2nd target matches.
    if top.semi && second(bot.layer) {
        if blend_ok || effect == 0 {
            return alpha_blend(top.color, bot.color, eva.max(1), evb.max(1));
        }
        return alpha_blend(top.color, bot.color, eva, evb);
    }

    if !blend_ok || effect == 0 {
        return top.color;
    }

    match effect {
        1 => {
            // Alpha: top must be 1st target, bot 2nd target
            if first(top.layer) && second(bot.layer) {
                alpha_blend(top.color, bot.color, eva, evb)
            } else {
                top.color
            }
        }
        2 | 3 => {
            if first(top.layer) && evy > 0 {
                brightness_pixel(top.color, effect, evy)
            } else {
                top.color
            }
        }
        _ => top.color,
    }
}

/// Map our layer id to BLDCNT bit index (0=BG0 .. 3=BG3, 4=OBJ, 5=BD).
fn layer_to_bld_bit(layer: u8) -> u8 {
    match layer {
        LAYER_OBJ => 4,
        LAYER_BD => 5,
        l if l >= 1 && l <= 4 => l - 1, // BG0..3 stored as 1..4
        _ => 5,
    }
}

#[inline]
fn alpha_blend(a: u16, b: u16, eva: u32, evb: u32) -> u16 {
    let ar = (a & 0x1F) as u32;
    let ag = ((a >> 5) & 0x1F) as u32;
    let ab = ((a >> 10) & 0x1F) as u32;
    let br = (b & 0x1F) as u32;
    let bg = ((b >> 5) & 0x1F) as u32;
    let bb = ((b >> 10) & 0x1F) as u32;
    let r = ((ar * eva + br * evb) / 16).min(31);
    let g = ((ag * eva + bg * evb) / 16).min(31);
    let bl = ((ab * eva + bb * evb) / 16).min(31);
    (r as u16) | ((g as u16) << 5) | ((bl as u16) << 10)
}

#[inline]
fn brightness_pixel(color: u16, effect: u16, evy: u32) -> u16 {
    let r = (color & 0x1F) as u32;
    let g = ((color >> 5) & 0x1F) as u32;
    let b = ((color >> 10) & 0x1F) as u32;
    let (r, g, b) = if effect == 2 {
        (
            r + (31 - r) * evy / 16,
            g + (31 - g) * evy / 16,
            b + (31 - b) * evy / 16,
        )
    } else {
        (
            r * (16 - evy) / 16,
            g * (16 - evy) / 16,
            b * (16 - evy) / 16,
        )
    };
    ((r as u16) & 0x1F) | (((g as u16) & 0x1F) << 5) | (((b as u16) & 0x1F) << 10)
}

// ── Windows ──────────────────────────────────────────────────────────

fn build_win_mask(bus: &Bus, y: usize, dispcnt: u16) -> [u8; WIDTH] {
    let win0_on = dispcnt & (1 << 13) != 0;
    let win1_on = dispcnt & (1 << 14) != 0;
    let objwin_on = dispcnt & (1 << 15) != 0;
    if !win0_on && !win1_on && !objwin_on {
        // No windows: all layers + blend enable
        return [0x3F; WIDTH];
    }

    let winin = bus.read16(0x0400_0048);
    let winout = bus.read16(0x0400_004A);
    let out_en = (winout & 0x3F) as u8;
    let obj_en = ((winout >> 8) & 0x3F) as u8;
    let win0_en = (winin & 0x3F) as u8;
    let win1_en = ((winin >> 8) & 0x3F) as u8;

    let (l0, r0, t0, b0) = win_bounds(bus, 0);
    let (l1, r1, t1, b1) = win_bounds(bus, 1);
    let y0_in = win0_on && in_range(y as u8, t0, b0);
    let y1_in = win1_on && in_range(y as u8, t1, b1);

    // OBJ window coverage for this scanline (opaque sprite pixels with gfx mode 2)
    let mut obj_win = [false; WIDTH];
    if objwin_on {
        mark_obj_window(bus, y, dispcnt, &mut obj_win);
    }

    let mut mask = [out_en; WIDTH];
    for x in 0..WIDTH {
        let mut m = out_en;
        if obj_win[x] {
            m = obj_en;
        }
        if y1_in && in_range(x as u8, l1, r1) {
            m = win1_en;
        }
        if y0_in && in_range(x as u8, l0, r0) {
            m = win0_en;
        }
        mask[x] = m;
    }
    mask
}

/// Mark pixels covered by OBJ-window sprites (ATTR0 gfx mode = obj window).
fn mark_obj_window(bus: &Bus, y: usize, dispcnt: u16, out: &mut [bool; WIDTH]) {
    let one_d = dispcnt & (1 << 6) != 0;
    let map_2d = !one_d;
    for i in 0..128 {
        let o = i * 8;
        let attr0 = oam_u16(bus, o);
        let attr1 = oam_u16(bus, o + 2);
        let attr2 = oam_u16(bus, o + 4);
        let affine_flag = attr0 & (1 << 8) != 0;
        let dbl_or_dis = attr0 & (1 << 9) != 0;
        if !affine_flag && dbl_or_dis {
            continue;
        }
        let gfx_mode = (attr0 >> 10) & 3;
        if gfx_mode != 2 {
            continue; // only obj-window sprites
        }
        let double = affine_flag && dbl_or_dis;
        let shape = (attr0 >> 14) & 3;
        let size = (attr1 >> 14) & 3;
        let (ow, oh) = obj_dims(shape, size);
        let (dw, dh) = if double { (ow * 2, oh * 2) } else { (ow, oh) };
        let oy = attr0 & 0xFF;
        let y_signed = if oy > 160 { oy as i32 - 256 } else { oy as i32 };
        if (y as i32) < y_signed || (y as i32) >= y_signed + dh as i32 {
            continue;
        }
        let ox = attr1 & 0x1FF;
        let x_signed = if ox >= 240 { ox as i32 - 512 } else { ox as i32 };
        let color256 = attr0 & (1 << 13) != 0;
        let tile = (attr2 & 0x3FF) as usize;
        let pal_bank = ((attr2 >> 12) & 0xF) as usize;
        let row = (y as i32 - y_signed) as usize;
        if affine_flag {
            let param = ((attr1 >> 9) & 0x1F) as usize;
            let pbase = param * 32;
            let pa = oam_u16(bus, pbase + 0x06) as i16 as i32;
            let pb = oam_u16(bus, pbase + 0x0E) as i16 as i32;
            let pc = oam_u16(bus, pbase + 0x16) as i16 as i32;
            let pd = oam_u16(bus, pbase + 0x1E) as i16 as i32;
            let half_w = dw as i32 / 2;
            let half_h = dh as i32 / 2;
            let cy = row as i32 - half_h;
            for xi in 0..dw {
                let sx = x_signed + xi as i32;
                if sx < 0 || sx >= WIDTH as i32 {
                    continue;
                }
                let cx = xi as i32 - half_w;
                let tx = ((pa * cx + pb * cy) >> 8) + (ow as i32 / 2);
                let ty = ((pc * cx + pd * cy) >> 8) + (oh as i32 / 2);
                if tx < 0 || ty < 0 || tx >= ow as i32 || ty >= oh as i32 {
                    continue;
                }
                if sample_obj_pixel(
                    bus,
                    tile,
                    tx as usize,
                    ty as usize,
                    ow,
                    color256,
                    pal_bank,
                    map_2d,
                )
                .is_some()
                {
                    out[sx as usize] = true;
                }
            }
        } else {
            let hflip = attr1 & (1 << 12) != 0;
            let vflip = attr1 & (1 << 13) != 0;
            let row_f = if vflip { oh - 1 - row } else { row };
            for xi in 0..ow {
                let sx = x_signed + xi as i32;
                if sx < 0 || sx >= WIDTH as i32 {
                    continue;
                }
                let col = if hflip { ow - 1 - xi } else { xi };
                if sample_obj_pixel(bus, tile, col, row_f, ow, color256, pal_bank, map_2d).is_some()
                {
                    out[sx as usize] = true;
                }
            }
        }
    }
}

fn win_bounds(bus: &Bus, win: u16) -> (u8, u8, u8, u8) {
    let h = bus.read16(0x0400_0040 + win as u32 * 2);
    let v = bus.read16(0x0400_0044 + win as u32 * 2);
    ((h >> 8) as u8, (h & 0xFF) as u8, (v >> 8) as u8, (v & 0xFF) as u8)
}

fn in_range(v: u8, lo: u8, hi: u8) -> bool {
    if lo <= hi {
        v >= lo && v < hi
    } else {
        v >= lo || v < hi
    }
}

// ── Helpers ──────────────────────────────────────────────────────────

fn fill_row(frame: &mut [u16], y: usize, color: u16) {
    frame[y * WIDTH..(y + 1) * WIDTH].fill(color);
}

fn bg_cnt(bus: &Bus, bg: u16) -> u16 {
    bus.read16(0x0400_0008 + bg as u32 * 2)
}

fn bg_offsets(bus: &Bus, bg: u16) -> (u16, u16) {
    let base = 0x0400_0010 + bg as u32 * 4;
    // Hardware only uses lower 9 bits; keep that (text BGs).
    (bus.read16(base) & 0x1FF, bus.read16(base + 2) & 0x1FF)
}

/// MOSAIC register: BG mosaic size X/Y (1–16 pixels).
fn mosaic_size(bus: &Bus) -> (usize, usize) {
    let m = bus.read16(0x0400_004C);
    let mx = ((m & 0xF) as usize) + 1;
    let my = (((m >> 4) & 0xF) as usize) + 1;
    (mx, my)
}

/// OBJ mosaic size X/Y from MOSAIC bits 8–15 (1–16).
fn obj_mosaic_size(bus: &Bus) -> (usize, usize) {
    let m = bus.read16(0x0400_004C);
    let mx = (((m >> 8) & 0xF) as usize) + 1;
    let my = (((m >> 12) & 0xF) as usize) + 1;
    (mx, my)
}

fn pal_color(bus: &Bus, index: usize) -> u16 {
    let off = index * 2;
    let lo = bus.pal.get(off).copied().unwrap_or(0) as u16;
    let hi = bus.pal.get(off + 1).copied().unwrap_or(0) as u16;
    lo | (hi << 8)
}

fn vram_u8(bus: &Bus, off: usize) -> u8 {
    bus.vram.get(off).copied().unwrap_or(0)
}

fn vram_u16(bus: &Bus, off: usize) -> u16 {
    let lo = vram_u8(bus, off) as u16;
    let hi = vram_u8(bus, off + 1) as u16;
    lo | (hi << 8)
}

// ── Text BG ──────────────────────────────────────────────────────────

fn composite_text_bg(
    bus: &Bus,
    bg: u16,
    y: usize,
    cnt: u16,
    win_mask: &[u8; WIDTH],
    top: &mut [Slot; WIDTH],
    bot: &mut [Slot; WIDTH],
) {
    let layer_bit = 1u8 << bg;
    let layer_id = bg as u8 + 1; // BG0=1 .. BG3=4
    let prio = (cnt & 3) as u8;
    let char_base = ((cnt >> 2) & 3) as usize * 0x4000;
    let screen_base = ((cnt >> 8) & 0x1F) as usize * 0x800;
    let color256 = cnt & (1 << 7) != 0;
    let size = (cnt >> 14) & 3;
    let (map_w, map_h) = match size {
        0 => (32usize, 32usize),
        1 => (64, 32),
        2 => (32, 64),
        _ => (64, 64),
    };

    let (hofs, vofs) = bg_offsets(bus, bg);
    let mosaic = cnt & (1 << 6) != 0;
    let (mos_x, mos_y) = mosaic_size(bus);
    let map_pix_h = map_h * 8;
    let map_pix_w = map_w * 8;
    let mut sy = y + vofs as usize;
    if mosaic {
        sy = sy / mos_y * mos_y;
    }
    let fy = sy % map_pix_h;
    let ty = fy / 8;
    let y_in_tile = fy % 8;

    for x in 0..WIDTH {
        if win_mask[x] & layer_bit == 0 {
            continue;
        }
        let mut sx = x + hofs as usize;
        if mosaic {
            sx = sx / mos_x * mos_x;
        }
        let fx = sx % map_pix_w;
        let tx = fx / 8;
        let x_in_tile = fx % 8;

        let (sb_off, local_tx, local_ty) = screenblock_offset(map_w, map_h, tx, ty);
        let map_index = screen_base + sb_off + (local_ty * 32 + local_tx) * 2;
        // VRAM only 96KB; map entries past end are invalid
        if map_index + 1 >= 0x18000 {
            continue;
        }
        let entry = vram_u16(bus, map_index);
        let tile_id = (entry & 0x3FF) as usize;
        let hflip = entry & (1 << 10) != 0;
        let vflip = entry & (1 << 11) != 0;
        let pal_bank = ((entry >> 12) & 0xF) as usize;
        let px = if hflip { 7 - x_in_tile } else { x_in_tile };
        let py = if vflip { 7 - y_in_tile } else { y_in_tile };

        let color = if color256 {
            let tile_off = char_base + tile_id * 64 + py * 8 + px;
            let idx = vram_u8(bus, tile_off) as usize;
            if idx == 0 {
                continue;
            }
            pal_color(bus, idx)
        } else {
            let tile_off = char_base + tile_id * 32 + py * 4 + px / 2;
            let byte = vram_u8(bus, tile_off);
            let idx = if px & 1 == 0 {
                byte & 0xF
            } else {
                byte >> 4
            } as usize;
            if idx == 0 {
                continue;
            }
            pal_color(bus, pal_bank * 16 + idx)
        };
        put_layer(
            top,
            bot,
            x,
            Slot {
                color,
                prio,
                layer: layer_id,
                semi: false,
            },
        );
    }
}

/// Byte offset from screen_base to the screenblock containing (tx, ty),
/// plus local tile coords within that 32×32 block.
/// Layout (GBATEK): 32×32→[0]; 64×32→[0][1]; 32×64→[0]/[1]; 64×64→[0][1]/[2][3].
fn screenblock_offset(
    map_w: usize,
    map_h: usize,
    tx: usize,
    ty: usize,
) -> (usize, usize, usize) {
    if map_w == 64 && map_h == 64 {
        let sx = tx / 32;
        let sy = ty / 32;
        let lx = tx % 32;
        let ly = ty % 32;
        ((sy * 2 + sx) * 0x800, lx, ly)
    } else if map_w == 64 {
        let sx = tx / 32;
        let lx = tx % 32;
        (sx * 0x800, lx, ty)
    } else if map_h == 64 {
        let sy = ty / 32;
        let ly = ty % 32;
        (sy * 0x800, tx, ly)
    } else {
        (0, tx, ty)
    }
}

// ── Affine BG ────────────────────────────────────────────────────────

fn composite_affine_bg(
    affine: AffineRefs,
    bus: &Bus,
    bg: u16,
    _y: usize,
    win_mask: &[u8; WIDTH],
    top: &mut [Slot; WIDTH],
    bot: &mut [Slot; WIDTH],
) {
    let layer_bit = 1u8 << bg;
    let layer_id = bg as u8 + 1;
    let cnt = bg_cnt(bus, bg);
    let prio = (cnt & 3) as u8;
    let char_base = ((cnt >> 2) & 3) as usize * 0x4000;
    let screen_base = ((cnt >> 8) & 0x1F) as usize * 0x800;
    let size = (cnt >> 14) & 3;
    let dim = 16usize << size;

    let base = if bg == 2 {
        0x0400_0020u32
    } else {
        0x0400_0030
    };
    let mut pa = bus.read16(base) as i16 as i32;
    let pb = bus.read16(base + 2) as i16 as i32;
    let mut pc = bus.read16(base + 4) as i16 as i32;
    let pd = bus.read16(base + 6) as i16 as i32;
    // Sanitize garbage matrices (games only set X/Y, or IO leftovers).
    let (pa, pc) = if pa.abs() < 0x10 && pb.abs() < 0x10 {
        (0x100, 0)
    } else if pc.abs() > 0x400 {
        (pa, 0)
    } else {
        (pa, pc)
    };
    let _ = (pb, pd);
    let (mut rx, mut ry) = if bg == 2 {
        (affine.0, affine.1)
    } else {
        (affine.2, affine.3)
    };
    let map_pix = dim * 8;
    let wrap = cnt & (1 << 13) != 0;

    for x in 0..WIDTH {
        if win_mask[x] & layer_bit == 0 {
            rx = rx.wrapping_add(pa);
            ry = ry.wrapping_add(pc);
            continue;
        }
        let mut sx = rx >> 8;
        let mut sy = ry >> 8;
        if wrap {
            sx = sx.rem_euclid(map_pix as i32);
            sy = sy.rem_euclid(map_pix as i32);
        } else if sx < 0 || sy < 0 || sx >= map_pix as i32 || sy >= map_pix as i32 {
            rx = rx.wrapping_add(pa);
            ry = ry.wrapping_add(pc);
            continue;
        }
        let tx = sx as usize / 8;
        let ty = sy as usize / 8;
        let px = sx as usize % 8;
        let py = sy as usize % 8;
        let map_off = screen_base + ty * dim + tx;
        let tile_id = vram_u8(bus, map_off) as usize;
        let tile_off = char_base + tile_id * 64 + py * 8 + px;
        let idx = vram_u8(bus, tile_off) as usize;
        if idx != 0 {
            put_layer(
                top,
                bot,
                x,
                Slot {
                    color: pal_color(bus, idx),
                    prio,
                    layer: layer_id,
                    semi: false,
                },
            );
        }
        rx = rx.wrapping_add(pa);
        ry = ry.wrapping_add(pc);
    }
}

// ── Bitmap modes ─────────────────────────────────────────────────────

fn mode3_into(bus: &Bus, y: usize, top: &mut [Slot; WIDTH]) {
    let base = y * WIDTH * 2;
    for x in 0..WIDTH {
        top[x] = Slot {
            color: vram_u16(bus, base + x * 2),
            prio: 0,
            layer: 3, // BG2-ish
            semi: false,
        };
    }
}

fn mode4_into(bus: &Bus, y: usize, dispcnt: u16, top: &mut [Slot; WIDTH]) {
    let page = if dispcnt & 0x10 != 0 { 0xA000 } else { 0 };
    let base = page + y * WIDTH;
    for x in 0..WIDTH {
        let idx = vram_u8(bus, base + x) as usize;
        top[x] = Slot {
            color: pal_color(bus, idx),
            prio: 0,
            layer: 3,
            semi: false,
        };
    }
}

fn mode5_into(bus: &Bus, y: usize, dispcnt: u16, top: &mut [Slot; WIDTH]) {
    if y >= 128 {
        return;
    }
    let page = if dispcnt & 0x10 != 0 { 0xA000 } else { 0 };
    let base = page + y * 160 * 2;
    for x in 0..WIDTH.min(160) {
        top[x] = Slot {
            color: vram_u16(bus, base + x * 2),
            prio: 0,
            layer: 3,
            semi: false,
        };
    }
}

// ── Sprites ──────────────────────────────────────────────────────────

fn composite_sprites(
    bus: &Bus,
    y: usize,
    dispcnt: u16,
    win_mask: &[u8; WIDTH],
    top: &mut [Slot; WIDTH],
    bot: &mut [Slot; WIDTH],
) {
    let one_d = dispcnt & (1 << 6) != 0;
    let map_2d = !one_d;
    const OBJ_BIT: u8 = 1 << 4;
    let (obj_mos_x, obj_mos_y) = obj_mosaic_size(bus);

    // Draw back-to-front by priority so put_layer gets correct top/bot.
    // Within same prio, lower OAM index is in front (GBATEK).
    for prio in (0..4u8).rev() {
        for i in (0..128).rev() {
            let o = i * 8;
            let attr0 = oam_u16(bus, o);
            let attr1 = oam_u16(bus, o + 2);
            let attr2 = oam_u16(bus, o + 4);

            // ATTR0: bit8=affine, bit9=disable(normal)/double(affine),
            // bits10-11=gfx mode (0 normal, 1 semi-trans, 2 obj-window).
            let affine_flag = attr0 & (1 << 8) != 0;
            let dbl_or_dis = attr0 & (1 << 9) != 0;
            if !affine_flag && dbl_or_dis {
                continue; // disabled
            }
            let gfx_mode = (attr0 >> 10) & 3;
            if gfx_mode == 2 || gfx_mode == 3 {
                continue; // obj window / prohibited
            }
            let semi = gfx_mode == 1;
            let double = affine_flag && dbl_or_dis;
            let mosaic = attr0 & (1 << 12) != 0;

            let shape = (attr0 >> 14) & 3;
            let size = (attr1 >> 14) & 3;
            let (ow, oh) = obj_dims(shape, size);
            let (dw, dh) = if double {
                (ow * 2, oh * 2)
            } else {
                (ow, oh)
            };

            let oy = attr0 & 0xFF;
            // Y is signed 8-bit: 0..159 on-screen, 160..255 → -96..-1
            let y_signed = if oy >= 160 {
                (oy as i32) - 256
            } else {
                oy as i32
            };
            let y0 = y_signed;
            let y1 = y0 + dh as i32;
            if (y as i32) < y0 || (y as i32) >= y1 {
                continue;
            }

            let pr = ((attr2 >> 10) & 3) as u8;
            if pr != prio {
                continue;
            }

            let ox = attr1 & 0x1FF;
            // X is 9-bit signed: 0..240ish on-screen, 256..511 → negative
            let x_signed = if ox >= 256 {
                (ox as i32) - 512
            } else {
                ox as i32
            };
            let color256 = attr0 & (1 << 13) != 0;
            let tile = (attr2 & 0x3FF) as usize;
            let pal_bank = ((attr2 >> 12) & 0xF) as usize;
            let mut row = (y as i32 - y0) as usize;
            // OBJ mosaic: quantize row within the sprite (Gen3 HP-bar drain uses this)
            if mosaic {
                row = row / obj_mos_y * obj_mos_y;
                if row >= dh {
                    continue;
                }
            }

            if affine_flag {
                let param = ((attr1 >> 9) & 0x1F) as usize;
                let pbase = param * 32;
                let mut pa = oam_u16(bus, pbase + 0x06) as i16 as i32;
                let pb = oam_u16(bus, pbase + 0x0E) as i16 as i32;
                let mut pc = oam_u16(bus, pbase + 0x16) as i16 as i32;
                let mut pd = oam_u16(bus, pbase + 0x1E) as i16 as i32;
                // Identity fallback if matrix still zero (uninit OAM)
                if pa == 0 && pb == 0 && pc == 0 && pd == 0 {
                    pa = 0x100;
                    pd = 0x100;
                }
                let half_w = dw as i32 / 2;
                let half_h = dh as i32 / 2;
                let cy = row as i32 - half_h;
                for xi in 0..dw {
                    let mut x_i = xi;
                    if mosaic {
                        x_i = x_i / obj_mos_x * obj_mos_x;
                    }
                    let sx = x_signed + x_i as i32;
                    if sx < 0 || sx >= WIDTH as i32 {
                        continue;
                    }
                    if win_mask[sx as usize] & OBJ_BIT == 0 {
                        continue;
                    }
                    let cx = x_i as i32 - half_w;
                    let tx = ((pa * cx + pb * cy) >> 8) + (ow as i32 / 2);
                    let ty = ((pc * cx + pd * cy) >> 8) + (oh as i32 / 2);
                    if tx < 0 || ty < 0 || tx >= ow as i32 || ty >= oh as i32 {
                        continue;
                    }
                    if let Some(c) = sample_obj_pixel(
                        bus,
                        tile,
                        tx as usize,
                        ty as usize,
                        ow,
                        color256,
                        pal_bank,
                        map_2d,
                    ) {
                        put_layer(
                            top,
                            bot,
                            sx as usize,
                            Slot {
                                color: c,
                                prio: pr,
                                layer: LAYER_OBJ,
                                semi,
                            },
                        );
                    }
                }
            } else {
                let hflip = attr1 & (1 << 12) != 0;
                let vflip = attr1 & (1 << 13) != 0;
                let row_f = if vflip {
                    if row >= oh {
                        continue;
                    }
                    oh - 1 - row
                } else {
                    row.min(oh.saturating_sub(1))
                };
                for xi in 0..ow {
                    let mut x_i = xi;
                    if mosaic {
                        x_i = x_i / obj_mos_x * obj_mos_x;
                    }
                    let sx = x_signed + x_i as i32;
                    if sx < 0 || sx >= WIDTH as i32 {
                        continue;
                    }
                    if win_mask[sx as usize] & OBJ_BIT == 0 {
                        continue;
                    }
                    let col = if hflip { ow - 1 - x_i } else { x_i };
                    if let Some(c) =
                        sample_obj_pixel(bus, tile, col, row_f, ow, color256, pal_bank, map_2d)
                    {
                        put_layer(
                            top,
                            bot,
                            sx as usize,
                            Slot {
                                color: c,
                                prio: pr,
                                layer: LAYER_OBJ,
                                semi,
                            },
                        );
                    }
                }
            }
        }
    }
}

fn oam_u16(bus: &Bus, off: usize) -> u16 {
    let lo = bus.oam.get(off).copied().unwrap_or(0) as u16;
    let hi = bus.oam.get(off + 1).copied().unwrap_or(0) as u16;
    lo | (hi << 8)
}

fn obj_dims(shape: u16, size: u16) -> (usize, usize) {
    match (shape, size) {
        (0, 0) => (8, 8),
        (0, 1) => (16, 16),
        (0, 2) => (32, 32),
        (0, 3) => (64, 64),
        (1, 0) => (16, 8),
        (1, 1) => (32, 8),
        (1, 2) => (32, 16),
        (1, 3) => (64, 32),
        (2, 0) => (8, 16),
        (2, 1) => (8, 32),
        (2, 2) => (16, 32),
        (2, 3) => (32, 64),
        _ => (8, 8),
    }
}

fn sample_obj_pixel(
    bus: &Bus,
    base_tile: usize,
    x: usize,
    y: usize,
    obj_w: usize,
    color256: bool,
    pal_bank: usize,
    map_2d: bool,
) -> Option<u16> {
    let tx = x / 8;
    let ty = y / 8;
    let px = x % 8;
    let py = y % 8;
    // OBJ tiles live in the upper 32 KiB of VRAM (0x06010000).
    // In modes 3–5 lower OBJ VRAM is partially used by the bitmap — mode 0–2 OK.
    let obj_vram = 0x10000usize;
    const VRAM_END: usize = 0x18000;

    if color256 {
        let tile_index = if map_2d {
            base_tile + ty * 32 + tx * 2
        } else {
            base_tile + (ty * (obj_w / 8) + tx) * 2
        };
        let off = obj_vram + tile_index * 32 + py * 8 + px;
        if off >= VRAM_END {
            return None;
        }
        let idx = vram_u8(bus, off) as usize;
        if idx == 0 {
            return None;
        }
        let lo = bus.pal.get(0x200 + idx * 2).copied().unwrap_or(0) as u16;
        let hi = bus.pal.get(0x200 + idx * 2 + 1).copied().unwrap_or(0) as u16;
        Some(lo | (hi << 8))
    } else {
        let tile_index = if map_2d {
            base_tile + ty * 32 + tx
        } else {
            base_tile + ty * (obj_w / 8) + tx
        };
        let off = obj_vram + tile_index * 32 + py * 4 + px / 2;
        if off >= VRAM_END {
            return None;
        }
        let byte = vram_u8(bus, off);
        let idx = if px & 1 == 0 { byte & 0xF } else { byte >> 4 } as usize;
        if idx == 0 {
            return None;
        }
        let pal_off = 0x200 + (pal_bank * 16 + idx) * 2;
        let lo = bus.pal.get(pal_off).copied().unwrap_or(0) as u16;
        let hi = bus.pal.get(pal_off + 1).copied().unwrap_or(0) as u16;
        Some(lo | (hi << 8))
    }
}

/// Convert BGR555 frame to RGB888 bytes.
pub fn frame_to_rgb(frame: &[u16]) -> Vec<u8> {
    let mut out = vec![0u8; frame.len() * 3];
    for (i, &px) in frame.iter().enumerate() {
        let r = (px & 0x1F) as u8;
        let g = ((px >> 5) & 0x1F) as u8;
        let b = ((px >> 10) & 0x1F) as u8;
        out[i * 3] = (r << 3) | (r >> 2);
        out[i * 3 + 1] = (g << 3) | (g >> 2);
        out[i * 3 + 2] = (b << 3) | (b >> 2);
    }
    out
}
