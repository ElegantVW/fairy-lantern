//! CPSR / SPSR flags and CPU modes.

#[derive(Clone, Copy, Debug, Default)]
pub struct Cpsr {
    pub n: bool,
    pub z: bool,
    pub c: bool,
    pub v: bool,
    /// bits 0–4: USR/FIQ/IRQ/SVC/ABT/UND/SYS
    pub mode: u8,
    pub thumb: bool,
    pub irq_disable: bool,
    pub fiq_disable: bool,
}

impl Cpsr {
    pub fn new_svc() -> Self {
        Self {
            mode: 0x13, // SVC
            irq_disable: true,
            fiq_disable: true,
            ..Default::default()
        }
    }

    pub fn to_u32(self) -> u32 {
        let mut r = 0u32;
        if self.n {
            r |= 1 << 31;
        }
        if self.z {
            r |= 1 << 30;
        }
        if self.c {
            r |= 1 << 29;
        }
        if self.v {
            r |= 1 << 28;
        }
        if self.irq_disable {
            r |= 1 << 7;
        }
        if self.fiq_disable {
            r |= 1 << 6;
        }
        if self.thumb {
            r |= 1 << 5;
        }
        r | (self.mode as u32 & 0x1F)
    }

    pub fn from_u32(v: u32) -> Self {
        Self {
            n: v & (1 << 31) != 0,
            z: v & (1 << 30) != 0,
            c: v & (1 << 29) != 0,
            v: v & (1 << 28) != 0,
            irq_disable: v & (1 << 7) != 0,
            fiq_disable: v & (1 << 6) != 0,
            thumb: v & (1 << 5) != 0,
            mode: (v & 0x1F) as u8,
        }
    }

    // used by savestate

    pub fn set_nz(&mut self, result: u32) {
        self.n = (result as i32) < 0;
        self.z = result == 0;
    }

    pub fn set_nz_i64(&mut self, result: i64) {
        self.n = result < 0;
        self.z = (result as u32) == 0;
    }
}
