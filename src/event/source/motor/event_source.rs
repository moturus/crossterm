//! Motor OS event source.
//!
//! Motor OS has no `/dev/tty`: a program's terminal, when it has one, is its own
//! stdin, and `moto_rt::poll` is the only way to ask whether a byte is waiting
//! there. There is nothing else to wait on — the platform has no signals, so no
//! `SIGWINCH` self-pipe — which makes this source a single registry holding a
//! single file descriptor.

use std::{
    io::{self, IsTerminal},
    time::{Duration, Instant},
};

use moto_rt::poll;

use crate::event::sys::Waker;
use crate::event::{
    Event, KeyCode, KeyEvent, KeyModifiers,
    internal::InternalEvent,
    source::{
        EventSource,
        motor::{
            input::CrLfCoalescer,
            probe::{Reply, SizeProbe},
        },
    },
    sys::{motor::motor_error, parse::Parser},
    timeout::PollTimeout,
};

/// Registry token for stdin.
const STDIN_TOKEN: poll::Token = 0;
/// Registry token for the waker's registry.
const WAKE_TOKEN: poll::Token = 1;

/// The readiness that says the far end of stdin is gone. Neither can be asked
/// for — only `POLL_READABLE` may be registered — but both are delivered.
const HANGUP: poll::EventBits = poll::POLL_READ_CLOSED | poll::POLL_ERROR;

/// How long a half-arrived escape sequence is held before it is taken to be as
/// finished as it will ever be.
///
/// The serial console delivers input one byte per read, so `ESC [ A` arrives as
/// three of them. A decoder that never waited would report every arrow key as
/// `Esc` `[` `A`; one that never gave up would swallow the Escape key. `tmux`
/// spends its `escape-time` on exactly this, and Motor OS's own terminal
/// multiplexer settled on the same 50 ms.
const ESCAPE_TIME: Duration = Duration::from_millis(50);

/// A Motor OS stdio pipe carries at most half of a 4 KiB page, so one read of
/// this size takes everything the pipe holds. The parser therefore always sees a
/// burst whole, and no inner drain loop is needed.
const READ_BUFFER_SIZE: usize = 2048;

pub(crate) struct MotorInternalEventSource {
    registry: moto_rt::RtFd,
    input_fd: moto_rt::RtFd,
    parser: Parser,
    coalescer: CrLfCoalescer,
    probe: SizeProbe,
    buffer: [u8; READ_BUFFER_SIZE],
    /// When the parser first had a half-arrived sequence in hand, so that
    /// [`ESCAPE_TIME`] is measured against a clock rather than against however
    /// many times a wait happened to return.
    holding_since: Option<Instant>,
    waker: Waker,
}

impl MotorInternalEventSource {
    pub fn new() -> io::Result<Self> {
        let registry = poll::new().map_err(motor_error)?;

        Self::with_registry(registry).inspect_err(|_| {
            let _ = moto_rt::fs::close(registry);
        })
    }

    fn with_registry(registry: moto_rt::RtFd) -> io::Result<Self> {
        let input_fd = if moto_rt::fs::is_terminal(moto_rt::FD_TERMINAL) {
            moto_rt::FD_TERMINAL
        } else {
            moto_rt::FD_STDIN
        };
        // Only `POLL_READABLE` may be registered; hangup and error readiness are
        // reported without being asked for.
        poll::add(registry, input_fd, STDIN_TOKEN, poll::POLL_READABLE).map_err(motor_error)?;

        let waker = Waker::new()?;
        poll::add(
            registry,
            waker.registry_fd(),
            WAKE_TOKEN,
            poll::POLL_READABLE,
        )
        .map_err(motor_error)?;

        let is_terminal = io::stdout().is_terminal();
        crate::terminal::sys::motor::start_resize_mode(is_terminal)?;

        Ok(Self {
            registry,
            input_fd,
            parser: Parser::default(),
            coalescer: CrLfCoalescer::default(),
            // Asking a terminal how big it is means writing to it, which is only
            // worth doing when there is one there.
            probe: SizeProbe::new(is_terminal),
            buffer: [0u8; READ_BUFFER_SIZE],
            holding_since: None,
            waker,
        })
    }

    /// How much longer the half-arrived sequence in the parser may be held, or
    /// `None` if the parser is not holding one.
    fn escape_time_left(&self) -> Option<Duration> {
        self.holding_since
            .map(|since| (since + ESCAPE_TIME).saturating_duration_since(Instant::now()))
    }

    /// Waits for readiness and returns what turned up, or `None` if the deadline
    /// passed first.
    ///
    /// `leftover` is `None` for "block indefinitely" and `Some(Duration::ZERO)`
    /// for "look, don't wait"; `moto_rt::poll::wait` honors both exactly.
    fn wait(&self, leftover: Option<Duration>) -> io::Result<Option<poll::Event>> {
        let deadline = leftover.map(|left| moto_rt::time::Instant::now() + left);
        let mut event = poll::Event::default();

        match poll::wait(self.registry, &mut event, 1, deadline).map_err(motor_error)? {
            0 => Ok(None),
            _ => Ok(Some(event)),
        }
    }

    /// Reads one burst of input and hands it to the parser.
    fn fill_parser(&mut self) -> io::Result<()> {
        let read_count = moto_rt::fs::read(self.input_fd, &mut self.buffer).map_err(motor_error)?;
        if read_count == 0 {
            return Err(hangup_error());
        }

        let kept = self.coalescer.retain(&mut self.buffer[..read_count]);
        // Always "more to come": here a burst that ends mid-sequence says
        // nothing about whether the rest is on its way, so the escape timer
        // decides when a sequence has waited long enough instead.
        self.parser.advance(&self.buffer[..kept], true);

        if !self.parser.is_mid_sequence() {
            self.holding_since = None;
        } else if self.holding_since.is_none() {
            self.holding_since = Some(Instant::now());
        }

        Ok(())
    }
}

impl Drop for MotorInternalEventSource {
    fn drop(&mut self) {
        let _ = moto_rt::fs::close(self.registry);
    }
}

impl EventSource for MotorInternalEventSource {
    fn try_read(&mut self, timeout: Option<Duration>) -> io::Result<Option<InternalEvent>> {
        let timeout = PollTimeout::new(timeout);

        loop {
            if let Some(event) = self.parser.next() {
                match event {
                    InternalEvent::ResizeModeStatus(status) => {
                        self.probe.resize_mode_status(status);
                        continue;
                    }
                    InternalEvent::ResizeModeReport(columns, rows) => {
                        if self.probe.resize_mode_report(columns, rows) {
                            return Ok(Some(InternalEvent::Event(Event::Resize(columns, rows))));
                        }
                        continue;
                    }
                    InternalEvent::TextAreaSize(columns, rows) => {
                        if self.probe.text_area_reply(columns, rows) {
                            return Ok(Some(InternalEvent::Event(Event::Resize(columns, rows))));
                        }
                        continue;
                    }
                    InternalEvent::CursorPosition(column, row) => {
                        // A cursor position report may be the answer to the size
                        // probe this source sent rather than to anything the
                        // application asked for. There is one stdin, so somebody
                        // has to arbitrate.
                        match self.probe.reply(column, row) {
                            Reply::NotMine => {
                                return Ok(Some(InternalEvent::CursorPosition(column, row)));
                            }
                            Reply::SameSize => continue,
                            Reply::Resized(columns, rows) => {
                                return Ok(Some(InternalEvent::Event(Event::Resize(
                                    columns, rows,
                                ))));
                            }
                        }
                    }
                    event => return Ok(Some(event)),
                }
            }

            self.probe.probe();

            if self.escape_time_left().is_some_and(|left| left.is_zero()) {
                // Nothing followed within `ESCAPE_TIME`: that `ESC` was the
                // Escape key, or that sequence is never going to finish.
                self.parser.flush();
                self.holding_since = None;
                continue;
            }

            // A wait can end on any of three deadlines; only the caller's ends
            // the call. The probe's is what makes a terminal that changed shape
            // while nothing was being typed noticeable at all: without it a wait
            // for input blocks until there is some, and the next probe is only
            // ever sent on the way into the wait after that.
            let deadline = shorter(
                timeout.leftover(),
                shorter(
                    self.escape_time_left(),
                    self.probe.next_probe_in(Instant::now()),
                ),
            );
            let Some(ready) = self.wait(deadline)? else {
                if timeout.elapsed() {
                    return Ok(None);
                }
                continue;
            };

            if ready.token == WAKE_TOKEN {
                if super::take_ctrl_c_event() {
                    return Ok(Some(InternalEvent::Event(Event::Key(KeyEvent::new(
                        KeyCode::Char('c'),
                        KeyModifiers::CONTROL,
                    )))));
                }

                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "Poll operation was woken up by `Waker::wake`",
                ));
            }

            // A hangup does not mean there is nothing left to read: a peer that
            // writes and closes in one breath leaves its last bytes in the pipe,
            // and this readiness arrives with no `POLL_READABLE` beside it. So
            // read on any of these, and let a read of nothing be the end.
            if ready.events & (poll::POLL_READABLE | HANGUP) == 0 {
                continue;
            }

            self.fill_parser()?;
        }
    }

    fn waker(&self) -> Waker {
        self.waker.clone()
    }
}

/// The nearer of two optional deadlines, where `None` means "no deadline".
fn shorter(left: Option<Duration>, right: Option<Duration>) -> Option<Duration> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (left, None) => left,
        (None, right) => right,
    }
}

fn hangup_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "the terminal on the other end of stdin is gone",
    )
}
