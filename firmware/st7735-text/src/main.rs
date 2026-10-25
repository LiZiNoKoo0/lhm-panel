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
//! Since T3 this file is also where the runtime starts: ESP-IDF's scheduler
//! (`esp-rtos`) comes up first, WiFi and the panel each get a task, and this
//! function ends by awaiting the network forever.
//!
//! What it does NOT do: compose pixels (panel-core), drive SPI (panel.rs),
//! parse bytes into lines (link.rs), format log lines (diag.rs), touch flash
//! (store.rs), or run WiFi (net.rs). If logic starts appearing here, it
//! belongs one layer down.
//!
//! Layers (see firmware/ARCHITECTURE.md §3, firmware/SPEC.md §2):
//!   panel-core  board-independent: canvas, glyphs, scenes, CRC, creds, link
//!   panel.rs    the ST7735 alone
//!   link.rs     bytes -> lines
//!   diag.rs     banner + per-frame CRC line + wifi/store state lines
//!   log.rs      diagnostics queue for tasks that do not own the UART
//!   sys.rs      heap + millisecond clock
//!   store.rs    the one flash sector we own
//!   net.rs      WiFi: join, or own a hotspot and serve the config page
//!   board.rs    pins, measured panel parameters, flash layout

#![no_std]
#![no_main]

extern crate alloc;

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_hal::delay::Delay;
use esp_hal::gpio::{Input, Level, Output, OutputConfig};
use esp_hal::interrupt::software::SoftwareInterruptControl;
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::spi::Mode as SpiMode;
use esp_hal::time::Rate;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::uart::{Config as UartConfig, Uart};
use esp_storage::FlashStorage;

use panel_core::badge;
use panel_core::boot::{self, Step};
use panel_core::canvas::{Canvas, BG};
use panel_core::link::{Inbox, Route, Transport};
use panel_core::scene::{Rows, Scene, FRAME_BYTES, LCD_H, LCD_W};
use panel_core::ui::{Feed, Page};
use panel_core::wire::LINE_CAP;

mod board;
mod bootstate;
mod diag;
mod link;
mod log;
mod net;
mod keys;
mod net_link;
mod panel;
mod store;
mod sys;

esp_bootloader_esp_idf::esp_app_desc!();

/// The whole frame lives in RAM: 25600 bytes, the exact size the PC renders,
/// so the CRC covers pixel-for-pixel the same image.
static mut FRAME: [u8; FRAME_BYTES] = [0; FRAME_BYTES];

/// One line in flight from a transport to whatever consumes it.
///
/// `LINE_CAP` and not the 48-byte bench limit: a protocol line may be 1040
/// bytes, and truncating it here would turn a long `ui` into a short and
/// wrong one -- which T6 would then rebuild the screen from. Lives in a static
/// because two of these on a task stack is 2 KB of the executor's arena.
static mut RX_LINE: [u8; LINE_CAP] = [0; LINE_CAP];

/// How often the render task looks at the UART. The hardware FIFO holds about
/// 11 ms of 115200 baud, so anything above that loses bytes; 5 ms keeps the
/// executor cheap and the wire safe.
const UART_POLL_MS: u64 = 5;
/// Last init step entered, so a panic before the banner still says where it
/// died. Cheap (one store per step) and survives nothing -- it is rewritten
/// every boot, which is all a boot loop needs.
static STAGE: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    // SPEC 7.3: one line, then reset. `esp-println` writes to the UART itself
    // instead of going through `link::UartLine`, which the render task owns --
    // a panic may well have happened inside that task.
    esp_println::println!(
        "\nPANIC stage={} {}",
        STAGE.load(core::sync::atomic::Ordering::Relaxed),
        info
    );
    // 115200 baud needs ~9 ms for a line like this. Resetting immediately
    // truncates it mid-path, which is exactly how this line was found.
    esp_hal::rom::ets_delay_us(40_000);
    esp_hal::system::software_reset()
}

/// Everything the render task owns. One struct so the task takes one argument,
/// and so it is obvious what the other tasks cannot touch.
struct Ui {
    lcd: panel::St7735<'static>,
    transport: link::UartLine<'static>,
    /// The daemon link's render-side half: two channel handles and an atomic.
    /// Present even when no daemon exists -- `is_up()` is then simply false.
    tcp: net_link::TcpLink,
    /// The page that was on the glass last time the board lost power (T6c).
    ///
    /// Read in `main` before the store moves into the network task, then handed
    /// over here so the panel can draw before WiFi exists (PROTOCOL 22.3).
    saved: Option<panel_core::ui::Layout>,
    /// The four breadboard keys (T6d). Owned here because the pins are handed
    /// out once, in `main`, and only the render loop reads them.
    keys: keys::Keys<'static>,
}

// `bench-colours` halts right after the bars, so the rest of main is unreachable
// *by design* in that build; the allow is scoped to this function only.
#[cfg_attr(feature = "bench-colours", allow(unreachable_code))]
#[esp_hal::main]
fn main() -> ! {
    let st = |n: u32| STAGE.store(n, core::sync::atomic::Ordering::Relaxed);

    st(1);
    let peripherals = esp_hal::init(esp_hal::Config::default());
    // The allocator comes up before anything else allocates, including the
    // scheduler and the WiFi driver.
    st(2);
    sys::init_heap();

    // Scheduler and time driver before the first `.await`: every `Timer` in
    // this program depends on this having run.
    st(3);
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    st(4);
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    st(5);
    esp_rtos::start(timg0.timer0, sw_int.software_interrupt0);

    st(6);
    let mut delay = Delay::new();

    // UART first: the banner before the panel is part of the boot contract.
    st(7);
    let uart = Uart::new(peripherals.UART0, UartConfig::default())
        .unwrap()
        .with_tx(peripherals.GPIO1)
        .with_rx(peripherals.GPIO3);
    st(8);
    let mut transport = link::UartLine::new(uart);
    diag::banner(&mut transport);

    // One line, two capabilities. The probe is released right after so the
    // heap the later cards budget against is the full amount again.
    let probe = sys::probe();
    diag::heap_clock(&mut transport, probe.size, probe.free, sys::now_ms());
    diag::stack_total(&mut transport, sys::stack_bytes());
    if !probe.verified {
        diag::heap_unverified(&mut transport);
    }
    drop(probe);

    st(10);
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
    st(11);
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

    // Bench-only colour bars (see Cargo.toml). Three equal thirds, each one a
    // pure primary, and then stop: if the running app took over it would paint
    // the self-test card straight over the bars and the test would prove nothing.
    // The left bar is 0xF800 -- red *only if* the glass is RGB.
    // The allow below is not a cover-up: this build is *supposed* to stop here,
    // so everything after the halt is unreachable by design.
    #[cfg(feature = "bench-colours")]
    #[allow(unreachable_code)]
    {
        const RED: u16 = 0xF800;
        const GREEN: u16 = 0x07E0;
        const BLUE: u16 = 0x001F;
        {
            let mut canvas = Canvas::new(&mut frame[..], LCD_W, LCD_H);
            canvas.clear(BG);
            canvas.fill_rect(0, 0, 53, LCD_H, RED);
            canvas.fill_rect(53, 0, 54, LCD_H, GREEN);
            canvas.fill_rect(107, 0, 53, LCD_H, BLUE);
        }
        lcd.blit_rect(0, 0, LCD_W as u16, LCD_H as u16, &frame[..]);
        diag::colours(&mut transport, board::MADCTL, board::INVERT);
        loop {
            delay.delay_millis(1000);
        }
    }

    // First row of the self-test card. Published after the first blit and not
    // before: "panel ok" must mean pixels actually reached the glass, not that
    // a handle exists.
    bootstate::announce(boot::PANEL, Step::Ok);
    // Credentials before WiFi: a missing record is not an error, it is the
    // "open the hotspot" case. A layout mismatch is logged once, here.
    st(12);
    let flash = FlashStorage::new(peripherals.FLASH);
    let (mut store, creds) = match store::Store::open(flash) {
        Ok(mut store) => {
            let opened = store.load();

            // Bench hook (see Cargo.toml): fake a pre-T4 board, so the
            // migration path is exercised on hardware without depending on the
            // user's password. Writes the old layout, then resets.
            #[cfg(feature = "bench-legacy")]
            if opened.creds.is_none() && opened.copy.is_none() {
                let fake = panel_core::creds::WifiCreds::new("bench-legacy", "bench-legacy-psk");
                if let Some(fake) = fake {
                    if store.write_legacy_for_bench(&fake) {
                        esp_hal::system::software_reset();
                    }
                }
            }

            // Bench hook (see Cargo.toml): exercise one real save on boot, so
            // the A/B switch can be tested without a phone and a web page.
            #[cfg(feature = "bench-save-on-boot")]
            let opened = {
                if let Some(creds) = opened.creds {
                    if store.save(&creds) {
                        diag::wifi_save_ok(&mut transport, creds.ssid());
                    } else {
                        diag::wifi_save_bad(&mut transport, "bench-save");
                    }
                }
                store.load()
            };

            // Bench hook (see Cargo.toml): pair a token without a phone. The
            // real path is the config page in AP mode, which needs someone to
            // type 16 characters into a browser; this writes the same record
            // so a strict daemon can be tested by a script.
            #[cfg(feature = "bench-seed-token")]
            let opened = {
                if let Some(creds) = opened.creds {
                    if !creds.has_token() {
                        match creds.with_token(board::BENCH_TOKEN) {
                            Some(paired) => {
                                if store.save(&paired) {
                                    diag::token_state(&mut transport, true, paired.token().len());
                                } else {
                                    diag::wifi_save_bad(&mut transport, "bench-token");
                                }
                            }
                            None => diag::wifi_save_bad(&mut transport, "bench-token-invalid"),
                        }
                    }
                }
                store.load()
            };

            // Bench switch (see Cargo.toml): keep the record, break the secret,
            // so the retry ladder's dead end can be walked without erasing
            // what the user typed.
            #[cfg(feature = "seed-bad-psk")]
            let opened = {
                let mut opened = opened;
                opened.creds = opened
                    .creds
                    .and_then(|c| panel_core::creds::WifiCreds::new(c.ssid(), "bench-wrong-password"));
                opened
            };

            if opened.migrated {
                diag::store_migrated(&mut transport);
            }
            diag::store_ok(
                &mut transport,
                opened.creds.is_some(),
                opened.copy.map(|c| (c.slot.letter(), c.seq)),
            );
            bootstate::announce(boot::STORE, Step::Ok);
            (Some(store), opened.creds)
        }
        Err(err) => {
            match err {
                store::OpenError::Layout { offset, len } => {
                    // Do not touch flash at all: AP mode still works, saving
                    // will report failure instead of writing into the wrong
                    // region.
                    diag::store_layout_bad(&mut transport, offset, len);
                }
                store::OpenError::Unreadable => {
                    diag::store_layout_bad(&mut transport, 0, 0);
                }
            }
            // Flash is the one stage that can genuinely fail at boot, so it is
            // also the only row that can turn red.
            bootstate::announce(boot::STORE, Step::Fail);
            (None, None)
        }
    };

    // T6d: the four keys, pull-ups on. Built here because `peripherals` is only
    // handed out in `main`; the render loop is the only reader.
    let key_cfg = keys::Keys::config();
    let keys = keys::Keys::new(
        Input::new(peripherals.GPIO32, key_cfg),
        Input::new(peripherals.GPIO33, key_cfg),
        Input::new(peripherals.GPIO25, key_cfg),
        Input::new(peripherals.GPIO26, key_cfg),
        sys::now_ms(),
    );

    // T6c: read the saved page here, while the store is still ours. `net_task`
    // takes ownership below (the chip has one flash, so the firmware has one
    // owner for it), which makes this the last moment the UI side can ask what
    // was on the glass before the power went away.
    let saved_page = store.as_mut().and_then(|s| s.load_layout());

    // The scheduler is up; hand over to the async world. `executor.run` never
    // returns, which is why the executor may live on this stack.
    st(13);
    let mut executor = esp_rtos::embassy::Executor::new();
    let executor = unsafe { &mut *(&mut executor as *mut esp_rtos::embassy::Executor) };
    executor.run(move |spawner| {
        spawner.spawn(
            ui_task(Ui {
                lcd,
                transport,
                tcp: net_link::TcpLink::new(),
                saved: saved_page,
                keys,
            })
            .unwrap(),
        );
        spawner
            .spawn(net_task(spawner, peripherals.WIFI, creds, store).unwrap());
    })
}

/// WiFi lives in its own task so the render task keeps its cadence even while
/// the radio is scanning or waiting for an association.
#[embassy_executor::task]
async fn net_task(
    spawner: Spawner,
    wifi: esp_hal::peripherals::WIFI<'static>,
    creds: Option<panel_core::creds::WifiCreds>,
    store: Option<store::Store<'static>>,
) {
    net::run(spawner, wifi, creds, store).await
}

#[embassy_executor::task]
async fn ui_task(mut ui: Ui) -> ! {
    let mut scene = Rows::new();
    let mut inbox = Inbox::new();
    // The page the daemon describes: last accepted layout plus the newest
    // readings. A local, not a field of `Ui`, because the dispatch below borrows
    // `ui.transport`/`ui.tcp` for the whole `inbox.poll` call.
    let mut page = Page::new();
    // T6c: put the last page back before WiFi exists. Without this the panel is
    // blank after every power cut until a PC happens to be running, and "the
    // board is dead" and "the board is waiting" look identical on the glass.
    let mut saved_ver = 0u32;
    if let Some(saved) = ui.saved.take() {
        saved_ver = saved.ver;
        let (scene_no, ver_no, node_count) = (saved.scene, saved.ver, saved.len());
        page.adopt(saved);
        diag::page_restored(&mut ui.transport, scene_no, ver_no, node_count);
    }
    // Set inside the dispatch closure, acted on after it returns: that closure
    // already borrows the transport and the inbox, and a flash erase in the
    // middle of a half-dispatched frame is not a thing to do.
    let mut save_request: Option<u32> = None;
    // Whether the badge is on the glass right now. Kept here rather than inside
    // the scene: the badge is not part of any layout, and a scene that had to
    // know about the link would be asking the wrong layer.
    let mut badge_shown = false;
    // Which boot-card state is on the glass right now (`None` = nothing painted
    // yet). Same edge rule as the badge: repaint when the picture would change,
    // not every tick -- a full blit is 25 KB over SPI.
    let mut boot_shown: Option<[Step; boot::STEPS]> = None;
    let rx = unsafe { &mut *core::ptr::addr_of_mut!(RX_LINE) };
    let frame = unsafe { &mut *core::ptr::addr_of_mut!(FRAME) };

    loop {
        // Lines produced by other tasks (WiFi, store, the daemon link) get
        // written first, so a state change never waits behind a redraw.
        log::drain_into(&mut ui.transport);

        // Every wire, one inbox. The UART carries the bench format, the socket
        // carries JSON, and the first byte decides which -- so "two links at
        // once" is not a state this loop has to arbitrate. Since T6 a JSON line
        // is a page description whichever wire it came from, which is what lets
        // a page be proven over the cable with no WiFi and no daemon.
        {
            let mut sources: [&mut dyn Transport; 2] = [&mut ui.transport, &mut ui.tcp];
            // Diagnostics produced here cannot borrow the UART (it is inside
            // `sources`); they go through the queue and are written next tick,
            // 5 ms later. A page that arrives is therefore always followed by
            // its `PAGE` line before the next frame.
            let mut sink = log::ChanLog::new();
            let now = sys::now_ms();
            inbox.poll(&mut sources, rx, |route, line| match route {
                Route::Bench => scene.apply(line),
                Route::Proto => match page.feed(line, now) {
                    Feed::Layout => {
                        let shown = page.layout().unwrap();
                        diag::page_ui(&mut sink, shown.scene, shown.ver, shown.len());
                        // Only a `ver` that actually moved is worth a flash
                        // erase. A daemon resends `ui` on every handshake
                        // (PROTOCOL 16 red line 4) and none of those resends is
                        // a change: `ver` is the daemon's word for "different".
                        if shown.ver != saved_ver {
                            save_request = Some(shown.ver);
                        }
                    }
                    Feed::Data(keys) => diag::page_data(&mut sink, keys),
                    // Refused, not ignored: the old page stays on the glass, and
                    // the word says why. Silence here would look like a page
                    // that never arrived.
                    Feed::Refused(err) => diag::page_reject(&mut sink, err.word()),
                    Feed::Ignored => {}
                },
            });
        }

        // T6d: keys move the focus and the daemon is told which item is
        // selected. A terminal without keys never reaches here, and a page with
        // nothing focusable makes `focus_step` a no-op that still reports
        // position 0 -- so pressing keys on a read-only page costs nothing.
        let now = sys::now_ms();
        if let Some(press) = ui.keys.poll(now) {
            match press {
                keys::Press::Up => page.focus_step(-1),
                keys::Press::Down => page.focus_step(1),
                // Confirm and Back move nothing; they only report. Acting on
                // them is the daemon's job, and it needs the position to do it.
                keys::Press::Confirm | keys::Press::Back => {}
            }
            let position = page.focus_position();
            let name = match press {
                keys::Press::Up => "up",
                keys::Press::Down => "down",
                keys::Press::Confirm => "ok",
                keys::Press::Back => "back",
            };
            diag::focus_press(&mut ui.transport, name, position);
            if position > 0 {
                let mut line = [0u8; 48];
                if let Some(len) = keys::focus_event(&mut line, position) {
                    let _ = ui.tcp.write_all(&line[..len]);
                }
            }
        }

        // Persist the page. Deliberately outside the dispatch closure and only
        // on a `ver` change: an erase costs ~50 ms and a sector has a finite
        // write budget, so this follows the operator's edits, not the frame
        // rate. A failure leaves the picture alone and says so, because the
        // silent version of this bug is "the new page survives until reboot".
        if let Some(ver) = save_request {
            let saved_ok = match page.layout() {
                Some(layout) => crate::net::with_store(|s| s.save_layout(layout))
                    .await
                    .unwrap_or(false),
                None => false,
            };
            if saved_ok {
                saved_ver = ver;
                diag::page_saved(&mut ui.transport, ver);
            } else {
                diag::page_save_failed(&mut ui.transport, ver);
            }
        }

        // SPEC 4.9: a dead link must not blank the picture -- the last layout
        // stays and gets a `LINK LOST` badge, because the PC can no longer say
        // anything and this is the only channel left. Redraw only on the edge:
        // a per-tick repaint would cost 25 KB of SPI 10 times a second to draw
        // the same badge.
        //
        // "Has a layout" now means either wire: the bench format paints two
        // rows, a `ui` paints a whole page. Both are content the badge must not
        // sit on top of a blank screen for.
        let has_content = scene.has_layout() || page.has_layout();
        let link_lost = has_content && !ui.tcp.is_up();
        let badge_changed = link_lost != badge_shown;
        badge_shown = link_lost;
        if badge_changed {
            // Said out loud because the badge is painted by this board alone: a
            // badge that never clears is invisible to every other form of
            // evidence (it looks exactly like "nothing changed").
            diag::badge(&mut ui.transport, link_lost);
        }

        // The self-test card is the idle page: shown while the glass has no
        // layout of its own (nothing from the PC yet), and replaced the moment
        // real content arrives. Before this, that window was a black screen,
        // which on a monitor is indistinguishable from a dead board.
        let steps = bootstate::snapshot();
        let show_boot = !has_content;
        let boot_changed = show_boot && boot_shown != Some(steps);
        if boot_changed {
            boot_shown = Some(steps);
        }

        // Redraw on a scene change, on the badge appearing/disappearing, on a
        // self-test row settling, or on a page/data line that moved the picture.
        // A per-tick repaint would cost 25 KB of SPI ten times a second to draw
        // the same picture.
        //
        // `is_dirty_at` rather than `is_dirty`: a reading going past its
        // deadline is a change to the picture that no message announced
        // (PROTOCOL 9.1). Without it the last number stays on the glass forever
        // once the daemon goes quiet -- the exact lie the rule forbids. It is an
        // edge: once the page is drawn as `--` it stops asking.
        let now = sys::now_ms();
        if scene.is_dirty() || page.is_dirty_at(now) || badge_changed || boot_changed {
            // One render = one whole frame = one blit. Never paint while
            // streaming pixels over SPI.
            let digest = {
                let mut canvas = Canvas::new(&mut frame[..], LCD_W, LCD_H);
                if show_boot {
                    boot::draw(&mut canvas, &steps);
                } else if page.has_layout() {
                    // A `ui` page outranks the bench format: the two-row text is
                    // the bench, the page is the product. Both cannot be right
                    // at once, and the one that came with a whole layout wins.
                    // `render_recording`, not `render`: it also records which
                    // nodes went to the glass as a number. That word is the only
                    // fact a later tick can use to notice a figure has aged out
                    // (PROTOCOL 9.1); plain `render` leaves it stale and the rule
                    // silently stops working.
                    page.render_recording(&mut canvas, now);
                    if badge_shown {
                        badge::draw(&mut canvas);
                    }
                } else {
                    scene.render(&mut canvas);
                    if badge_shown {
                        badge::draw(&mut canvas);
                    }
                }
                canvas.digest()
            };
            diag::frame_line(&mut ui.transport, LCD_W, LCD_H, digest, FRAME_BYTES);
            ui.lcd.blit_rect(0, 0, LCD_W as u16, LCD_H as u16, &frame[..]);
            scene.mark_clean();
            page.mark_clean_at(now);
        }


        Timer::after(Duration::from_millis(UART_POLL_MS)).await;
    }
}
