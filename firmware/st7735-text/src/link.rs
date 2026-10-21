//! UART0 transport: bytes in, whole lines out.
//!
//! What this does NOT do: no framing beyond `\n`, no checksums, no retries, no
//! blocking wait for input. `poll_line` reads what is already there and returns
//! `Ok(None)` when there is nothing -- the render loop must never stall on a
//! silent cable.

use esp_hal::uart::Uart;
use esp_hal::Blocking;

use panel_core::link::{LinkError, Transport};
use panel_core::scene::LINE_MAX;

/// Read size for one `uart.read` call.
const CHUNK: usize = 64;

pub struct UartLine<'d> {
    uart: Uart<'d, Blocking>,
    /// Bytes of the current, unfinished line.
    line: [u8; LINE_MAX],
    fill: usize,
    /// True while skipping the rest of an over-long line (SPEC 3.2 / E1).
    skipping: bool,
    /// Bytes pulled from the UART but not consumed yet.
    stage: [u8; CHUNK],
    stage_len: usize,
    stage_pos: usize,
    overlong: u32,
    read_errors: u32,
}

impl<'d> UartLine<'d> {
    pub fn new(uart: Uart<'d, Blocking>) -> Self {
        Self {
            uart,
            line: [0; LINE_MAX],
            fill: 0,
            skipping: false,
            stage: [0; CHUNK],
            stage_len: 0,
            stage_pos: 0,
            overlong: 0,
            read_errors: 0,
        }
    }

    /// Counters for the banner / diagnostics. Nothing consumes them yet; they
    /// exist so that a future `stat` line has something true to report
    /// (PROTOCOL §15). Not printed, so the wire stays byte-identical to P1.
    #[allow(dead_code)]
    pub fn overlong_lines(&self) -> u32 {
        self.overlong
    }

    #[allow(dead_code)]
    pub fn read_errors(&self) -> u32 {
        self.read_errors
    }

    fn refill(&mut self) -> bool {
        let n = match self.uart.read(&mut self.stage) {
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
                match byte {
                    b'\n' => {
                        let len = self.fill;
                        let dropped = self.skipping;
                        self.fill = 0;
                        self.skipping = false;
                        if dropped {
                            continue;
                        }
                        if len > 0 {
                            let n = if len < out.len() { len } else { out.len() };
                            out[..n].copy_from_slice(&self.line[..n]);
                            return Ok(Some(n));
                        }
                        // Blank line: nothing to draw, keep looking.
                    }
                    b'\r' => {}
                    _ => {
                        if self.skipping {
                            continue;
                        }
                        if self.fill < LINE_MAX && self.fill < out.len() {
                            self.line[self.fill] = byte;
                            self.fill += 1;
                        } else {
                            // Over-long: drop the whole line, not half of it.
                            self.overlong = self.overlong.wrapping_add(1);
                            self.fill = 0;
                            self.skipping = true;
                        }
                    }
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
