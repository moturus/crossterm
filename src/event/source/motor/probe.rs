//! Fallback polling for a Motor OS terminal's size.
//!
//! Mode 2048 supplies push notifications when the terminal supports it. Until
//! support is confirmed, the event source polls, asking less often once nothing
//! appears to be listening.
//!
//! There are two ways to ask, and the order matters to whoever is watching the
//! screen. `CSI 18 t` is answered without the cursor going anywhere, so it is
//! tried first. The fallback parks the cursor in the bottom right corner and
//! asks where it ended up, which *is* visible: on a slow terminal — a nested
//! VM, a serial line — the cursor is seen to jump to the corner and back on
//! every probe. Hiding it around the query is not an option, because nothing
//! here knows whether the application wants a cursor at all: `Hide` and `Show`
//! are stateless commands, and putting the cursor back would turn one on for a
//! full-screen program that had deliberately turned it off.

use std::time::{Duration, Instant};

/// How often the terminal is asked.
///
/// This is the fallback for a terminal that does not implement mode 2048, and
/// only that: one that does is never asked at all. What is left is a console in
/// front of an emulator that cannot push, where the event being polled for is
/// somebody dragging a window — so the cost is paid continuously and the news
/// arrives a few times an hour at most. Ten seconds bounds how long a resize
/// goes unnoticed there at a tenth of what a one-second cadence cost.
const PROBE_INTERVAL: Duration = Duration::from_secs(10);

/// How often it is asked once it has stopped answering.
///
/// A terminal descriptor is not a promise that anything will answer it. Motor
/// OS derives the bit per descriptor and propagates it through spawn, so a
/// program whose stdout was redirected to a file still has a terminal stdin,
/// and an `ssh` command run without a pty has none of the three but may still
/// be asked by a program that checks only one. Asking those on the normal
/// cadence would put 22 bytes of unanswerable question into the output for the
/// life of the program.
const QUIET_INTERVAL: Duration = Duration::from_secs(30);

/// How many probes may go unanswered before [`QUIET_INTERVAL`] takes over.
///
/// More than one, because a console that is merely *slow* — a serial line
/// handing over a reply one byte at a time, a terminal on the far side of a
/// network — must not be mistaken for one that cannot answer at all. Any answer
/// puts the probe straight back on [`PROBE_INTERVAL`].
const QUIET_AFTER: u32 = 3;

/// How many `CSI 18 t` queries go unanswered before the cursor probe takes over.
///
/// A terminal that does not implement the window operations says nothing at
/// all, so the only evidence is silence, and silence takes a timeout to
/// establish. Two, for the same reason [`QUIET_AFTER`] is more than one.
const TEXT_AREA_ATTEMPTS: u32 = 2;

/// How long to wait before giving up on an unanswered `CSI 18 t`.
///
/// Much shorter than [`PROBE_INTERVAL`], because until something answers, the
/// application is painting at whatever `COLUMNS`/`LINES` or the 80x24 fallback
/// said — and on the physical console nothing sets those. Escalation is a
/// startup cost, paid once, and it should not be measured in tens of seconds.
const ESCALATION_INTERVAL: Duration = Duration::from_millis(250);

/// What a cursor position report turned out to be.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Reply {
    /// Not this probe's reply; the caller must pass the report on.
    NotMine,
    /// This probe's reply, and the terminal is the size it already was.
    SameSize,
    /// This probe's reply, and the terminal has a new size.
    Resized(u16, u16),
}

#[derive(Debug)]
pub(crate) struct SizeProbe {
    /// Whether there is a terminal on stdout to ask at all.
    enabled: bool,
    /// When the last probe went out.
    sent: Option<Instant>,
    /// Whether that probe's reply is still owed.
    awaiting_reply: bool,
    /// How many cursor probes in a row have gone unanswered ([`QUIET_AFTER`]).
    unanswered: u32,
    /// Whether mode 2048 has made the polling fallback unnecessary.
    resize_mode_supported: bool,
    /// Whether `CSI 18 t` has ever been answered. Once it has, the cursor is
    /// never moved to measure the terminal again.
    text_area_supported: bool,
    /// How many `CSI 18 t` queries have gone unanswered
    /// ([`TEXT_AREA_ATTEMPTS`]).
    text_area_unanswered: u32,
}

impl SizeProbe {
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            sent: None,
            awaiting_reply: false,
            unanswered: 0,
            resize_mode_supported: false,
            text_area_supported: false,
            text_area_unanswered: 0,
        }
    }

    /// Whether the next question is the one that leaves the cursor alone.
    fn asks_text_area(&self) -> bool {
        self.text_area_supported || self.text_area_unanswered < TEXT_AREA_ATTEMPTS
    }

    /// Whether the terminal has been asked something and has answered nothing,
    /// so how to ask it is still being worked out.
    ///
    /// This covers the step *after* the last `CSI 18 t` as well as the ones
    /// between: giving up on the cursor-free question and then waiting a full
    /// [`PROBE_INTERVAL`] to try the other one would put the whole escalation
    /// budget back into the startup path it exists to keep short.
    fn escalating(&self) -> bool {
        !self.text_area_supported && self.text_area_unanswered > 0 && self.unanswered == 0
    }

    /// How long between probes, given how the terminal has been answering.
    fn interval(&self) -> Duration {
        if self.escalating() {
            ESCALATION_INTERVAL
        } else if self.unanswered >= QUIET_AFTER {
            QUIET_INTERVAL
        } else {
            PROBE_INTERVAL
        }
    }

    /// Whether it is time to ask again.
    fn due(&self, now: Instant) -> bool {
        if self.resize_mode_supported {
            return false;
        }
        match (self.enabled, self.sent) {
            (false, _) => false,
            // Never asked: one probe is worth it whatever is out there.
            (true, None) => true,
            (true, Some(sent)) => now.duration_since(sent) >= self.interval(),
        }
    }

    /// How long until the next probe is due, or `None` if none ever will be.
    ///
    /// The event source waits no longer than this, so that a terminal that
    /// changed shape while nothing was being typed is still noticed. Waiting on
    /// input alone would mean a window could be dragged to twice its size and
    /// the application would go on painting the old one until the next
    /// keystroke.
    pub(crate) fn next_probe_in(&self, now: Instant) -> Option<Duration> {
        if !self.enabled || self.resize_mode_supported {
            return None;
        }

        Some(match self.sent {
            None => Duration::ZERO,
            Some(sent) => (sent + self.interval()).saturating_duration_since(now),
        })
    }

    /// Decides whether to ask now, and records that it did; the caller does the
    /// writing. Split out from [`probe`](Self::probe) so that the decision is
    /// testable on a host that must not be written escape sequences.
    fn take_turn(&mut self, now: Instant) -> bool {
        if !self.due(now) {
            return false;
        }

        self.sent = Some(now);

        // `CSI 18 t` moves nothing and is answered with a report no application
        // asks for, so there is nobody to collide with and nothing to arbitrate.
        if self.asks_text_area() {
            self.text_area_unanswered = self.text_area_unanswered.saturating_add(1);
            return true;
        }

        // The cursor probe parks the cursor in the bottom right corner on its
        // way past; asking for that while the application is waiting to be told
        // where its own cursor is would answer its question with the corner.
        // Standing down still counts as this interval's turn: a probe that
        // stayed due would spin the event loop for as long as the application
        // waited.
        if crate::cursor::sys::ansi::position_request_in_flight() {
            self.awaiting_reply = false;
            return false;
        }

        self.awaiting_reply = true;
        self.unanswered = self.unanswered.saturating_add(1);
        true
    }

    /// Whether this probe may still claim a cursor position report.
    ///
    /// There is no clock on this on purpose. A reply is owed from the moment the
    /// question goes out until the next question replaces it, however long the
    /// terminal takes: a window measured in milliseconds is a window a serial
    /// console handing the reply over one byte at a time can miss, and a missed
    /// reply used to mean the size was never learned at all.
    fn awaits_reply(&self) -> bool {
        // A terminal answers `ESC[6n` once per request and in order, so while
        // the application has a request of its own outstanding the next report
        // is far more likely to be the one it is waiting for.
        self.awaiting_reply && !crate::cursor::sys::ansi::position_request_in_flight()
    }

    /// Offers a cursor position report, as the parser produces it: zero-based
    /// column and row.
    pub(crate) fn reply(&mut self, column: u16, row: u16) -> Reply {
        if !self.awaits_reply() {
            return Reply::NotMine;
        }

        self.awaiting_reply = false;
        self.unanswered = 0;

        // The cursor was asked to go far past the last cell and was clamped to
        // it, so where it ended up, counted from one, is the size.
        let (columns, rows) = (column.saturating_add(1), row.saturating_add(1));
        if crate::terminal::sys::motor::set_probed_size(columns, rows) {
            Reply::Resized(columns, rows)
        } else {
            Reply::SameSize
        }
    }

    /// Records the answer to `CSI 18 t`, and stops the cursor ever being moved
    /// to measure the terminal again.
    ///
    /// Nothing arbitrates this the way [`reply`](Self::reply) has to: no
    /// application asks a window operation, so a report that arrives is this
    /// probe's by construction. Any cursor probe still outstanding is
    /// abandoned, or its answer would be claimed from the application later.
    pub(crate) fn text_area_reply(&mut self, columns: u16, rows: u16) -> bool {
        self.text_area_supported = true;
        self.text_area_unanswered = 0;
        self.unanswered = 0;
        self.awaiting_reply = false;
        crate::terminal::sys::motor::set_probed_size(columns, rows)
    }

    /// Applies a DECRPM reply. States 1--3 confirm support; 0 and 4 do not.
    pub(crate) fn resize_mode_status(&mut self, status: u8) {
        if matches!(status, 1..=3) {
            self.confirm_resize_mode();
        }
    }

    /// Records a validated in-band resize report and stops fallback probing.
    pub(crate) fn resize_mode_report(&mut self, columns: u16, rows: u16) -> bool {
        self.confirm_resize_mode();
        crate::terminal::sys::motor::set_probed_size(columns, rows)
    }

    fn confirm_resize_mode(&mut self) {
        self.resize_mode_supported = true;
        self.awaiting_reply = false;
    }

    /// Asks the terminal for its size, if it is time to ask.
    ///
    /// Compiled for Motor OS only. The host build of this module exists to run
    /// the tests below, and writing escape sequences to whatever terminal is
    /// running them would be rude.
    #[cfg(target_os = "motor")]
    pub(crate) fn probe(&mut self) {
        use std::io::Write;

        /// Ask for the size of the text area. The cursor is not part of the
        /// question, so nothing on screen moves and nothing has to be put back.
        const TEXT_AREA_PROBE: &[u8] = b"\x1b[18t";

        /// Save the cursor, ask to move it far past the bottom right corner,
        /// ask where it ended up, put it back. The move is clamped to the last
        /// cell, so the reply *is* the size. DECSC/DECRC mean the cursor ends
        /// where it started, but it is *seen* in the corner in between on a
        /// terminal slow enough to paint the intermediate state.
        const CURSOR_PROBE: &[u8] = b"\x1b7\x1b[9999;9999H\x1b[6n\x1b8";

        // Sampled before the turn is taken, which is what spends the attempt.
        let text_area = self.asks_text_area();
        if !self.take_turn(Instant::now()) {
            return;
        }

        let mut stdout = std::io::stdout();
        if stdout
            .write_all(if text_area {
                TEXT_AREA_PROBE
            } else {
                CURSOR_PROBE
            })
            .and_then(|()| stdout.flush())
            .is_err()
        {
            // Nothing went out, so nothing is owed.
            self.awaiting_reply = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use serial_test::serial;

    use super::*;

    /// A probe that has just sent a *cursor* probe and is waiting for the
    /// reply. Assembled rather than taken a turn, so that it is also a probe
    /// under a test that holds a cursor request in flight.
    ///
    /// Reaching the cursor probe at all means `CSI 18 t` was tried and never
    /// answered, so the text-area attempts are spent.
    fn awaiting(now: Instant) -> SizeProbe {
        SizeProbe {
            enabled: true,
            sent: Some(now),
            awaiting_reply: true,
            unanswered: 1,
            resize_mode_supported: false,
            text_area_supported: false,
            text_area_unanswered: TEXT_AREA_ATTEMPTS,
        }
    }

    /// A probe that has given up on `CSI 18 t` but sent nothing yet.
    fn cursor_style() -> SizeProbe {
        let mut probe = SizeProbe::new(true);
        probe.text_area_unanswered = TEXT_AREA_ATTEMPTS;
        probe
    }

    use crate::terminal::sys::motor::forget_probed_size;

    #[test]
    #[serial]
    fn test_a_probe_is_due_once_and_then_on_the_interval() {
        let start = Instant::now();
        let mut probe = awaiting(start);

        assert!(!probe.due(start + PROBE_INTERVAL / 2));
        assert!(probe.due(start + PROBE_INTERVAL));

        // An answer keeps it there.
        forget_probed_size();
        probe.reply(79, 23);
        assert!(probe.take_turn(start + PROBE_INTERVAL));
        assert!(probe.due(start + PROBE_INTERVAL * 2));
        forget_probed_size();
    }

    #[test]
    #[serial]
    fn test_the_cursor_is_left_alone_until_the_terminal_will_not_say() {
        // The cursor probe is *seen* on a slow terminal -- it jumps to the
        // corner and back -- so `CSI 18 t`, which moves nothing, is asked
        // first and the corner is a last resort.
        let start = Instant::now();
        let mut probe = SizeProbe::new(true);
        assert!(
            probe.asks_text_area(),
            "the first question moves the cursor"
        );

        // Silence is the only evidence, so it takes a few attempts, and they
        // come quickly: nothing paints at the right size until one lands.
        for attempt in 0..TEXT_AREA_ATTEMPTS {
            let at = start + ESCALATION_INTERVAL * attempt;
            assert!(probe.take_turn(at));
            assert!(
                probe.next_probe_in(at).unwrap() <= ESCALATION_INTERVAL,
                "escalation waited a whole probe interval"
            );
        }
        assert!(
            !probe.asks_text_area(),
            "a terminal that never answered is still not asked the other way"
        );

        // The cursor probe that follows comes just as quickly -- the point of
        // giving up is to get an answer, not to wait ten seconds for one.
        let gave_up = start + ESCALATION_INTERVAL * TEXT_AREA_ATTEMPTS;
        assert!(probe.take_turn(gave_up));
        // And once something has been asked the visible way, the cadence is
        // the ordinary one again.
        assert_eq!(probe.next_probe_in(gave_up), Some(PROBE_INTERVAL));
    }

    #[test]
    #[serial]
    fn test_an_answered_text_area_query_retires_the_cursor_probe() {
        forget_probed_size();
        let now = Instant::now();
        let mut probe = SizeProbe::new(true);
        assert!(probe.take_turn(now));

        assert!(probe.text_area_reply(80, 24));
        assert!(
            probe.asks_text_area(),
            "the cursor was moved after an answer"
        );
        // The same size next time is not news, and polling continues either
        // way: `CSI 18 t` is a question, not a subscription.
        assert!(!probe.text_area_reply(80, 24));
        assert!(probe.next_probe_in(now).is_some());
        forget_probed_size();
    }

    #[test]
    #[serial]
    fn test_a_text_area_answer_gives_up_any_claim_on_a_cursor_report() {
        // A cursor probe may be outstanding when the terminal finally answers
        // the other question. Its claim has to be dropped, or the next report
        // -- which by then can only be the application's own -- is taken from
        // it, which is the mistake `position_request_in_flight` exists to
        // prevent.
        forget_probed_size();
        let now = Instant::now();
        let mut probe = awaiting(now);
        probe.text_area_reply(80, 24);
        assert_eq!(probe.reply(120, 40), Reply::NotMine);
        forget_probed_size();
    }

    #[test]
    #[serial]
    fn test_the_console_is_not_asked_every_second() {
        // Every other test here is written in terms of `PROBE_INTERVAL`, so
        // none of them would notice it going back to a second. This one is
        // about the cadence itself. The probe polls for somebody dragging a
        // window, and mode 2048 has taken the job wherever the terminal can
        // push at all -- so a cadence in the low seconds buys latency nobody
        // can perceive and charges every program's output for it, for as long
        // as the program runs.
        assert!(PROBE_INTERVAL >= Duration::from_secs(10));

        let start = Instant::now();
        let mut probe = cursor_style();
        assert!(probe.take_turn(start));
        assert!(!probe.due(start + Duration::from_secs(1)));
        assert!(probe.due(start + PROBE_INTERVAL));
    }

    #[test]
    #[serial]
    fn test_a_console_that_stops_answering_is_asked_less_often() {
        let start = Instant::now();
        let mut probe = cursor_style();

        // A slow console is still a console: nothing gives up before
        // `QUIET_AFTER` questions have gone unanswered.
        for turn in 0..QUIET_AFTER {
            assert!(probe.take_turn(start + PROBE_INTERVAL * turn));
        }
        let quiet_from = start + PROBE_INTERVAL * (QUIET_AFTER - 1);
        assert!(!probe.due(quiet_from + PROBE_INTERVAL));
        assert!(probe.due(quiet_from + QUIET_INTERVAL));

        // One answer, however late, and it is a working console again.
        forget_probed_size();
        probe.reply(79, 23);
        assert!(probe.due(quiet_from + PROBE_INTERVAL));
        forget_probed_size();
    }

    #[test]
    fn test_a_probe_is_never_due_without_a_terminal() {
        let probe = SizeProbe::new(false);
        assert!(!probe.due(Instant::now()));
        assert!(!probe.due(Instant::now() + PROBE_INTERVAL * 10));
        // And nothing waits on a probe that will never be sent.
        assert_eq!(probe.next_probe_in(Instant::now()), None);
    }

    #[test]
    #[serial]
    fn test_a_wait_is_capped_at_the_next_probe() {
        let start = Instant::now();
        let mut probe = cursor_style();

        // Never asked: ask before waiting on anything.
        assert_eq!(probe.next_probe_in(start), Some(Duration::ZERO));

        assert!(probe.take_turn(start));
        assert_eq!(
            probe.next_probe_in(start + PROBE_INTERVAL / 2),
            Some(PROBE_INTERVAL / 2)
        );
        // Overdue is zero, not a negative wait.
        assert_eq!(
            probe.next_probe_in(start + PROBE_INTERVAL * 2),
            Some(Duration::ZERO)
        );
    }

    #[test]
    #[serial]
    fn test_the_probe_stands_down_while_the_application_asks() {
        let now = Instant::now();
        let mut probe = cursor_style();
        assert!(probe.due(now));

        let _in_flight = crate::cursor::sys::ansi::testing::request_in_flight();
        // Neither ask...
        assert!(!probe.take_turn(now));
        // ...nor claim the answer to somebody else's question.
        assert_eq!(awaiting(now).reply(120, 40), Reply::NotMine);
        // Standing down still spends the turn, so the event source waits on the
        // application rather than spinning on a probe that stays due. It comes
        // back on the escalation cadence rather than the full one: no size has
        // been learned yet, and the application's own request will not be in
        // flight for long.
        assert!(!probe.due(now));
        assert_eq!(probe.next_probe_in(now), Some(ESCALATION_INTERVAL));

        // Once a probe has actually gone out, the ordinary cadence applies.
        drop(_in_flight);
        assert!(probe.take_turn(now + ESCALATION_INTERVAL));
        assert_eq!(
            probe.next_probe_in(now + ESCALATION_INTERVAL),
            Some(PROBE_INTERVAL)
        );
    }

    #[test]
    #[serial]
    fn test_a_reply_is_the_size_the_cursor_was_clamped_to() {
        forget_probed_size();
        let now = Instant::now();

        // `ESC[41;121R` reaches us as the zero-based (120, 40).
        assert_eq!(awaiting(now).reply(120, 40), Reply::Resized(121, 41));
        // Same answer next time round: nothing to tell the application.
        assert_eq!(awaiting(now).reply(120, 40), Reply::SameSize);
        assert_eq!(awaiting(now).reply(79, 23), Reply::Resized(80, 24));
        forget_probed_size();
    }

    #[test]
    #[serial]
    fn test_a_reply_is_owed_for_as_long_as_it_takes() {
        forget_probed_size();
        let start = Instant::now();

        // The question went out, an age passed, and the console finally answered
        // it. That answer is this probe's, whenever it turns up: taking it only
        // within some window of the question is what left a slow console -- a
        // serial line, a terminal across a network -- at 80x24 for good.
        let mut probe = awaiting(start);
        assert!(!probe.due(start + PROBE_INTERVAL / 2));
        assert_eq!(probe.reply(120, 40), Reply::Resized(121, 41));
        forget_probed_size();
    }

    #[test]
    #[serial]
    fn test_a_report_nobody_asked_for_is_passed_on() {
        let now = Instant::now();

        // No probe outstanding.
        assert_eq!(SizeProbe::new(true).reply(120, 40), Reply::NotMine);

        // One reply per probe.
        forget_probed_size();
        let mut probe = awaiting(now);
        assert!(matches!(probe.reply(120, 40), Reply::Resized(..)));
        assert_eq!(probe.reply(120, 40), Reply::NotMine);
        forget_probed_size();
    }

    #[test]
    fn test_only_supported_resize_mode_status_stops_probing() {
        let now = Instant::now();
        for status in [0, 4, 5] {
            let mut probe = SizeProbe::new(true);
            probe.resize_mode_status(status);
            assert_eq!(probe.next_probe_in(now), Some(Duration::ZERO));
        }
        for status in 1..=3 {
            let mut probe = SizeProbe::new(true);
            probe.resize_mode_status(status);
            assert_eq!(probe.next_probe_in(now), None);
        }
    }

    #[test]
    #[serial]
    fn test_a_resize_report_confirms_support_without_decrpm() {
        forget_probed_size();
        let now = Instant::now();
        let mut probe = SizeProbe::new(true);
        assert!(probe.resize_mode_report(120, 40));
        assert_eq!(probe.next_probe_in(now), None);
        assert!(!probe.resize_mode_report(120, 40));
        forget_probed_size();
    }
}
