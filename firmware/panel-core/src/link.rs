//! The link contract: how a scene's bytes and a dispatcher's lines move.
//!
//! What this does NOT do: no sockets, no UART, no threads, no retries by
//! itself. It defines the shape every transport must have, plus the reconnect
//! ladder, so that swapping UART for TCP does not change a single caller.

/// Why a transport gave up. Not every layer cares, but they must all be able to
/// say "this failed" without panicking.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LinkError {
    /// Peer is gone / socket closed / cable pulled.
    Closed,
    /// Nothing arrived for a while, or a write timed out.
    Timeout,
    /// Hardware said no (driver returned an error).
    Io,
}

/// Backoff ladder shared by every transport and by the PC side (PROTOCOL §1).
/// Index is the number of consecutive failures; the last entry is the cap.
pub const BACKOFF_MS: [u32; 5] = [1_000, 2_000, 4_000, 8_000, 10_000];

pub fn backoff_ms(consecutive_failures: u32) -> u32 {
    let idx = consecutive_failures.saturating_sub(1) as usize;
    BACKOFF_MS[if idx < BACKOFF_MS.len() { idx } else { BACKOFF_MS.len() - 1 }]
}

/// One line at a time. `poll_line` must never block forever: returning
/// `Ok(None)` means "come back later", which is what keeps the render loop
/// predictable (ARCHITECTURE §4).
pub trait Transport {
    /// Copies one `\n`-terminated line (without the newline) into `out`.
    /// Returns the length, or `None` if no complete line is available yet.
    fn poll_line(&mut self, out: &mut [u8]) -> Result<Option<usize>, LinkError>;

    /// Writes everything or reports an error. Partial writes must be retried
    /// inside the implementation, not by the caller.
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), LinkError>;

    fn is_up(&self) -> bool;
}

#[cfg(test)]
mod tests {
    use super::backoff_ms;

    #[test]
    fn ladder_matches_the_pc_side() {
        assert_eq!(backoff_ms(0), 1_000);
        assert_eq!(backoff_ms(1), 1_000);
        assert_eq!(backoff_ms(2), 2_000);
        assert_eq!(backoff_ms(3), 4_000);
        assert_eq!(backoff_ms(4), 8_000);
        assert_eq!(backoff_ms(5), 10_000);
        assert_eq!(backoff_ms(60), 10_000);
    }
}
