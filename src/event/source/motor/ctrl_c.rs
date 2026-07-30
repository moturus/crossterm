use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(target_os = "motor")]
use std::{io, sync::mpsc, thread};

struct PendingEvents(AtomicUsize);

impl PendingEvents {
    const fn new() -> Self {
        Self(AtomicUsize::new(0))
    }

    fn add(&self) {
        let mut count = self.0.load(Ordering::SeqCst);
        loop {
            let next = count
                .checked_add(1)
                .expect("crossterm Ctrl+C pending-event count exhausted");
            match self
                .0
                .compare_exchange_weak(count, next, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => return,
                Err(actual) => count = actual,
            }
        }
    }

    fn take(&self) -> bool {
        let mut count = self.0.load(Ordering::SeqCst);
        loop {
            let Some(next) = count.checked_sub(1) else {
                return false;
            };
            match self
                .0
                .compare_exchange_weak(count, next, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => return true,
                Err(actual) => count = actual,
            }
        }
    }
}

#[cfg(target_os = "motor")]
static PENDING_EVENTS: PendingEvents = PendingEvents::new();

#[cfg(target_os = "motor")]
pub(crate) fn enable_ctrl_c_events() -> io::Result<()> {
    let waker = crate::event::internal::motor_waker()?;
    let (setup_tx, setup_rx) = mpsc::sync_channel(0);

    thread::Builder::new()
        .name("crossterm-ctrl-c".into())
        .spawn(move || match moto_rt::process::ctrl_c_register_handler() {
            Ok(mut last) => {
                if setup_tx.send(Ok(())).is_err() {
                    return;
                }
                while let Ok(next) = moto_rt::process::ctrl_c_wait(last) {
                    for _ in last..next {
                        PENDING_EVENTS.add();
                        waker
                            .wake()
                            .expect("failed to wake crossterm's Motor event reader for Ctrl+C");
                    }
                    last = next;
                }
            }
            // A process without a terminal has no Ctrl+C source to register.
            Err(moto_rt::Error::NotFound) => {
                let _ = setup_tx.send(Ok(()));
            }
            Err(error) => {
                let _ = setup_tx.send(Err(motor_error(error)));
            }
        })?;

    setup_rx.recv().unwrap_or_else(|_| {
        Err(io::Error::other(
            "crossterm Ctrl+C listener stopped during setup",
        ))
    })
}

#[cfg(target_os = "motor")]
fn motor_error(error: moto_rt::Error) -> io::Error {
    io::Error::from_raw_os_error(moto_rt::ErrorCode::from(error).into())
}

#[cfg(target_os = "motor")]
pub(crate) fn take_ctrl_c_event() -> bool {
    PENDING_EVENTS.take()
}

#[cfg(test)]
mod tests {
    use super::PendingEvents;

    #[test]
    fn pending_events_are_counted_individually() {
        let pending = PendingEvents::new();

        pending.add();
        pending.add();
        pending.add();

        assert!(pending.take());
        assert!(pending.take());
        assert!(pending.take());
        assert!(!pending.take());
    }
}
