//! Motor OS glue for the event reader.

use std::io;

/// Motor OS error codes are what `io::Error::from_raw_os_error` expects there,
/// so this yields an error with the right `ErrorKind` and message.
pub(crate) fn motor_error(error: moto_rt::Error) -> io::Error {
    io::Error::from_raw_os_error(moto_rt::ErrorCode::from(error).into())
}

pub(crate) use waker::Waker;

mod waker {
    use std::{io, sync::Arc};

    use moto_rt::poll;

    use super::motor_error;

    /// A poll registry, closed once the last holder lets go of it.
    #[derive(Debug)]
    struct Registry(moto_rt::RtFd);

    impl Drop for Registry {
        fn drop(&mut self) {
            let _ = moto_rt::fs::close(self.0);
        }
    }

    /// Allows to wake up the `EventSource::try_read()` method.
    ///
    /// `moto_rt::poll::wake` posts an event to the registries a registry is
    /// registered *in*, rather than to the registry itself, so waking a source
    /// takes a second registry that lives inside the source's own and holds no
    /// file descriptors of its own. The event is left in the outer registry
    /// whether or not anything is waiting on it at the time, so a wake that
    /// arrives before the wait does is not lost, and one wake produces exactly
    /// one event.
    ///
    /// No thread and no pipe, unlike the UNIX wakers: this is the whole of it.
    #[derive(Clone, Debug)]
    pub(crate) struct Waker {
        inner: Arc<Registry>,
    }

    impl Waker {
        /// Creates a `Waker` and the registry it wakes through.
        pub(crate) fn new() -> io::Result<Self> {
            let registry = poll::new().map_err(motor_error)?;

            Ok(Self {
                inner: Arc::new(Registry(registry)),
            })
        }

        /// The registry to register in the source's own, so that a wake is
        /// something the source can wait on.
        pub(crate) fn registry_fd(&self) -> moto_rt::RtFd {
            self.inner.0
        }

        /// Wakes whatever is waiting on the registry this was registered in.
        pub(crate) fn wake(&self) -> io::Result<()> {
            poll::wake(self.inner.0).map_err(motor_error)
        }
    }
}
