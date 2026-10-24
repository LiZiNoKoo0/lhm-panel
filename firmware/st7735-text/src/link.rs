//! UART0 transport: bytes in, whole lines out.
//!
//! What this does NOT do: no framing beyond `\n`, no checksums, no retries, no
//! blocking wait for input. `poll_line` reads what is already there and returns
//! `Ok(None)` when there is nothing -- the render loop must never stall on a
//! silent cable.
//!
//! Since T5.2 the splitting itself is `panel_core::link::LineBuf`, shared with
//! the TCP link. This file is now only about the UART: how to get bytes out of
//! a FIFO without ever waiting for them.

use esp_hal::uart::Uart;
use esp_hal::Blocking;

use panel_core::link::{LineBuf, LinkError, Transport};
use panel_core::wire::LINE_CAP;

/// Read size for one `uart.read_buffered` call.
const CHUNK: usize = 64;

/// How long a line the UART accepts.
///
/// This used to be `scene::LINE_MAX` (48), which is the bench format's limit and
/// was silently too short the moment a JSON line could arrive here: an `Inbox`
/// classifies a line by its first byte, so a `ui` sent over the cable is a
/// protocol line, and 48 bytes would drop it whole as an overflow. Same cap as
/// the socket, so both wires carry the same messages (`check_parity --ui`
/// depends on exactly that).
const UART_LINE_MAX: usize = LINE_CAP;

pub struct UartLine<'d> {
    uart: Uart<'d, Blocking>,
    /// The shared splitter: `\n` framing, whole-line drop on overflow, E1 count.
    lines: LineBuf<UART_LINE_MAX>,
    /// Bytes pulled from the UART but not consumed yet.
    stage: [u8; CHUNK],
    stage_len: usize,
    stage_pos: usize,
    read_errors: u32,
}

impl<'d> UartLine<'d> {
    pub fn new(uart: Uart<'d, Blocking>) -> Self {
        Self {
            uart,
            lines: LineBuf::new(),
            stage: [0; CHUNK],
            stage_len: 0,
            stage_pos: 0,
            read_errors: 0,
        }
    }

    /// Counters for the banner / diagnostics. Nothing consumes them yet; they
    /// exist so that a future `stat` line has something true to report
    /// (PROTOCOL §15). Not printed, so the wire stays byte-identical to P1.
    #[allow(dead_code)]
    pub fn overlong_lines(&self) -> u32 {
        self.lines.overlong()
    }

    #[allow(dead_code)]
    pub fn read_errors(&self) -> u32 {
        self.read_errors
    }

    fn refill(&mut self) -> bool {
        // `Uart::read` blocks until a byte arrives -- esp-hal documents exactly
        // that -- and this is called from an async task on a single-threaded
        // executor. One silent cable and the render loop never returns: no
        // timers fire, no other task is ever polled, the panel never draws, and
        // the WiFi task never starts. `read_buffered` returns what is already in
        // the FIFO, zero included, which is what "poll" is supposed to mean.
        let n = match self.uart.read_buffered(&mut self.stage) {
            Ok(n) => n,
            Err(_) => {
                self.read_errors = self.read_errors.wrapping_add(1);
                0
            }
        };
        self.stage_len = n;
        self.stage_pos = 0;
        n > 0
    }
}

impl<'d> Transport for UartLine<'d> {
    fn poll_line(&mut self, out: &mut [u8]) -> Result<Option<usize>, LinkError> {
        loop {
            if self.stage_pos >= self.stage_len && !self.refill() {
                return Ok(None);
            }
            while self.stage_pos < self.stage_len {
                let byte = self.stage[self.stage_pos];
                self.stage_pos += 1;
                if let Some(len) = self.lines.push(byte) {
                    let n = if len < out.len() { len } else { out.len() };
                    out[..n].copy_from_slice(&self.lines.line(len)[..n]);
                    return Ok(Some(n));
                }
            }
        }
    }

    fn write_all(&mut self, bytes: &[u8]) -> Result<(), LinkError> {
        self.uart.write(bytes).map_err(|_| LinkError::Io)?;
        self.uart.flush().map_err(|_| LinkError::Io)?;
        Ok(())
    }

    fn is_up(&self) -> bool {
        // A UART has no notion of a peer; the link is up as long as the cable
        // is plugged in, which we cannot see from here.
        true
    }
}
