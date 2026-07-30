//! Motor OS related logic for terminal manipulation.
//!
//! Motor OS has no termios, no `ioctl`, no `/dev/tty` and no signals. A console
//! is always raw: a program reads input bytes exactly as they arrive and drives
//! the display entirely with ANSI escape sequences. Everything crossterm needs
//! from the platform therefore reduces to "is there a terminal on the other end
//! of stdio, and how big is it".

use std::io::{self, Write};

use parking_lot::Mutex;

use crate::terminal::WindowSize;

/// The size reported when nothing better is known. VT100's geometry, and the
/// same guess `tput` makes for an unknown terminal.
const FALLBACK_SIZE: (u16, u16) = (80, 24);

/// Whether the application has asked for raw mode.
///
/// Motor OS consoles are always raw, so this only records the request: it exists
/// so that `is_raw_mode_enabled` answers what the application expects, which is
/// what `cursor::position` and `supports_keyboard_enhancement` branch on.
static TERMINAL_STATE: Mutex<TerminalState> = parking_lot::const_mutex(TerminalState::new());

#[cfg(any(feature = "events", test))]
const RESIZE_MODE_QUERY: &[u8] = b"\x1b[?2048$p";
const RESIZE_MODE_ENABLE: &[u8] = b"\x1b[?2048h";
const RESIZE_MODE_DISABLE: &[u8] = b"\x1b[?2048l";

struct TerminalState {
    raw_mode: bool,
    resize_mode_started: bool,
    resize_mode_active: bool,
}

impl TerminalState {
    const fn new() -> Self {
        Self {
            raw_mode: false,
            resize_mode_started: false,
            resize_mode_active: false,
        }
    }

    #[cfg(any(feature = "events", test))]
    fn start_resize_mode(&mut self, output: &mut impl Write) -> io::Result<()> {
        if self.resize_mode_started {
            return Ok(());
        }
        output.write_all(RESIZE_MODE_QUERY)?;
        output.write_all(RESIZE_MODE_ENABLE)?;
        output.flush()?;
        self.resize_mode_started = true;
        self.resize_mode_active = true;
        Ok(())
    }

    fn set_raw_mode(&mut self, enabled: bool, output: &mut impl Write) -> io::Result<()> {
        if self.resize_mode_started && self.resize_mode_active != enabled {
            output.write_all(if enabled {
                RESIZE_MODE_ENABLE
            } else {
                RESIZE_MODE_DISABLE
            })?;
            output.flush()?;
            self.resize_mode_active = enabled;
        }
        self.raw_mode = enabled;
        Ok(())
    }
}

/// What the last valid terminal size report contained.
///
/// Only the event source owns stdin and can read either a mode 2048 report or a
/// fallback probe reply, so this is where it leaves the latest one.
static PROBED_SIZE: Mutex<Option<(u16, u16)>> = parking_lot::const_mutex(None);

pub(crate) fn is_raw_mode_enabled() -> bool {
    TERMINAL_STATE.lock().raw_mode
}

pub(crate) fn enable_raw_mode() -> io::Result<()> {
    TERMINAL_STATE.lock().set_raw_mode(true, &mut io::stdout())
}

pub(crate) fn disable_raw_mode() -> io::Result<()> {
    TERMINAL_STATE.lock().set_raw_mode(false, &mut io::stdout())
}

/// Starts in-band resize notifications when stdout is a terminal.
#[cfg(all(target_os = "motor", feature = "events"))]
pub(crate) fn start_resize_mode(is_terminal: bool) -> io::Result<()> {
    if is_terminal {
        TERMINAL_STATE.lock().start_resize_mode(&mut io::stdout())?;
    }
    Ok(())
}

/// The terminal size, in columns and rows. In precedence order:
///
/// 1. what the last valid in-band report or fallback probe reported;
/// 2. `COLUMNS` and `LINES`, which `rmux` sets for the programs it starts in a
///    pane, and which nothing sets over `ssh` or on the serial console;
/// 3. 80x24.
///
/// Motor OS has no `TIOCGWINSZ` or signals, so 1 is the only source that follows
/// a terminal whose size changes. It is filled while the application is inside
/// [`event::poll`](crate::event::poll) or [`event::read`](crate::event::read).
///
/// This function itself never blocks, never writes to the terminal and never
/// spawns a process: nothing should have to wait on a console that may never
/// answer before it can paint its first frame.
pub(crate) fn size() -> io::Result<(u16, u16)> {
    if let Some(size) = *PROBED_SIZE.lock() {
        return Ok(size);
    }

    Ok((
        env_size("COLUMNS").unwrap_or(FALLBACK_SIZE.0),
        env_size("LINES").unwrap_or(FALLBACK_SIZE.1),
    ))
}

/// Records a validated size report, and says whether it is news.
#[cfg(feature = "events")]
pub(crate) fn set_probed_size(columns: u16, rows: u16) -> bool {
    let mut reported = PROBED_SIZE.lock();
    if *reported == Some((columns, rows)) {
        return false;
    }

    *reported = Some((columns, rows));
    true
}

/// Puts the size cache back to how a program starts: nothing reported yet.
#[cfg(test)]
pub(crate) fn forget_probed_size() {
    *PROBED_SIZE.lock() = None;
}

/// Motor OS reports no pixel geometry, so `width` and `height` are 0. The
/// documented contract for [`WindowSize`] already allows that.
pub(crate) fn window_size() -> io::Result<WindowSize> {
    let (columns, rows) = size()?;
    Ok(WindowSize {
        rows,
        columns,
        width: 0,
        height: 0,
    })
}

fn env_size(name: &str) -> Option<u16> {
    std::env::var(name).ok()?.parse().ok().filter(|&n| n > 0)
}

/// Motor OS never reports support for the progressive keyboard enhancement
/// protocol.
///
/// The standard detection sends `ESC[?u` followed by `ESC[c` and concludes "not
/// supported" from a primary-device-attributes reply that arrives without a
/// flags reply. Nothing on Motor OS answers `ESC[c`: `rmux` deliberately answers
/// `ESC[6n` and nothing else, so the honest query would stall every application
/// for its full timeout and then report `false` anyway.
#[cfg(feature = "events")]
pub fn supports_keyboard_enhancement() -> io::Result<bool> {
    Ok(false)
}

#[cfg(test)]
mod tests {
    use serial_test::serial;

    use super::*;

    // Both statics are process-wide, so these tests set what they depend on and
    // do not run alongside each other.

    #[test]
    #[serial]
    fn test_raw_mode_is_tracked() {
        assert!(!is_raw_mode_enabled());
        enable_raw_mode().unwrap();
        assert!(is_raw_mode_enabled());
        disable_raw_mode().unwrap();
        assert!(!is_raw_mode_enabled());
    }

    #[test]
    fn test_resize_mode_lifecycle() {
        let mut state = TerminalState::new();
        let mut output = Vec::new();

        state.start_resize_mode(&mut output).unwrap();
        state.start_resize_mode(&mut output).unwrap();
        assert_eq!(output, b"\x1b[?2048$p\x1b[?2048h");

        output.clear();
        state.set_raw_mode(false, &mut output).unwrap();
        state.set_raw_mode(false, &mut output).unwrap();
        assert_eq!(output, RESIZE_MODE_DISABLE);

        output.clear();
        state.set_raw_mode(true, &mut output).unwrap();
        assert_eq!(output, RESIZE_MODE_ENABLE);
    }

    #[test]
    #[serial]
    fn test_size_falls_back_to_80x24() {
        forget_probed_size();
        temp_env::with_vars([("COLUMNS", None::<&str>), ("LINES", None)], || {
            assert_eq!(size().unwrap(), (80, 24));

            let window = window_size().unwrap();
            assert_eq!((window.columns, window.rows), (80, 24));
            // Motor OS reports no pixel geometry.
            assert_eq!((window.width, window.height), (0, 0));
        });
    }

    #[test]
    #[serial]
    fn test_size_uses_the_environment() {
        forget_probed_size();
        temp_env::with_vars([("COLUMNS", Some("120")), ("LINES", Some("40"))], || {
            assert_eq!(size().unwrap(), (120, 40));
        });
        // Each is taken on its own, and nonsense is ignored.
        temp_env::with_vars([("COLUMNS", Some("120")), ("LINES", None)], || {
            assert_eq!(size().unwrap(), (120, 24));
        });
        temp_env::with_vars(
            [("COLUMNS", Some("0")), ("LINES", Some("not a number"))],
            || {
                assert_eq!(size().unwrap(), (80, 24));
            },
        );
    }

    #[cfg(feature = "events")]
    #[test]
    #[serial]
    fn test_a_probed_size_outranks_the_environment() {
        forget_probed_size();
        temp_env::with_vars([("COLUMNS", Some("120")), ("LINES", Some("40"))], || {
            assert!(set_probed_size(121, 41));
            assert_eq!(size().unwrap(), (121, 41));

            // The same answer again is not news; a different one is.
            assert!(!set_probed_size(121, 41));
            assert!(set_probed_size(80, 24));
            assert_eq!(size().unwrap(), (80, 24));
        });
        forget_probed_size();
    }

    #[cfg(feature = "events")]
    #[test]
    #[serial]
    fn test_keyboard_enhancement_is_never_supported() {
        assert!(!supports_keyboard_enhancement().unwrap());
    }
}
