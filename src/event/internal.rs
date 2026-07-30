use std::time::Duration;

use parking_lot::{MappedMutexGuard, Mutex, MutexGuard};

#[cfg(any(unix, target_os = "motor"))]
use crate::event::KeyboardEnhancementFlags;
use crate::event::{Event, filter::Filter, read::InternalEventReader, timeout::PollTimeout};

/// Static instance of `InternalEventReader`.
/// This needs to be static because there can be one event reader.
static EVENT_READER: Mutex<Option<InternalEventReader>> = parking_lot::const_mutex(None);

pub(crate) fn lock_event_reader() -> MappedMutexGuard<'static, InternalEventReader> {
    MutexGuard::map(EVENT_READER.lock(), |reader| {
        reader.get_or_insert_with(InternalEventReader::default)
    })
}

#[cfg(target_os = "motor")]
pub(crate) fn motor_waker() -> std::io::Result<crate::event::sys::Waker> {
    lock_event_reader().try_waker()
}

fn try_lock_event_reader_for(
    duration: Duration,
) -> Option<MappedMutexGuard<'static, InternalEventReader>> {
    Some(MutexGuard::map(
        EVENT_READER.try_lock_for(duration)?,
        |reader| reader.get_or_insert_with(InternalEventReader::default),
    ))
}

/// Polls to check if there are any `InternalEvent`s that can be read within the given duration.
pub(crate) fn poll<F>(timeout: Option<Duration>, filter: &F) -> std::io::Result<bool>
where
    F: Filter,
{
    let (mut reader, timeout) = if let Some(timeout) = timeout {
        let poll_timeout = PollTimeout::new(Some(timeout));
        let reader = match try_lock_event_reader_for(timeout) {
            Some(reader) => reader,
            None => return Ok(false),
        };
        (reader, poll_timeout.leftover())
    } else {
        (lock_event_reader(), None)
    };
    reader.poll(timeout, filter)
}

/// Reads a single `InternalEvent`.
pub(crate) fn read<F>(filter: &F) -> std::io::Result<InternalEvent>
where
    F: Filter,
{
    let mut reader = lock_event_reader();
    reader.read(filter)
}

/// Reads a single `InternalEvent`. Non-blocking.
pub(crate) fn try_read<F>(filter: &F) -> Option<InternalEvent>
where
    F: Filter,
{
    let mut reader = lock_event_reader();
    reader.try_read(filter)
}

/// An internal event.
///
/// Encapsulates publicly available `Event` with additional internal
/// events that shouldn't be publicly available to the crate users.
#[derive(Debug, PartialOrd, PartialEq, Hash, Clone, Eq)]
pub(crate) enum InternalEvent {
    /// An event.
    Event(Event),
    /// A cursor position (`col`, `row`).
    #[cfg(any(unix, target_os = "motor"))]
    CursorPosition(u16, u16),
    /// The progressive keyboard enhancement flags enabled by the terminal.
    #[cfg(any(unix, target_os = "motor"))]
    KeyboardEnhancementFlags(KeyboardEnhancementFlags),
    /// Attributes and architectural class of the terminal.
    #[cfg(any(unix, target_os = "motor"))]
    PrimaryDeviceAttributes,
    /// DEC private mode 2048's current state.
    #[cfg(any(target_os = "motor", test))]
    ResizeModeStatus(u8),
    /// A mode 2048 size report (`columns`, `rows`).
    #[cfg(any(target_os = "motor", test))]
    ResizeModeReport(u16, u16),
    /// The answer to `CSI 18 t`, the size query that leaves the cursor where
    /// the application put it (`columns`, `rows`).
    #[cfg(any(target_os = "motor", test))]
    TextAreaSize(u16, u16),
}
