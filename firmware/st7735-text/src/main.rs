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
//! This file is only assembly: peripherals in, one loop, three calls out.
//! What it does NOT do: compose pixels (panel-core), drive SPI (panel.rs),
//! parse bytes into lines (link.rs), or format log lines (diag.rs). If logic
//! starts appearing here, it belongs one layer down.
//!
//! Layers (see firmware/ARCHITECTURE.md §3, firmware/SPEC.md §2):
//!   panel-core  board-independent: canvas, glyphs, scenes, CRC, link contract
//!   panel.rs    the ST7735 alone
//!   link.rs     bytes -> lines
//!   diag.rs     banner + per-frame CRC line
//!   sys.rs      heap + millisecond clock
//!   board.rs    pins and measured panel parameters

#![no_std]
#![no_main]

extern crate alloc;

use esp_hal::delay::Delay;
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::main;
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::spi::Mode as SpiMode;
use esp_hal::time::Rate;
use esp_hal::uart::{Config as UartConfig, Uart};

use panel_core::canvas::{Canvas, BG};
use panel_core::link::Transport;
use panel_core::scene::{Rows, Scene, FRAME_BYTES, LINE_MAX, LCD_H, LCD_W};

mod board;
mod diag;
mod link;
mod panel;
mod sys;

esp_bootloader_esp_idf::esp_app_desc!();

/// The whole frame lives in RAM: 25600 bytes, the exact size the PC renders,
/// so the CRC covers pixel-for-pixel the same image.
static mut FRAME: [u8; FRAME_BYTES] = [0; FRAME_BYTES];

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    // UART0 belongs to the draw loop, so there is nothing safe to print from
    // here yet. SPEC 7.3 wants one line before resetting; that needs the UART
    // to be handed out before main, which is a later step.
    loop {}
}

#[main]
fn main() -> ! {
    let peripherals = esp_hal::init(esp_hal::Config::default());
    // The allocator comes up before anything else allocates, and before the
    // banner, because the banner is where its state is reported.
    sys::init_heap();
    let mut delay = Delay::new();

    // UART first: the banner before the panel is part of the boot contract.
    let uart = Uart::new(peripherals.UART0, UartConfig::default())
        .unwrap()
        .with_tx(peripherals.GPIO1)
        .with_rx(peripherals.GPIO3);
    let mut transport = link::UartLine::new(uart);
    diag::banner(&mut transport);

    // One line, two capabilities. The probe is released right after so the
    // heap the later cards budget against is the full 64 KB again.
    let probe = sys::probe();
    diag::heap_clock(&mut transport, probe.size, probe.free, sys::now_ms());
    if !probe.verified {
        diag::heap_unverified(&mut transport);
    }
    drop(probe);

    let mut backlight = Output::new(peripherals.GPIO2, Level::High, OutputConfig::default());
    backlight.set_high();

    let mut rst = Output::new(peripherals.GPIO15, Level::High, OutputConfig::default());
    rst.set_low();
    delay.delay_millis(board::RST_LOW_MS);
    rst.set_high();
    delay.delay_millis(board::RST_SETTLE_MS);

    let spi = Spi::new(
        peripherals.SPI2,
        SpiConfig::default()
            .with_frequency(Rate::from_mhz(board::SPI_MHZ))
            .with_mode(SpiMode::_0),
    )
    .unwrap()
    .with_sck(peripherals.GPIO18)
    .with_mosi(peripherals.GPIO23);

    let cs = Output::new(peripherals.GPIO5, Level::High, OutputConfig::default());
    let dc = Output::new(peripherals.GPIO4, Level::Low, OutputConfig::default());

    let mut lcd = panel::St7735::new(spi, cs, dc);
    lcd.init(&mut delay);

    let frame = unsafe { &mut *core::ptr::addr_of_mut!(FRAME) };
    {
        // Black until the PC says otherwise: no stale picture after a reset.
        let mut canvas = Canvas::new(&mut frame[..], LCD_W, LCD_H);
        canvas.clear(BG);
    }
    lcd.blit_rect(0, 0, LCD_W as u16, LCD_H as u16, &frame[..]);

    diag::init_done(
        &mut transport,
        board::MADCTL,
        board::OFF_X,
        board::OFF_Y,
        board::INVERT,
        board::SPI_MHZ,
    );
    diag::ready(&mut transport);

    let mut scene = Rows::new();
    let mut line = [0u8; LINE_MAX];

    loop {
        match transport.poll_line(&mut line) {
            Ok(Some(len)) => scene.apply(&line[..len]), // state only, no pixels
            Ok(None) => {}
            Err(_) => {}
        }

        if scene.is_dirty() {
            // One render = one whole frame = one blit. Never paint while
            // streaming pixels over SPI.
            let digest = {
                let mut canvas = Canvas::new(&mut frame[..], LCD_W, LCD_H);
                scene.render(&mut canvas);
                canvas.digest()
            };
            diag::frame_line(&mut transport, LCD_W, LCD_H, digest, FRAME_BYTES);
            lcd.blit_rect(0, 0, LCD_W as u16, LCD_H as u16, &frame[..]);
            scene.mark_clean();
        }
    }
}
