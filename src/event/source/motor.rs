//! Motor OS event source.

pub(crate) mod input;
pub(crate) mod probe;

mod ctrl_c;

#[cfg(target_os = "motor")]
pub(crate) use ctrl_c::enable_ctrl_c_events;
#[cfg(target_os = "motor")]
pub(crate) use ctrl_c::take_ctrl_c_event;

#[cfg(target_os = "motor")]
mod event_source;

#[cfg(target_os = "motor")]
pub(crate) use event_source::MotorInternalEventSource;
