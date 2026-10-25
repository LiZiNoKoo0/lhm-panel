//! WiFi: exactly one of three states at a time.
//!
//!   no credentials            -> own a hotspot and serve the config page
//!   credentials, no address   -> 30 s, then stop trying and own a hotspot
//!   credentials, address      -> stay connected, report the IP, stop serving
//!
//! What this does NOT do: it never touches pixels, never formats a log line
//! (that is `diag`), and never writes flash itself (`store`). AP+STA
//! coexistence is deliberately absent: it costs a second set of buffers and
//! the three states above are mutually exclusive anyway.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use core::fmt::Write as _;
use core::net::{Ipv4Addr, SocketAddr};

use embassy_executor::Spawner;
use embassy_net::{Config as NetConfig, Ipv4Cidr, Runner, Stack, StackResources, StaticConfigV4};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Instant, Timer};
use edge_dhcp::server::{Server, ServerOptions};
use edge_dhcp::{Options, Packet};
use edge_nal::{UdpBind, UdpReceive, UdpSend};
use edge_nal_embassy::{Udp, UdpBuffers};
use esp_hal::peripherals::WIFI;
use esp_radio::wifi::ap::AccessPointConfig;
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::scan::ScanConfig;
use esp_radio::wifi::{Config as WifiConfig, ControllerConfig, Interface, WifiController, WifiError};
use heapless::Vec as HeaplessVec;
use picoserve::extract::{Form, State};
use picoserve::response::{IntoResponse, Response, StatusCode};
use picoserve::routing::{get, post, Router};
use panel_core::creds::WifiCreds;

use crate::board;
use crate::bootstate;
use crate::diag;
use crate::log::ChanLog;
use crate::store::Store;

/// The self-test row this module fills in.
use panel_core::boot::{self as bootcard, Step};

/// Sockets the embassy-net stack may hand out: the DHCP server's UDP socket,
/// the HTTP listener, picoserve's accepted connection, the T5 daemon socket,
/// and room for one reconnect where the old socket has not been reclaimed yet.
/// Two was too few -- the third allocation panicked inside smoltcp with
/// "adding a socket to a full SocketSet", which looks like a WiFi failure and
/// is really an array that is one element short.
const SOCKETS: usize = 6;

/// The page is served on this port; 80 needs no explaining to a phone.
const HTTP_PORT: u16 = 80;

/// How long the "saved" page gets to reach the browser before the reset.
const REBOOT_DELAY_MS: u64 = 600;

/// At most this many networks in the list: the page must stay under 4 KB.
const MAX_LISTED: usize = 16;

/// Association attempts inside one 30 s budget (see `try_station` for why more
/// than one is needed at all).
const JOIN_ATTEMPTS: u32 = 3;
/// Longest a single attempt may take; the remaining budget caps it further.
const ATTEMPT: Duration = Duration::from_secs(12);
/// Below this, one more attempt cannot succeed in time.
const MIN_ATTEMPT: Duration = Duration::from_secs(3);

// The controller and the store outlive any single handler: picoserve calls
// handlers from the server task, while `run` owns both objects.
static CTRL: Mutex<CriticalSectionRawMutex, Option<WifiController<'static>>> = Mutex::new(None);
static STORE: Mutex<CriticalSectionRawMutex, Option<Store<'static>>> = Mutex::new(None);

/// Run `f` against the flash store, if one opened at boot.
///
/// The store is a single owned `FlashStorage`: the chip has one, so the firmware
/// has one. Rather than hand a second handle to the UI task (impossible) or move
/// the store back out of the network task (which needs it for the config page),
/// the UI task borrows it through here for the one thing it owns -- persisting
/// the page it just painted (T6c).
///
/// `None` means the store never opened, which is a boot-time failure already
/// reported as `STORE fail`; callers treat it the same as a failed write.
pub async fn with_store<R>(f: impl FnOnce(&mut Store<'static>) -> R) -> Option<R> {
    let mut guard = STORE.lock().await;
    guard.as_mut().map(f)
}

#[derive(Clone)]
struct AppState {
    spawner: Spawner,
}

// -- tasks ------------------------------------------------------------------

/// Drives the embassy-net stack. `run()` never returns, so this task never
/// finishes; it is the thing that keeps packets moving.
#[embassy_executor::task]
pub async fn stack_runner(mut runner: Runner<'static, Interface<'static>>) {
    runner.run().await
}

/// Reset a moment after the response has been handed to TCP.
#[embassy_executor::task]
async fn reboot_soon() {
    Timer::after(Duration::from_millis(REBOOT_DELAY_MS)).await;
    esp_hal::system::software_reset();
}

/// Hands out addresses on the soft-AP. Without this a phone joins the hotspot
/// and then sits there with no IP, which looks exactly like a dead board.
///
/// The receive loop is written out here instead of calling
/// `edge_dhcp::io::server::run`, because that helper swallows every error and
/// gives no way to see whether a packet ever arrived -- which is the only
/// question worth asking when a client sits at "obtaining IP address".
///
/// Nothing here writes the UART directly: lines go through `ChanLog` so the
/// render task keeps sole ownership of the wire.
#[embassy_executor::task]
async fn dhcp_task(stack: Stack<'static>) -> ! {
    let mut buffers: UdpBuffers<1, 600, 600, 2> = UdpBuffers::new();
    let udp = Udp::new(stack, &mut buffers);
    let mut rx = [0u8; 600];
    let mut tx = [0u8; 600];
    let server_ip = Ipv4Addr::new(
        board::AP_IP[0],
        board::AP_IP[1],
        board::AP_IP[2],
        board::AP_IP[3],
    );

    loop {
        let socket = udp.bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 67))).await;
        let mut socket = match socket {
            Ok(socket) => socket,
            Err(_) => {
                diag::wifi_fail(&mut ChanLog::new(), "dhcp-bind");
                Timer::after(Duration::from_secs(1)).await;
                continue;
            }
        };
        diag::dhcp_up(&mut ChanLog::new());

        let mut gw_buf = [Ipv4Addr::UNSPECIFIED];
        let options = ServerOptions::new(server_ip, Some(&mut gw_buf));
        // Lease times are only compared against each other, so a clock that
        // restarts at every boot is fine here.
        let mut server = Server::<_, 64>::new(|| (crate::sys::now_ms() / 1000) as u64, server_ip);

        loop {
            let (len, from) = match socket.receive(&mut rx).await {
                Ok(pair) => pair,
                Err(_) => {
                    diag::wifi_fail(&mut ChanLog::new(), "dhcp-rx");
                    break;
                }
            };
            let _ = from;

            let request = match Packet::decode(&rx[..len]) {
                Ok(request) => request,
                Err(_) => {
                    diag::wifi_fail(&mut ChanLog::new(), "dhcp-decode");
                    continue;
                }
            };

            let mut opt_buf = Options::buf();
            if let Some(reply) = server.handle_request(&mut opt_buf, &options, &request) {
                let bytes = match reply.encode(&mut tx) {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        diag::wifi_fail(&mut ChanLog::new(), "dhcp-encode");
                        continue;
                    }
                };
                // Always broadcast: a client that is still asking for an
                // address has none, so there is nothing to unicast to.
                let dest = SocketAddr::from((Ipv4Addr::BROADCAST, 68));
                if socket.send(dest, bytes).await.is_err() {
                    diag::wifi_fail(&mut ChanLog::new(), "dhcp-send");
                }
            }
        }

        Timer::after(Duration::from_millis(200)).await;
    }
}

// -- entry ------------------------------------------------------------------

/// Bring WiFi up and never come back. Called once, from the main task.
pub async fn run(
    spawner: Spawner,
    wifi: WIFI<'static>,
    creds: Option<WifiCreds>,
    store: Option<Store<'static>>,
) -> ! {
    let mut log = ChanLog::new();
    let ap_ssid = ap_ssid();

    // Printed before the radio is touched, so a boot that dies in here can be
    // told apart from one that dies before it (see `diag::wifi_init`).
    diag::wifi_init(&mut log);

    let (mut controller, interfaces) = match esp_radio::wifi::new(wifi, ControllerConfig::default()) {
        Ok(pair) => pair,
        Err(err) => {
            diag::wifi_fail(&mut log, &format!("{:?}", err));
            Timer::after(Duration::from_secs(2)).await;
            esp_hal::system::software_reset();
        }
    };

    // Hand the store to the handlers before anything can ask for it. `None`
    // here means "flash layout did not check out": the page still opens, saving
    // just reports failure instead of writing into the wrong region.
    *STORE.lock().await = store;

    if let Some(creds) = creds.as_ref() {
        if try_station(&spawner, &mut controller, interfaces.station, creds, &mut log)
            .await
            .is_some()
        {
            // Connected: keep the controller alive forever. Dropping it stops
            // WiFi, so there is no "task finished" state to go back to.
            *CTRL.lock().await = Some(controller);
            loop {
                Timer::after(Duration::from_secs(3600)).await;
            }
        }
        // Credentials existed and did not work: the one row that means "look
        // here". The hotspot comes up next, so the card stays on the glass and
        // the red row explains why the panel is not showing data yet.
        bootstate::announce(bootcard::WIFI, Step::Fail);
    } else {
        // No credentials: not a failure, a job for the person in front of it.
        // `...` says "waiting", which is exactly the state.
        bootstate::announce(bootcard::WIFI, Step::Busy);
    }

    // Either there were no credentials or the join failed: own the hotspot.
    // The station interface was consumed by the attempt above -- a partial
    // move of `interfaces`, which is why the AP field is still usable.
    *CTRL.lock().await = Some(controller);
    start_ap(&spawner, interfaces.access_point, ap_ssid, &mut log).await
}

/// One connection attempt. Returns the live stack if an address arrived.
///
/// No stack is created unless the association succeeds: that is what makes the
/// fallback to AP mode clean. If we spawned the DHCP stack up front and the
/// join failed, its runner task would keep polling a link that never comes up
/// (and could never be stopped).
/// Why a join died, in words short enough to survive one log line.
///
/// `{:?}` on `WifiError::Disconnected` prints the whole station record -- ssid,
/// bssid, reason, rssi -- which is longer than the line, so the field that
/// matters (the reason name) was exactly what got cut off the end. Print the
/// reason and drop the rest: the ssid is on the `WIFI sta` line above it.
fn join_reason(err: &WifiError) -> String {
    match err {
        WifiError::Disconnected(info) => format!("Disconnected({:?})", info.reason),
        other => format!("{:?}", other),
    }
}

async fn try_station(
    spawner: &Spawner,
    controller: &mut WifiController<'static>,
    interface: Interface<'static>,
    creds: &WifiCreds,
    log: &mut ChanLog,
) -> Option<Stack<'static>> {
    diag::wifi_sta(log, creds.ssid());

    let cfg = WifiConfig::Station(
        StationConfig::default()
            .with_ssid(creds.ssid())
            .with_password(creds.psk().to_string()),
    );
    if let Err(err) = controller.set_config(&cfg) {
        diag::wifi_fail(log, &format!("{:?}", err));
        return None;
    }

    // The driver is configured (by this crate's defaults, which it does not let
    // us change) with `failure_retry_cnt = 1` and a *fast* scan -- cached
    // channel only. On a cold boot there is no cached channel yet, so an AP
    // that is perfectly reachable can be missed, and the join dies in about a
    // second. The bench board did exactly that: joined at 06:05, failed 1.6 s
    // after `WIFI sta` at 06:04, same credentials, same router. Retrying here
    // -- inside the same 30 s budget -- turns a coin-flip boot into an
    // exception, and it is the only knob this layer owns.
    let deadline = Instant::now() + Duration::from_millis(board::STA_TIMEOUT_MS);
    let mut joined = false;
    for _ in 0..JOIN_ATTEMPTS {
        let left = deadline.saturating_duration_since(Instant::now());
        // Not enough left for a meaningful attempt: give the hotspot the rest
        // instead of sitting on a dead radio.
        if left < MIN_ATTEMPT {
            break;
        }
        match embassy_time::with_timeout(left.min(ATTEMPT), controller.connect_async()).await {
            Ok(Ok(_info)) => {
                joined = true;
                break;
            }
            Ok(Err(err)) => diag::wifi_fail(log, &join_reason(&err)),
            Err(_) => diag::wifi_fail(log, "Timeout"),
        }
        // One second between attempts: long enough for the radio to settle,
        // short enough to stay inside the budget.
        Timer::after(Duration::from_millis(1_000)).await;
    }
    if !joined {
        return None;
    }

    // Associated, now ask for an address. A DHCP failure here also falls back
    // to the hotspot: without an address the panel is unreachable anyway.
    let (stack, runner) = embassy_net::new(
        interface,
        NetConfig::dhcpv4(Default::default()),
        Box::leak(Box::new(StackResources::<SOCKETS>::new())),
        0x5eed_1234_5678_9abc,
    );
    let _ = spawner.spawn(stack_runner(runner).unwrap());

    if embassy_time::with_timeout(Duration::from_secs(10), stack.wait_config_up())
        .await
        .is_err()
    {
        diag::wifi_fail(log, "NoDHCP");
        return None;
    }

    let ip = stack
        .config_v4()
        .map(|c| c.address.address().octets())
        .unwrap_or([0, 0, 0, 0]);
    let rssi = controller.rssi().unwrap_or(0);
    diag::wifi_up(log, ip, rssi as i8);
    bootstate::announce(bootcard::WIFI, Step::Ok);

    // T5.2: the daemon link runs on this stack. T5.5 made the peer a runtime
    // fact: a normal build now starts the pump with `None` and lets UDP
    // discovery find the daemon, so the task starts even without an address.
    if board::DISCOVERY || board::DAEMON_PEER.is_some() {
        // T5.6: the link task owns the token for its whole life, so it is leaked
        // once here instead of living in a mutex the pump would lock on every
        // reconnect. Once per boot, bounded, and never written.
        let token: &'static str =
            alloc::boxed::Box::leak(alloc::string::String::from(creds.token()).into_boxed_str());
        diag::token_state(log, creds.has_token(), token.len());
        let _ = spawner.spawn(crate::net_link::pump(stack, board::DAEMON_PEER, token).unwrap());
    }
    Some(stack)
}

/// Own the hotspot, serve the page, and never return.
async fn start_ap(
    spawner: &Spawner,
    interface: Interface<'static>,
    ap_ssid: String,
    log: &mut ChanLog,
) -> ! {
    // `AccessPointConfig::default()` is already an open network (auth None,
    // empty password). The page configures the board; it does not protect it.
    let cfg = WifiConfig::AccessPoint(AccessPointConfig::default().with_ssid(ap_ssid.as_str()));

    {
        let mut guard = CTRL.lock().await;
        match guard.as_mut() {
            Some(controller) => {
                if let Err(err) = controller.set_config(&cfg) {
                    diag::wifi_fail(log, &format!("{:?}", err));
                }
            }
            None => diag::wifi_fail(log, "NoController"),
        }
    }

    let static_v4 = StaticConfigV4 {
        address: Ipv4Cidr::new(board::AP_IP.into(), board::AP_PREFIX_LEN),
        gateway: None,
        dns_servers: HeaplessVec::new(),
    };

    let (stack, runner) = embassy_net::new(
        interface,
        NetConfig::ipv4_static(static_v4),
        Box::leak(Box::new(StackResources::<SOCKETS>::new())),
        0x1234_5eed_9abc_def0,
    );
    let _ = spawner.spawn(stack_runner(runner).unwrap());
    stack.wait_config_up().await;
    diag::wifi_ap(log, &ap_ssid, board::AP_IP);

    spawner.spawn(dhcp_task(stack).unwrap());

    let state = AppState {
        spawner: *spawner,
    };
    let app = Box::leak(Box::new(
        Router::new()
            .route("/", get(index))
            .route("/save", post(save))
            .with_state(state),
    ));

    let config = picoserve::Config::new(picoserve::Timeouts {
        start_read_request: Duration::from_secs(5),
        // A scan runs inside the handler, so the write timeout has to outlast
        // it; the read side can stay short.
        read_request: Duration::from_secs(5),
        write: Duration::from_secs(10),
        ..Default::default()
    });

    let http_buf: &'static mut [u8] = Box::leak(Box::new([0u8; 4096]));
    let tcp_rx: &'static mut [u8] = Box::leak(Box::new([0u8; 1536]));
    let tcp_tx: &'static mut [u8] = Box::leak(Box::new([0u8; 1536]));

    loop {
        let _ = picoserve::Server::new(&*app, &config, http_buf)
            .listen_and_serve(0u8, stack, HTTP_PORT, tcp_rx, tcp_tx)
            .await;
        Timer::after(Duration::from_millis(100)).await;
    }
}

// -- handlers ---------------------------------------------------------------

async fn index(State(_state): State<AppState>) -> impl IntoResponse {
    let mut log = ChanLog::new();
    let mut page = String::with_capacity(2048);
    page.push_str(PAGE_HEAD);

    // Scan on demand: AP mode is the only time the page is reachable, and a
    // background scanner would keep the radio busy for nothing.
    //
    // The result is logged rather than swallowed: "no networks listed" and "the
    // radio refused to scan" look identical on the phone, and this is the only
    // place that can tell them apart.
    let scanned = {
        let mut guard = CTRL.lock().await;
        match guard.as_mut() {
            Some(controller) => match controller.scan_async(&ScanConfig::default()).await {
                Ok(networks) => Some(networks),
                Err(err) => {
                    diag::wifi_fail(&mut log, &format!("scan {:?}", err));
                    None
                }
            },
            None => {
                diag::wifi_fail(&mut log, "scan NoController");
                None
            }
        }
    };

    let mut listed = 0;
    if let Some(networks) = scanned {
        for ap in networks.iter() {
            if listed == MAX_LISTED {
                break;
            }
            let name = ap.ssid.as_str();
            // Skip anything that would need HTML escaping: a config page must
            // not be a vector for markup, and skipping is honest about the gap.
            if name.is_empty() || name.contains('<') || name.contains('&') || name.contains('"') {
                continue;
            }
            let _ = write!(page, "<option value=\"{}\">", name);
            let _ = write!(page, "{} ({})</option>", name, ap.signal_strength);
            listed += 1;
        }
    }

    page.push_str(PAGE_TAIL);
    diag::scan_n(&mut log, listed);
    Response::ok(page).with_content_type(HTML)
}

#[derive(serde::Deserialize)]
struct SaveForm {
    ssid: String,
    psk: String,
    /// T5.6: the daemon's auth token. Optional on the form so an older page
    /// (or someone who only wants to change the WiFi) does not wipe a token
    /// that is already paired -- empty means "keep what is stored".
    #[serde(default)]
    token: String,
}

async fn save(State(state): State<AppState>, Form(form): Form<SaveForm>) -> impl IntoResponse {
    let mut log = ChanLog::new();

    let base = match WifiCreds::new(&form.ssid, &form.psk) {
        Some(creds) => creds,
        None => {
            diag::wifi_save_bad(&mut log, "InvalidCreds");
            return Response::new(StatusCode::BAD_REQUEST, SAVE_BAD_PAGE).with_content_type(HTML);
        }
    };

    // Empty field = keep the stored token; a filled one replaces it. Either way
    // the *stored* record is the truth, so the token cannot be cleared by the
    // form losing a field it never sent.
    let wanted = form.token.trim();
    let token = if wanted.is_empty() {
        let mut guard = STORE.lock().await;
        match guard.as_mut() {
            Some(store) => store.token_owned(),
            None => String::new(),
        }
    } else {
        wanted.to_string()
    };

    let creds = match base.with_token(&token) {
        Some(creds) => creds,
        None => {
            diag::wifi_save_bad(&mut log, "InvalidToken");
            return Response::new(StatusCode::BAD_REQUEST, SAVE_BAD_TOKEN_PAGE)
                .with_content_type(HTML);
        }
    };

    let saved = {
        let mut guard = STORE.lock().await;
        match guard.as_mut() {
            Some(store) => store.save(&creds),
            None => false,
        }
    };

    if !saved {
        // Kept nothing, so do not reset: the hotspot stays up and the person
        // can simply try again.
        diag::wifi_save_bad(&mut log, "FlashWriteFailed");
        return Response::new(StatusCode::BAD_REQUEST, SAVE_BAD_PAGE).with_content_type(HTML);
    }

    diag::wifi_save_ok(&mut log, creds.ssid());
    diag::token_state(&mut log, creds.has_token(), creds.token().len());
    let _ = state.spawner.spawn(reboot_soon().unwrap());
    Response::ok(SAVE_OK_PAGE).with_content_type(HTML)
}

// -- helpers ----------------------------------------------------------------

/// `st7735-` + the last two MAC bytes, so two boards on one desk differ.
fn ap_ssid() -> String {
    let mac = esp_hal::efuse::interface_mac_address(esp_hal::efuse::InterfaceMacAddress::Station);
    let bytes = mac.as_bytes();
    format!(
        "{}{:02x}{:02x}",
        board::AP_SSID_PREFIX,
        bytes[4],
        bytes[5]
    )
}

// Deliberately static and JS-free: this page has to work from any phone's
// browser, on the first try, with no network to fetch assets from.
const HTML: &str = "text/html; charset=utf-8";

const PAGE_HEAD: &str = concat!(
    "<!doctype html><html><head><meta charset=utf-8>",
    "<meta name=viewport content=\"width=device-width,initial-scale=1\">",
    "<title>st7735 setup</title></head><body>",
    "<h3>Wi-Fi for the panel</h3>",
    "<form method=post action=/save>",
    "<label>Network<br><input name=ssid list=nets autocomplete=off required></label>",
    "<datalist id=nets>"
);

const PAGE_TAIL: &str = concat!(
    "</datalist>",
    "<p><label>Password<br><input name=psk type=password autocomplete=off></label></p>",
    "<p><label>Daemon token<br>",
    "<input name=token autocomplete=off spellcheck=false ",
    "placeholder=\"leave blank to keep the stored one\"></label></p>",
    "<p><button type=submit>Save and reboot</button></p>",
    "</form>",
    "<p>Open network: leave the password empty.</p>",
    "<p>The token is the one the daemon printed at startup ",
    "(<code>--token</code> or the random hex in its log). While it is empty the ",
    "panel can only talk to a daemon started with <code>--lenient</code>.</p>",
    "</body></html>"
);

const SAVE_OK_PAGE: &str = concat!(
    "<!doctype html><html><head><meta charset=utf-8>",
    "<title>saved</title></head><body>",
    "<h3>Saved</h3><p>The panel is rebooting and will join the network.",
    " You can close this page.</p></body></html>"
);

const SAVE_BAD_PAGE: &str = concat!(
    "<!doctype html><html><head><meta charset=utf-8>",
    "<title>not saved</title></head><body>",
    "<h3>Not saved</h3><p>SSID must be 1-32 bytes, password 8-63 (or empty).",
    " Nothing was written; try again.</p></body></html>"
);

const SAVE_BAD_TOKEN_PAGE: &str = concat!(
    "<!doctype html><html><head><meta charset=utf-8>",
    "<title>not saved</title></head><body>",
    "<h3>Not saved</h3><p>The token must be at most 32 printable characters ",
    "and cannot contain a quote or a backslash (it goes inside JSON).",
    " Nothing was written; try again.</p></body></html>"
);

/// Unused import guard: `ToString` is here for the SSID/format helpers above.
#[allow(dead_code)]
fn _tostring_is_used(s: &str) -> String {
    s.to_string()
}
