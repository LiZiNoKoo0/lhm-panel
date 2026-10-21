//! ST7735 160x80 live text panel: the PC decides what is on the glass.
//!
//! Wire format on UART0 (115200 8N1), one frame per line, '\n' terminated:
//!
//!     <top line>|<bottom line>
//!
//! Both parts are plain ASCII, truncated to 20 characters (160px / 8px) and
//! centred horizontally. An empty part leaves that row black. After every
//! redraw the board answers
//!
//!     FRAME 160x80 crc=0x........ bytes=25600
//!
//! which is the CRC32 of the exact 25600 bytes it pushed, so the PC can prove
//! the glass matches what it meant to send (`firmware/check_parity.py`).
//!
//! What this firmware deliberately does NOT do:
//!   * format numbers, invent labels, or know what "CPU" means -- it draws the
//!     bytes it was given (the daemon formats; a terminal stays dumb)
//!   * speak the display protocol, open sockets, or join WiFi -- M6 replaces
//!     this serial link with TCP 7802 + PROTOCOL.md; the drawing code stays
//!   * keep state beyond the last frame, or animate anything by itself
//!
//! Wiring (same module as the working st7735-esp32 project on this desk):
//!   SCLK=GPIO18  MOSI=GPIO23  CS=GPIO5  DC=GPIO4  RST=GPIO15  BL=GPIO2
//!   UART0 TX=GPIO1  RX=GPIO3
//!
//! Panel quirks measured on this exact module: landscape 160x80 needs
//! MADCTL=0x60 (MV|MX), offsets (0,24), and NO inversion.

#![no_std]
#![no_main]

use esp_hal::delay::Delay;
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::main;
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::spi::Mode as SpiMode;
use esp_hal::time::Rate;
use esp_hal::uart::{Config as UartConfig, Uart};
use esp_hal::Blocking;

mod font8x16;

esp_bootloader_esp_idf::esp_app_desc!();

// ============================ layout ============================

const LCD_W: usize = 160;
const LCD_H: usize = 80;
const FRAME_BYTES: usize = LCD_W * LCD_H * 2;

/// Where the two text rows sit. 2 x 16px inside 80px, with an 8px gutter.
const TOP_Y: usize = 20;
const BOTTOM_Y: usize = 44;

/// Characters per row: 160px / 8px.
const COLS: usize = LCD_W / font8x16::CELL_W;

/// Longest line we accept from the PC, '|' included.
const LINE_MAX: usize = 48;

const FG: u16 = 0xFFFF;
const BG: u16 = 0x0000;

// ============================ panel wiring ============================

const MADCTL: u8 = 0x60; // MV (rotate 90) | MX (mirror) -> landscape, origin top-left
const OFF_X: u16 = 0;
const OFF_Y: u16 = 24;
const INVERT: bool = false;
const SPI_MHZ: u32 = 20;

// ============================ ST7735 commands ============================

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

/// The whole frame lives in RAM: 25600 bytes, the exact size the PC renders,
/// so the CRC covers pixel-for-pixel the same image.
static mut FRAME: [u8; FRAME_BYTES] = [0; FRAME_BYTES];

struct St7735<'d> {
    spi: Spi<'d, Blocking>,
    cs: Output<'d>,
    dc: Output<'d>,
}

impl<'d> St7735<'d> {
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

    fn init(&mut self, delay: &mut Delay) {
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

    /// Push the whole RAM frame in one window, in chunks the SPI can swallow.
    fn blit(&mut self, frame: &[u8]) {
        self.set_window(0, 0, LCD_W as u16 - 1, LCD_H as u16 - 1);
        for chunk in frame.chunks(480) {
            self.pixels(chunk);
        }
    }
}

// ============================ text into the frame ============================

fn put_pixel(frame: &mut [u8], x: usize, y: usize, colour: u16) {
    if x >= LCD_W || y >= LCD_H {
        return;
    }
    let at = (y * LCD_W + x) * 2;
    frame[at] = (colour >> 8) as u8;
    frame[at + 1] = (colour & 0xFF) as u8;
}

/// Paint the whole frame with the background colour, byte order matching SPI.
fn clear(frame: &mut [u8]) {
    let hi = (BG >> 8) as u8;
    let lo = (BG & 0xFF) as u8;
    let mut at = 0;
    while at + 1 < frame.len() {
        frame[at] = hi;
        frame[at + 1] = lo;
        at += 2;
    }
}

/// Draw one row of up to COLS 8x16 cells, centred; MSB of each row byte is the
/// leftmost pixel. Mirrors `render_page()` in preview_st7735.py.
fn draw_row(frame: &mut [u8], text: &[u8], y0: usize) {
    let cells = if text.len() < COLS { text.len() } else { COLS };
    let width = cells * font8x16::CELL_W;
    let x0 = if width < LCD_W { (LCD_W - width) / 2 } else { 0 };
    for (cell, &ch) in text[..cells].iter().enumerate() {
        let idx = if (font8x16::FIRST..=font8x16::LAST).contains(&ch) {
            (ch - font8x16::FIRST) as usize
        } else {
            (b' ' - font8x16::FIRST) as usize
        };
        for row in 0..font8x16::CELL_H {
            let bits = font8x16::GLYPHS[idx][row];
            for col in 0..font8x16::CELL_W {
                if (bits >> (7 - col)) & 1 == 1 {
                    put_pixel(frame, x0 + cell * font8x16::CELL_W + col, y0 + row, FG);
                }
            }
        }
    }
}

/// CRC32/IEEE, bitwise, same polynomial and init as zlib.crc32 on the PC side.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

// ============================ tiny output helpers ============================

fn send(uart: &mut Uart<'_, Blocking>, text: &[u8]) {
    let _ = uart.write(text);
    let _ = uart.flush();
}

fn send_frame_line(uart: &mut Uart<'_, Blocking>, crc: u32) {
    let mut line = [0u8; 44];
    let head = b"FRAME 160x80 crc=0x";
    line[..head.len()].copy_from_slice(head);
    let mut at = head.len();
    for shift in [28u32, 24, 20, 16, 12, 8, 4, 0] {
        let nibble = ((crc >> shift) & 0xF) as u8;
        line[at] = if nibble < 10 { b'0' + nibble } else { b'a' + nibble - 10 };
        at += 1;
    }
    let tail = b" bytes=25600\n";
    line[at..at + tail.len()].copy_from_slice(tail);
    send(uart, &line[..at + tail.len()]);
}

// ============================ main ============================

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    // UART0 belongs to the draw loop, so there is nothing to print from here.
    loop {}
}

#[main]
fn main() -> ! {
    let peripherals = esp_hal::init(esp_hal::Config::default());
    let mut delay = Delay::new();

    let mut uart = Uart::new(peripherals.UART0, UartConfig::default())
        .unwrap()
        .with_tx(peripherals.GPIO1)
        .with_rx(peripherals.GPIO3);
    send(&mut uart, b"\n=== st7735 160x80 live text / 8x16 dot matrix ===\n");

    let mut bl = Output::new(peripherals.GPIO2, Level::High, OutputConfig::default());
    bl.set_high();

    let mut rst = Output::new(peripherals.GPIO15, Level::High, OutputConfig::default());
    rst.set_low();
    delay.delay_millis(20);
    rst.set_high();
    delay.delay_millis(120);

    let spi = Spi::new(
        peripherals.SPI2,
        SpiConfig::default()
            .with_frequency(Rate::from_mhz(SPI_MHZ))
            .with_mode(SpiMode::_0),
    )
    .unwrap()
    .with_sck(peripherals.GPIO18)
    .with_mosi(peripherals.GPIO23);

    let cs = Output::new(peripherals.GPIO5, Level::High, OutputConfig::default());
    let dc = Output::new(peripherals.GPIO4, Level::Low, OutputConfig::default());

    let mut lcd = St7735 { spi, cs, dc };
    lcd.init(&mut delay);

    let frame = unsafe { &mut *core::ptr::addr_of_mut!(FRAME) };
    clear(frame); // black until the PC says otherwise
    lcd.blit(frame);

    send(&mut uart, b"init done: MADCTL=0x60 OFF=(0,24) INVERT=false SPI=20MHz\n");
    send(&mut uart, b"READY\n");

    let mut line = [0u8; LINE_MAX];
    let mut fill = 0usize;
    let mut chunk = [0u8; 64];

    loop {
        let n = match uart.read(&mut chunk) {
            Ok(n) => n,
            Err(_) => 0,
        };
        if n == 0 {
            continue;
        }
        for &byte in &chunk[..n] {
            match byte {
                b'\n' => {
                    if fill > 0 {
                        draw_and_report(frame, &mut lcd, &mut uart, &line[..fill]);
                    }
                    fill = 0;
                }
                b'\r' => {}
                _ => {
                    if fill < LINE_MAX {
                        line[fill] = byte;
                        fill += 1;
                    } else {
                        fill = 0; // over-long line: drop it, don't wedge
                    }
                }
            }
        }
    }
}

/// Split "top|bottom", paint both rows, report the CRC, then push the frame.
fn draw_and_report(
    frame: &mut [u8],
    lcd: &mut St7735<'_>,
    uart: &mut Uart<'_, Blocking>,
    message: &[u8],
) {
    let split = message.iter().position(|&b| b == b'|').unwrap_or(message.len());
    let (top, rest) = message.split_at(split);
    let bottom = if rest.is_empty() { &rest[..0] } else { &rest[1..] };

    clear(frame);
    draw_row(frame, top, TOP_Y);
    draw_row(frame, bottom, BOTTOM_Y);

    let sum = crc32(frame);
    send_frame_line(uart, sum);
    lcd.blit(frame);
}

