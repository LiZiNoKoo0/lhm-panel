//! One-way pipe for diagnostics produced outside the render task.
//!
//! The UART belongs to exactly one task (the one that also owns the panel), so
//! nothing else may write to it directly. `net.rs` and `store.rs` therefore
//! format their lines with the same `diag` functions as everything else and
//! hand them over here; the render task drains the queue between frames.
//!
//! Falling back to "drop the line" when the queue is full is deliberate: a
//! monitor that blocks on logging is worse than a monitor that is quiet
//! (`diag.rs` says the same thing at the top).

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use panel_core::link::{LinkError, Transport};

/// Longest line we are willing to queue: the shared limit, so a formatter can
/// never build a line this queue would have to cut.
pub use panel_core::link::LINE_MAX;

pub struct Line {
    bytes: [u8; LINE_MAX],
    len: u8,
}

impl Line {
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }
}

static LOG: Channel<CriticalSectionRawMutex, Line, 8> = Channel::new();

/// The producing end. Cheap to create: it is just another handle on the queue.
pub struct ChanLog;

impl ChanLog {
    pub fn new() -> Self {
        Self
    }
}

impl Transport for ChanLog {
    fn poll_line(&mut self, _out: &mut [u8]) -> Result<Option<usize>, LinkError> {
        // Diagnostics never accept input.
        Ok(None)
    }

    fn write_all(&mut self, bytes: &[u8]) -> Result<(), LinkError> {
        let mut line = Line {
            bytes: [0u8; LINE_MAX],
            len: 0,
        };
        // Over-long input loses its *tail*, never its `\n`. A line without a
        // terminator is not a short line, it is no line at all: the next one
        // glues onto it and the reader sees one corrupt record instead of two
        // truthful ones. The bench board printed exactly that --
        // `WIFI fail reason=Disconnected(DisconnectedStationInfo { ssid: "L`
        // followed by the AP line on the same row -- and the only fully
        // readable failure in the log was the one that fit.
        let over = bytes.len() > LINE_MAX;
        let n = if over { LINE_MAX - 1 } else { bytes.len() };
        line.bytes[..n].copy_from_slice(&bytes[..n]);
        if over {
            line.bytes[LINE_MAX - 1] = b'\n';
        }
        line.len = if over { LINE_MAX } else { n } as u8;
        // try_send, not send: never block the caller on the logger.
        let _ = LOG.try_send(line);
        Ok(())
    }

    fn is_up(&self) -> bool {
        true
    }
}

/// Drain whatever is queued into `out`. Returns how many lines were written.
pub fn drain_into(out: &mut dyn Transport) -> usize {
    let mut written = 0;
    while let Ok(line) = LOG.try_receive() {
        let _ = out.write_all(line.as_bytes());
        written += 1;
    }
    written
}
