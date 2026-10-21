//! ST7735 driver: commands, window, pixel push.
//!
//! What this does NOT do: it does not compose frames, does not know what a
//! character is, and does not decide when to redraw. It takes 16-bit pixels in
//! big-endian byte order and puts them in a rectangle.

use esp_hal::delay::Delay;
use esp_hal::gpio::Output;
use esp_hal::spi::master::Spi;
use esp_hal::Blocking;

use crate::board::{INVERT, MADCTL, OFF_X, OFF_Y};

// -- ST7735 command set (only what this panel needs) --------------------
const CMD_SWRESET: u8 = 0x01;
const CMD_SLPOUT: u8 = 0x11;
const CMD_NORON: u8 = 0x13;
const CMD_INVOFF: u8 = 0x20;
const CMD_INVON: u8 = 0x21;
const CMD_DISPON: u8 = 0x29;
const CMD_CASET: u8 = 0x2A;
const CMD_RASET: u8 = 0x2B;
const CMD_RAMWR: u8 = 0x2C;
const CMD_MADCTL: u8 = 0x36;
const CMD_COLMOD: u8 = 0x3A;
const CMD_FRMCTR1: u8 = 0xB1;
const CMD_FRMCTR2: u8 = 0xB2;
const CMD_FRMCTR3: u8 = 0xB3;
const CMD_INVCTR: u8 = 0xB4;
const CMD_PWCTR1: u8 = 0xC0;
const CMD_PWCTR2: u8 = 0xC1;
const CMD_PWCTR3: u8 = 0xC2;
const CMD_PWCTR4: u8 = 0xC3;
const CMD_PWCTR5: u8 = 0xC4;
const CMD_VMCTR1: u8 = 0xC5;
const CMD_GMCTRP1: u8 = 0xE0;
const CMD_GMCTRN1: u8 = 0xE1;

/// Chunk size for the pixel stream. The SPI FIFO swallows this without
/// splitting a transaction into hundreds of tiny CS pulses.
const PIXEL_CHUNK: usize = 480;

pub struct St7735<'d> {
    spi: Spi<'d, Blocking>,
    cs: Output<'d>,
    dc: Output<'d>,
}

impl<'d> St7735<'d> {
    pub fn new(spi: Spi<'d, Blocking>, cs: Output<'d>, dc: Output<'d>) -> Self {
        Self { spi, cs, dc }
    }

    fn cmd(&mut self, cmd: u8, data: &[u8]) {
        self.cs.set_low();
        self.dc.set_low();
        let _ = self.spi.write(&[cmd]);
        if !data.is_empty() {
            self.dc.set_high();
            let _ = self.spi.write(data);
        }
        self.cs.set_high();
    }

    fn pixels(&mut self, data: &[u8]) {
        self.cs.set_low();
        self.dc.set_high();
        let _ = self.spi.write(data);
        self.cs.set_high();
    }

    fn set_window(&mut self, x0: u16, y0: u16, x1: u16, y1: u16) {
        let (ax, bx) = (x0 + OFF_X, x1 + OFF_X);
        let (ay, by) = (y0 + OFF_Y, y1 + OFF_Y);
        let mut b = [0u8; 4];
        b[0..2].copy_from_slice(&ax.to_be_bytes());
        b[2..4].copy_from_slice(&bx.to_be_bytes());
        self.cmd(CMD_CASET, &b);
        b[0..2].copy_from_slice(&ay.to_be_bytes());
        b[2..4].copy_from_slice(&by.to_be_bytes());
        self.cmd(CMD_RASET, &b);
        self.cmd(CMD_RAMWR, &[]);
    }

    pub fn init(&mut self, delay: &mut Delay) {
        self.cmd(CMD_SWRESET, &[]);
        delay.delay_millis(150);
        self.cmd(CMD_SLPOUT, &[]);
        delay.delay_millis(120);

        self.cmd(CMD_FRMCTR1, &[0x01, 0x2C, 0x2D]);
        self.cmd(CMD_FRMCTR2, &[0x01, 0x2C, 0x2D]);
        self.cmd(CMD_FRMCTR3, &[0x01, 0x2C, 0x2D, 0x01, 0x2C, 0x2D]);
        self.cmd(CMD_INVCTR, &[0x07]);
        self.cmd(CMD_PWCTR1, &[0xA2, 0x02, 0x84]);
        self.cmd(CMD_PWCTR2, &[0xC5]);
        self.cmd(CMD_PWCTR3, &[0x0A, 0x00]);
        self.cmd(CMD_PWCTR4, &[0x8A, 0x2A]);
        self.cmd(CMD_PWCTR5, &[0x8A, 0xEE]);
        self.cmd(CMD_VMCTR1, &[0x0E]);
        self.cmd(if INVERT { CMD_INVON } else { CMD_INVOFF }, &[]);
        self.cmd(CMD_MADCTL, &[MADCTL]);
        self.cmd(CMD_COLMOD, &[0x05]); // 16 bits per pixel
        self.cmd(
            CMD_GMCTRP1,
            &[
                0x02, 0x1C, 0x07, 0x12, 0x37, 0x32, 0x29, 0x2D, 0x29, 0x25, 0x2B, 0x39, 0x00, 0x01,
                0x03, 0x10,
            ],
        );
        self.cmd(
            CMD_GMCTRN1,
            &[
                0x03, 0x1D, 0x07, 0x06, 0x2E, 0x2C, 0x29, 0x2D, 0x2E, 0x2E, 0x37, 0x3F, 0x00, 0x00,
                0x02, 0x10,
            ],
        );
        self.cmd(CMD_NORON, &[]);
        delay.delay_millis(10);
        self.cmd(CMD_DISPON, &[]);
        delay.delay_millis(100);
    }

    /// Push a window of pixels. `data` must be exactly `w * h * 2` bytes of
    /// big-endian RGB565, row-major, starting at (x, y).
    ///
    /// The window is opened once and never changed mid-stream: re-opening it
    /// between chunks is how a panel ends up with a sheared half-frame.
    pub fn blit_rect(&mut self, x: u16, y: u16, w: u16, h: u16, data: &[u8]) {
        if w == 0 || h == 0 {
            return;
        }
        self.set_window(x, y, x + w - 1, y + h - 1);
        let want = (w as usize) * (h as usize) * 2;
        let usable = if data.len() < want { data.len() } else { want };
        for chunk in data[..usable].chunks(PIXEL_CHUNK) {
            self.pixels(chunk);
        }
    }
}
