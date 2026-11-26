// Aster Kernel — PL011 串口
#![allow(dead_code)]

use core::fmt::{self, Write};

#[cfg(not(feature = "rpi4"))]
pub const UART_BASE: usize = 0x0900_0000;
#[cfg(feature = "rpi4")]
pub const UART_BASE: usize = 0xFE20_1000;

const DR: usize = 0x00;
const FR: usize = 0x18;
const IBRD: usize = 0x24;
const FBRD: usize = 0x28;
const LCRH: usize = 0x2C;
const CR: usize = 0x30;
const ICR: usize = 0x44;

#[inline(always)]
unsafe fn reg(off: usize) -> *mut u32 {
    (UART_BASE + off) as *mut u32
}

#[inline(always)]
unsafe fn rd(off: usize) -> u32 {
    core::ptr::read_volatile(reg(off))
}

#[inline(always)]
unsafe fn wr(off: usize, v: u32) {
    core::ptr::write_volatile(reg(off), v);
}

pub fn init() {
    unsafe {
        wr(CR, 0);
        wr(ICR, 0x7FF);
        wr(IBRD, 13);
        wr(FBRD, 2);
        wr(LCRH, (3 << 5) | (1 << 4));
        wr(CR, (1 << 0) | (1 << 8) | (1 << 9));
    }
}

pub fn putc(c: u8) {
    unsafe {
        while rd(FR) & (1 << 5) != 0 {}
        wr(DR, c as u32);
        if c == b'\n' {
            while rd(FR) & (1 << 5) != 0 {}
            wr(DR, b'\r' as u32);
        }
    }
}

pub fn puts(s: &str) {
    for b in s.bytes() {
        putc(b);
    }
}

pub fn rx_ready() -> bool {
    unsafe { rd(FR) & (1 << 4) == 0 }
}

pub fn getc() -> Option<u8> {
    unsafe {
        if rd(FR) & (1 << 4) != 0 {
            None
        } else {
            Some((rd(DR) & 0xFF) as u8)
        }
    }
}

/// 多核里只有一个核会往串口写，用这层把 `unsafe` 收在这里
pub struct Console;

impl Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        puts(s);
        Ok(())
    }
}

pub static mut CONSOLE: Console = Console;

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {{
        use core::fmt::Write;
        let _ = write!(unsafe { &mut $crate::uart::CONSOLE }, $($arg)*);
    }};
}

#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($($arg:tt)*) => {{
        $crate::print!($($arg)*);
        $crate::print!("\n");
    }};
}
