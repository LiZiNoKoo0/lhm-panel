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
//
// 0x68 = 0x60 | 0x08: bit 3 selects the RGB/BGR order and **this module needs
// BGR** (2026-10-23, measured with `bench-colours`: sending 0xF800 red,
// 0x07E0 green, 0x001F blue lit the glass blue, green, red -- a clean R/B swap,
// which bit 3 is the only knob for). The standalone st7735-esp32 project used
// 0x60 on a module that looked right; either the two modules differ or the pink
// there was always lavender. Don't "simplify" this back to 0x60 without redoing
// the three-bar test -- white text is identical in both orders, so no amount of
// staring at the self-test card can catch a regression here.
pub const MADCTL: u8 = 0x68;
pub const OFF_X: u16 = 0;
pub const OFF_Y: u16 = 24;
pub const INVERT: bool = false;
pub const SPI_MHZ: u32 = 20;

// -- boot timings ------------------------------------------------------
pub const RST_LOW_MS: u32 = 20;
pub const RST_SETTLE_MS: u32 = 120;

// -- flash layout for credentials (T3) ---------------------------------
// The IDF `nvs` partition this board ships with: 0x9000..0xF000 (24 KB, six
// 4 KB sectors). WiFi is initialised with `nvs_enable: 0`, so the driver never
// touches it, and we do not implement the NVS format -- these are raw sectors.
// 0xF000 is `phy_init` (esp-phy owns it) and everything below 0x9000 is
// bootloader + partition table. Never write outside this window.
pub const NVS_START: u32 = 0x9000;
pub const NVS_END: u32 = 0xF000;
pub const FLASH_SECTOR: u32 = 0x1000;
/// T3 keeps one copy here. T4 adds a second sector and an A/B switch.
pub const CREDS_SECTOR: u32 = NVS_START;

// -- wifi (T3) ---------------------------------------------------------
/// AP name is this prefix plus the last two MAC bytes: `st7735-c758`.
pub const AP_SSID_PREFIX: &str = "st7735-";
/// The soft-AP's own address; the page is served from here.
pub const AP_IP: [u8; 4] = [192, 168, 4, 1];
pub const AP_PREFIX_LEN: u8 = 24;
/// SPEC 4.9: no credentials, or 30 s without an address, means the hotspot
/// comes up instead of leaving the board dark and unreachable.
pub const STA_TIMEOUT_MS: u64 = 30_000;

// -- daemon link (T5) ---------------------------------------------------
/// PROTOCOL 4: the terminal is always the client, and the default port is 7802.
pub const DAEMON_PORT: u16 = 7802;

/// Where the daemon sits, when it is known before the first packet arrives.
///
/// A shipped build passes `None`: since T5.5 the address is *data*, found with
/// the UDP `discover`/`here` pair (PROTOCOL 1), so a DHCP lease change no longer
/// needs a rebuild. The two builds below still differ where it matters:
///
///   * normal build -> `None` + `DISCOVERY = true`. Discovery broadcasts to
///     `DAEMON_PORT`; if nothing answers, `FALLBACK_PEER` keeps this desk working.
///   * `bench-echo` -> a fixed address on 7804 and no discovery: that server
///     knows nothing about JSON and would answer no `here`.
#[cfg(feature = "bench-echo")]
pub const DAEMON_PEER: Option<([u8; 4], u16)> = Some(([192, 168, 1, 128], BENCH_ECHO_PORT));
#[cfg(not(feature = "bench-echo"))]
pub const DAEMON_PEER: Option<([u8; 4], u16)> = None;

/// Whether this build asks the network where the daemon is.
///
/// Off for the bench build only (see above). A `const` rather than `#[cfg]` for
/// the same reason `SEND_HELLO` is: `cfg`-ing the call path out makes the other
/// path dead code, and this project builds with zero warnings.
pub const DISCOVERY: bool = !cfg!(feature = "bench-echo");

/// Last resort when a full discovery round gets no answer.
///
/// Deliberately the same address the firmware used to hard-code: a desk with no
/// responder (daemon not running yet, or UDP broadcast filtered by the router)
/// must still link once the daemon appears, instead of sitting dark forever.
/// `DISC none` on the wire says this is what happened, so it is never silent.
pub const FALLBACK_PEER: ([u8; 4], u16) = ([192, 168, 1, 128], DAEMON_PORT);

/// The token the `bench-seed-token` build writes into flash when none is paired.
///
/// Exists for the same reason as `seed-bad-psk`: the pairing path normally needs
/// a phone, an AP, and someone typing a 16-character secret, and that cannot be
/// repeated by a script. Start the daemon with `--token <this>` to get a strict
/// daemon that accepts the board, then with any other token to watch it refuse.
/// Never part of a shipped build.
pub const BENCH_TOKEN: &str = "0123456789abcdef";

/// The echo server's port, deliberately **not** 7802.
///
/// 7802 is what a running `fake_daemon.py` holds, and `pcmon.webui` holds 7803;
/// borrowing either would mean stopping the user's session to run a test. The
/// socket path being proven does not care about the port number, so the bench
/// gets its own and collides with nothing.
#[cfg(feature = "bench-echo")]
pub const BENCH_ECHO_PORT: u16 = 7804;
