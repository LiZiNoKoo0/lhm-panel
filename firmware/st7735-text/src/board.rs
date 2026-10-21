//! Board facts: pins, panel parameters, timing.
//!
//! What this does NOT do: no logic, no state, no behaviour. Every number in
//! here was measured on the module on the desk -- if a value is a guess, it is
//! marked as one and it does not belong in this file.
//!
//! Why the pin numbers below look unused: esp-hal hands out peripheral handles
//! (`peripherals.GPIO18`), it cannot take an integer. So main.rs names the
//! pins directly and this table documents the wiring. Keep the two in sync; the
//! banner prints the values it can, so a mismatch is visible in the log.

#![allow(dead_code)]

// -- panel wiring -------------------------------------------------------
// Same module as the working st7735-esp32 project on this desk.
pub const PIN_SCLK: usize = 18;
pub const PIN_MOSI: usize = 23;
pub const PIN_CS: usize = 5;
pub const PIN_DC: usize = 4;
pub const PIN_RST: usize = 15;
pub const PIN_BL: usize = 2;

// -- uart0 (debug + P1 wire format) ------------------------------------
pub const PIN_TX: usize = 1;
pub const PIN_RX: usize = 3;

// -- reserved, not used yet (SPEC 3.7) ---------------------------------
// I2C SDA / SCL for a future sensor.
pub const PIN_I2C_SDA: usize = 21;
pub const PIN_I2C_SCL: usize = 22;
// KEY1..KEY4: all can pull up internally, none is a strapping pin.
pub const PIN_KEY1: usize = 32;
pub const PIN_KEY2: usize = 33;
pub const PIN_KEY3: usize = 25;
pub const PIN_KEY4: usize = 26;

// -- panel quirks measured on this exact module ------------------------
// Landscape 160x80 needs MV|MX, offsets (0,24), and no inversion.
pub const MADCTL: u8 = 0x60;
pub const OFF_X: u16 = 0;
pub const OFF_Y: u16 = 24;
pub const INVERT: bool = false;
pub const SPI_MHZ: u32 = 20;

// -- boot timings ------------------------------------------------------
pub const RST_LOW_MS: u32 = 20;
pub const RST_SETTLE_MS: u32 = 120;
