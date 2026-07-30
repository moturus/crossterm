#[cfg(all(unix, feature = "event-stream"))]
pub(crate) use unix::waker::Waker;
#[cfg(all(windows, feature = "event-stream"))]
pub(crate) use windows::waker::Waker;

// The ANSI escape sequence parser is OS-independent; it sits beside the per-OS
// modules rather than inside one of them.
#[cfg(all(unix, feature = "events"))]
pub(crate) mod parse;

#[cfg(unix)]
pub(crate) mod unix;
#[cfg(windows)]
pub(crate) mod windows;
