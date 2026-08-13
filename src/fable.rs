//! Built-in playable fable — Mode 3 spark steered with D-pad / arrows.

use crate::cart::Cart;

/// "Spark" — move a bright pixel. Close the window or Esc to snuff the lantern.
pub fn spark_rom() -> Cart {
    let mut c = Asm::new();

    let header_b = c.mark();
    c.emit(0xEA00_0000);

    while c.here() * 4 < 0xC0 {
        c.emit(0xE1A0_0000);
    }

    // ── main ──────────────────────────────────────────────
    let main = c.here();
    c.mov_imm(13, 0x0300_0000);
    c.add_imm(13, 13, 0x7F00);

    // DISPCNT = 0x403 (Mode3|BG2)
    c.mov_imm(0, 0x0400_0000);
    c.mov_imm(1, 0x400);
    c.orr_imm(1, 1, 3);
    c.strh(1, 0);

    c.mov_imm(10, 0x0600_0000); // VRAM
    c.mov_imm(4, 120); // x
    c.mov_imm(5, 80); // y
    // color white 0x7FFF = 0x7F00 | 0xFF (ARM imm limits)
    c.mov_imm(8, 0x7F00);
    c.orr_imm(8, 8, 0xFF);
    c.mov_reg(6, 4); // old x
    c.mov_reg(7, 5); // old y

    let loop_top = c.here();
    let bl_wait = c.mark();
    c.emit(0xEB00_0000);
    let bl_erase = c.mark();
    c.emit(0xEB00_0000);
    let bl_input = c.mark();
    c.emit(0xEB00_0000);
    c.mov_reg(6, 4);
    c.mov_reg(7, 5);
    let bl_draw = c.mark();
    c.emit(0xEB00_0000);
    let b_loop = c.mark();
    c.emit(0xEA00_0000);

    // ── wait_vblank ───────────────────────────────────────
    let wait_vblank = c.here();
    c.push_lr();
    c.mov_imm(0, 0x0400_0000);
    c.add_imm(0, 0, 6);
    let w1 = c.here();
    c.ldrh(1, 0);
    c.cmp_imm(1, 160);
    let w1_b = c.mark();
    c.emit(0x3A00_0000); // BCC
    let w2 = c.here();
    c.ldrh(1, 0);
    c.cmp_imm(1, 160);
    let w2_b = c.mark();
    c.emit(0x2A00_0000); // BHS
    c.pop_pc();

    // ── erase ─────────────────────────────────────────────
    let erase = c.here();
    c.push_lr();
    c.mov_imm(2, 240);
    c.mul(3, 7, 2);
    c.add_reg(3, 3, 6);
    c.lsl_imm(3, 3, 1);
    c.add_reg(0, 10, 3);
    c.mov_imm(1, 0);
    c.strh(1, 0);
    c.pop_pc();

    // ── input_move ────────────────────────────────────────
    // KEYINPUT active-low: bit0 A,1 B,2 Sel,3 St,4 Right,5 Left,6 Up,7 Down
    let input_move = c.here();
    c.push_lr();
    c.mov_imm(0, 0x0400_0000);
    c.add_imm(0, 0, 0x130);
    c.ldrh(1, 0);

    // Left: if pressed (bit5 clear) and x>0 then x--
    c.tst_imm(1, 0x20);
    let sk_l = c.mark();
    c.emit(0x1A00_0000); // BNE not pressed
    c.cmp_imm(4, 0);
    let sk_l2 = c.mark();
    c.emit(0x0A00_0000); // BEQ already 0
    c.sub_imm(4, 4, 1);
    let al = c.here();

    // Right
    c.tst_imm(1, 0x10);
    let sk_r = c.mark();
    c.emit(0x1A00_0000);
    c.cmp_imm(4, 239);
    let sk_r2 = c.mark();
    c.emit(0x2A00_0000); // BHS
    c.add_imm(4, 4, 1);
    let ar = c.here();

    // Up
    c.tst_imm(1, 0x40);
    let sk_u = c.mark();
    c.emit(0x1A00_0000);
    c.cmp_imm(5, 0);
    let sk_u2 = c.mark();
    c.emit(0x0A00_0000);
    c.sub_imm(5, 5, 1);
    let au = c.here();

    // Down
    c.tst_imm(1, 0x80);
    let sk_d = c.mark();
    c.emit(0x1A00_0000);
    c.cmp_imm(5, 159);
    let sk_d2 = c.mark();
    c.emit(0x2A00_0000);
    c.add_imm(5, 5, 1);
    let ad = c.here();

    c.pop_pc();

    // ── draw ──────────────────────────────────────────────
    let draw = c.here();
    c.push_lr();
    c.mov_imm(2, 240);
    c.mul(3, 5, 2);
    c.add_reg(3, 3, 4);
    c.lsl_imm(3, 3, 1);
    c.add_reg(0, 10, 3);
    c.strh(8, 0);
    c.pop_pc();

    // patches
    c.patch_b(header_b, main);
    c.patch_bl(bl_wait, wait_vblank);
    c.patch_bl(bl_erase, erase);
    c.patch_bl(bl_input, input_move);
    c.patch_bl(bl_draw, draw);
    c.patch_b(b_loop, loop_top);
    c.patch_b_cond(w1_b, 0x3, w1); // BCC
    c.patch_b_cond(w2_b, 0x2, w2); // BHS
    c.patch_b_cond(sk_l, 0x1, al);
    c.patch_b_cond(sk_l2, 0x0, al);
    c.patch_b_cond(sk_r, 0x1, ar);
    c.patch_b_cond(sk_r2, 0x2, ar);
    c.patch_b_cond(sk_u, 0x1, au);
    c.patch_b_cond(sk_u2, 0x0, au);
    c.patch_b_cond(sk_d, 0x1, ad);
    c.patch_b_cond(sk_d2, 0x2, ad);

    let mut bytes = c.bytes();
    if bytes.len() < 0x200 {
        bytes.resize(0x200, 0);
    }
    let mut title = [0u8; 12];
    title[..5].copy_from_slice(b"SPARK");
    bytes[0xA0..0xAC].copy_from_slice(&title);
    bytes[0xAC..0xB0].copy_from_slice(b"FLSP");
    bytes[0xB0..0xB2].copy_from_slice(b"FL");

    Cart {
        data: bytes,
        title: "SPARK".into(),
        game_code: "FLSP".into(),
        maker: "FL".into(),
        path: "<built-in: spark>".into(),
        inner_name: None,
    }
}

struct Asm {
    words: Vec<u32>,
}

impl Asm {
    fn new() -> Self {
        Self { words: Vec::new() }
    }
    fn here(&self) -> usize {
        self.words.len()
    }
    fn mark(&self) -> usize {
        self.words.len()
    }
    fn emit(&mut self, w: u32) {
        self.words.push(w);
    }
    fn bytes(self) -> Vec<u8> {
        let mut b = Vec::with_capacity(self.words.len() * 4);
        for w in self.words {
            b.extend_from_slice(&w.to_le_bytes());
        }
        b
    }

    fn mov_imm(&mut self, rd: u32, imm: u32) {
        self.emit(dp_imm(0b1101, rd, 0, imm, false));
    }
    fn add_imm(&mut self, rd: u32, rn: u32, imm: u32) {
        self.emit(dp_imm(0b0100, rd, rn, imm, false));
    }
    fn sub_imm(&mut self, rd: u32, rn: u32, imm: u32) {
        self.emit(dp_imm(0b0010, rd, rn, imm, false));
    }
    fn orr_imm(&mut self, rd: u32, rn: u32, imm: u32) {
        self.emit(dp_imm(0b1100, rd, rn, imm, false));
    }
    fn cmp_imm(&mut self, rn: u32, imm: u32) {
        self.emit(dp_imm(0b1010, 0, rn, imm, true));
    }
    fn tst_imm(&mut self, rn: u32, imm: u32) {
        self.emit(dp_imm(0b1000, 0, rn, imm, true));
    }
    fn mov_reg(&mut self, rd: u32, rm: u32) {
        self.emit(0xE1A0_0000 | (rd << 12) | rm);
    }
    fn add_reg(&mut self, rd: u32, rn: u32, rm: u32) {
        self.emit(0xE080_0000 | (rn << 16) | (rd << 12) | rm);
    }
    fn lsl_imm(&mut self, rd: u32, rm: u32, sh: u32) {
        self.emit(0xE1A0_0000 | (rd << 12) | (sh << 7) | rm);
    }
    fn mul(&mut self, rd: u32, rm: u32, rs: u32) {
        self.emit(0xE000_0090 | (rd << 16) | (rs << 8) | rm);
    }
    fn strh(&mut self, rd: u32, rn: u32) {
        self.emit(0xE1C0_00B0 | (rn << 16) | (rd << 12));
    }
    fn ldrh(&mut self, rd: u32, rn: u32) {
        self.emit(0xE1D0_00B0 | (rn << 16) | (rd << 12));
    }
    fn push_lr(&mut self) {
        self.emit(0xE92D_4000);
    }
    fn pop_pc(&mut self) {
        self.emit(0xE8BD_8000);
    }

    fn patch_b(&mut self, at: usize, dest: usize) {
        let off = dest as i32 - at as i32 - 2;
        self.words[at] = 0xEA00_0000 | ((off as u32) & 0x00FF_FFFF);
    }
    fn patch_bl(&mut self, at: usize, dest: usize) {
        let off = dest as i32 - at as i32 - 2;
        self.words[at] = 0xEB00_0000 | ((off as u32) & 0x00FF_FFFF);
    }
    fn patch_b_cond(&mut self, at: usize, cond: u32, dest: usize) {
        let off = dest as i32 - at as i32 - 2;
        self.words[at] = (cond << 28) | 0x0A00_0000 | ((off as u32) & 0x00FF_FFFF);
    }
}

fn dp_imm(opcode: u32, rd: u32, rn: u32, imm: u32, set_flags: bool) -> u32 {
    let (rot, val) = find_imm(imm).unwrap_or((0, 0));
    let s = if set_flags { 1u32 } else { 0 };
    (0xE << 28) | (1 << 25) | (opcode << 21) | (s << 20) | (rn << 16) | (rd << 12) | (rot << 8) | val
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unencodable_imm_does_not_panic() {
        assert!(find_imm(0x102).is_none());
        let op = dp_imm(0b1101, 0, 0, 0x102, false);
        assert_eq!(op & 0xFF, 0, "fallback encodes #0, does not abort");
        assert!(find_imm(0xFF).is_some());
        assert!(find_imm(0x200).is_some());
        assert!(find_imm(0x0300_0000).is_some());
    }
}

fn find_imm(imm: u32) -> Option<(u32, u32)> {
    for rot in 0..16u32 {
        for val in 0..256u32 {
            let r = rot * 2;
            let v = if r == 0 { val } else { val.rotate_right(r) };
            if v == imm {
                return Some((rot, val));
            }
        }
    }
    None
}
