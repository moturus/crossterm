//! This module provides platform related functions.

#[cfg(target_os = "motor")]
#[cfg(feature = "events")]
pub use self::motor::supports_keyboard_enhancement;
#[cfg(target_os = "motor")]
pub(crate) use self::motor::{
    disable_raw_mode, enable_raw_mode, is_raw_mode_enabled, size, window_size,
};
#[cfg(unix)]
#[cfg(feature = "events")]
pub use self::unix::supports_keyboard_enhancement;
#[cfg(unix)]
pub(crate) use self::unix::{
    disable_raw_mode, enable_raw_mode, is_raw_mode_enabled, size, window_size,
};
#[cfg(windows)]
#[cfg(feature = "events")]
pub use self::windows::supports_keyboard_enhancement;
#[cfg(all(windows, test))]
pub(crate) use self::windows::temp_screen_buffer;
#[cfg(windows)]
pub(crate) use self::windows::{
    clear, disable_raw_mode, enable_raw_mode, is_raw_mode_enabled, scroll_down, scroll_up,
    set_size, set_window_title, size, window_size,
};

#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub mod file_descriptor;
// The Motor OS terminal layer is portable Rust; compiling it under `cfg(test)`
// everywhere keeps its unit tests in the normal test run.
#[cfg(any(target_os = "motor", test))]
pub(crate) mod motor;
#[cfg(unix)]
mod unix;
