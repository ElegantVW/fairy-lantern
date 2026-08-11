//! Four GBA timers — methods on Bus via free functions.

use crate::bus::Bus;
use crate::irq;

#[derive(Clone, Debug)]
pub struct Timers {
    pub counter: [u32; 4],
    pub reload: [u16; 4],
    /// Leftover cycles after prescale division (per channel).
    pub frac: [u32; 4],
}

impl Default for Timers {
    fn default() -> Self {
        Self::new()
    }
}

impl Timers {
    pub fn new() -> Self {
        Self {
            counter: [0; 4],
            reload: [0; 4],
            frac: [0; 4],
        }
    }

    pub fn on_write_reload(&mut self, idx: usize, val: u16) {
        if idx < 4 {
            self.reload[idx] = val;
            self.counter[idx] = val as u32;
            self.frac[idx] = 0;
        }
    }
}

/// Advance timers by approx CPU cycles.
pub fn step(t: &mut Timers, bus: &mut Bus, cycles: u32) {
    // Use the cached ctrl/reload shadows (synced on MMIO writes) — reading the
    // IO registers from here costs 4 bus round-trips *per instruction*.
    let ctrl = bus.timer_ctrl_prev;
    let reload = bus.timer_reload;
    for i in 0..4 {
        let c = ctrl[i];
        if c & 0x80 == 0 {
            continue;
        }
        // Count-up cascade: driven by previous overflow, not free-run clock
        if i > 0 && c & 0x4 != 0 {
            continue;
        }
        let presc = match c & 3 {
            0 => 1u32,
            1 => 64,
            2 => 256,
            _ => 1024,
        };
        let total = t.frac[i].wrapping_add(cycles);
        if total < presc {
            t.frac[i] = total;
            continue;
        }
        let add = total / presc;
        t.frac[i] = total - add * presc;
        tick_channel(t, bus, i, add, c, reload[i]);
    }
}

fn tick_channel(t: &mut Timers, bus: &mut Bus, i: usize, ticks: u32, ctrl: u16, reload_val: u16) {
    if crate::cpu::fairy_trace() && i == 3 && bus.dbg_evt % 28 == 0 {
        eprintln!(
            "TM3 tick evt{} ctrl={:04X} cnt={:04X} add={}",
            bus.dbg_evt, ctrl, t.counter[i] & 0xFFFF, ticks
        );
    }
    let reload = (reload_val as u32) & 0xFFFF;
    let mut counter = t.counter[i] & 0xFFFF;
    let mut left = ticks;
    // Ticks from reload to overflow (reload=0 → full 65536)
    let period = (0x1_0000u32 - reload).max(1);
    while left > 0 {
        let room = 0x1_0000u32 - counter; // ticks until overflow
        if left < room {
            counter += left;
            left = 0;
        } else {
            left -= room;
            counter = reload;
            if ctrl & 0x40 != 0 {
                raise_timer(bus, i);
            }
            cascade_next(t, bus, i);
            // Cap pathological multi-overflow storms from huge cycle slices
            if left > period.saturating_mul(64) {
                let extra = left / period;
                left %= period;
                if ctrl & 0x40 != 0 {
                    for _ in 0..extra.min(16) {
                        raise_timer(bus, i);
                    }
                }
                for _ in 0..extra.min(16) {
                    cascade_next(t, bus, i);
                }
                counter = reload.wrapping_add(left).min(0xFFFF);
                left = 0;
            }
        }
    }
    t.counter[i] = counter;
    bus.write16_raw(0x0400_0100 + i as u32 * 4, counter as u16);
}

fn cascade_next(t: &mut Timers, bus: &mut Bus, i: usize) {
    if i + 1 >= 4 {
        return;
    }
    let n = i + 1;
    let nctrl = bus.timer_ctrl_prev[n];
    if nctrl & 0x80 == 0 || nctrl & 0x4 == 0 {
        return;
    }
    let nc = (t.counter[n] & 0xFFFF) + 1;
    if nc > 0xFFFF {
        t.counter[n] = t.reload[n] as u32;
        bus.write16_raw(0x0400_0100 + n as u32 * 4, t.reload[n]);
        if nctrl & 0x40 != 0 {
            raise_timer(bus, n);
        }
        cascade_next(t, bus, n);
    } else {
        t.counter[n] = nc;
        bus.write16_raw(0x0400_0100 + n as u32 * 4, nc as u16);
    }
}

fn raise_timer(bus: &mut Bus, i: usize) {
    let bit = match i {
        0 => irq::IRQ_TIMER0,
        1 => irq::IRQ_TIMER1,
        2 => irq::IRQ_TIMER2,
        _ => irq::IRQ_TIMER3,
    };
    irq::raise(bus, bit);
}
