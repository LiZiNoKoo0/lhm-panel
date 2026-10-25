//! Board-independent core of the text panel.
//!
//! What this crate does NOT do: no SPI, no UART, no flash, no pins, no delays,
//! no allocator, no logging. It turns text into a 160x80 RGB565 frame and
//! hashes that frame; everything that touches hardware lives in the BSP crate.
//!
//! This crate compiles for both the ESP32 (`no_std`) and the host (tests run
//! with `std`). That is the point: `firmware/SPEC.md` C-2 makes "it builds and
//! tests on a PC" the objective test of whether the core is really portable.

#![cfg_attr(not(test), no_std)]

pub mod auth;
pub mod badge;
pub mod boot;
pub mod canvas;
pub mod crc32;
pub mod creds;
pub mod discover;
pub mod font8x16;
pub mod json;
pub mod link;
pub mod saved;
pub mod scene;
pub mod sector;
pub mod session;
pub mod sha256;
pub mod ui;
pub mod wire;
