use std::{
    io::{self, Error, Write},
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use crate::{
    event::{
        filter::CursorPositionFilter,
        internal::{self, InternalEvent},
    },
    terminal::{disable_raw_mode, enable_raw_mode, sys::is_raw_mode_enabled},
};

/// How many [`position`] calls are waiting for a reply.
///
/// A terminal answers `ESC[6n` once per request and in order, so a backend that
/// sends requests of its own — to discover the terminal size, say — has to know
/// not to claim a reply this function is waiting for.
static POSITION_REQUESTS_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// Whether [`position`] is currently waiting for a cursor position report.
#[cfg(any(target_os = "motor", test))]
pub(crate) fn position_request_in_flight() -> bool {
    POSITION_REQUESTS_IN_FLIGHT.load(Ordering::SeqCst) > 0
}

/// Counts one request against [`POSITION_REQUESTS_IN_FLIGHT`] for as long as it
/// is outstanding.
struct RequestInFlight;

impl RequestInFlight {
    fn new() -> Self {
        POSITION_REQUESTS_IN_FLIGHT.fetch_add(1, Ordering::SeqCst);
        Self
    }
}

impl Drop for RequestInFlight {
    fn drop(&mut self) {
        POSITION_REQUESTS_IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Returns the cursor position (column, row).
///
/// The top left cell is represented as `(0, 0)`.
///
/// On systems where the position is read back over ANSI, this function will
/// block and possibly time out while
/// [`crossterm::event::read`](crate::event::read) or [`crossterm::event::poll`](crate::event::poll) are being called.
pub fn position() -> io::Result<(u16, u16)> {
    if is_raw_mode_enabled() {
        read_position_raw()
    } else {
        read_position()
    }
}

fn read_position() -> io::Result<(u16, u16)> {
    enable_raw_mode()?;
    let pos = read_position_raw();
    disable_raw_mode()?;
    pos
}

fn read_position_raw() -> io::Result<(u16, u16)> {
    let _in_flight = RequestInFlight::new();

    // Discard any buffered cursor-position replies from earlier `ESC[6n` requests so the
    // position returned below corresponds to the fresh request we are about to send.
    // Poll with a zero timeout to drain only already-available events without blocking.
    while let Ok(true) = internal::poll(Some(Duration::ZERO), &CursorPositionFilter) {
        let _ = internal::read(&CursorPositionFilter);
    }

    // Use `ESC [ 6 n` to and retrieve the cursor position.
    let mut stdout = io::stdout();
    stdout.write_all(b"\x1B[6n")?;
    stdout.flush()?;

    loop {
        match internal::poll(Some(Duration::from_millis(2000)), &CursorPositionFilter) {
            Ok(true) => {
                if let Ok(InternalEvent::CursorPosition(x, y)) =
                    internal::read(&CursorPositionFilter)
                {
                    return Ok((x, y));
                }
            }
            Ok(false) => {
                return Err(Error::other(
                    "The cursor position could not be read within a normal duration",
                ));
            }
            Err(_) => {}
        }
    }
}

#[cfg(test)]
pub(crate) mod testing {
    /// Pretends a `position()` call is under way for as long as the returned
    /// value is alive, so that backends which arbitrate over it can be tested
    /// without a terminal.
    pub(crate) fn request_in_flight() -> impl Drop {
        super::RequestInFlight::new()
    }
}
