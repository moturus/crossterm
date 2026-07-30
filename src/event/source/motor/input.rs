//! What sits between a Motor OS console and the ANSI parser.
//!
//! Both problems here come from how the platform delivers bytes rather than
//! from any API, so the logic is byte-in, byte-out and is unit-tested on the
//! host as well as compiled for Motor OS.

/// Drops the `\n` of a CR LF pair.
///
/// One Enter keypress reaches a program as CR LF on Motor OS, from the serial
/// console and from an `rmux` pane alike. Left alone the `\n` becomes a key
/// event of its own — `Ctrl+J` in raw mode, a second `Enter` outside it — so
/// every Enter would be reported twice. Over `ssh` the client's terminal is in
/// raw mode and sends a lone `\r`, which passes through untouched.
#[derive(Debug, Default)]
pub(crate) struct CrLfCoalescer {
    after_cr: bool,
}

impl CrLfCoalescer {
    /// Removes, in place, every `\n` that immediately follows a `\r`, and
    /// returns the length of what is left.
    ///
    /// The one bit of state carries across calls, so a CR LF split between two
    /// reads — the normal case on a console that delivers a byte at a time — is
    /// still one Enter.
    pub(crate) fn retain(&mut self, buffer: &mut [u8]) -> usize {
        let mut kept = 0;

        for read in 0..buffer.len() {
            let byte = buffer[read];
            let after_cr = std::mem::replace(&mut self.after_cr, byte == b'\r');
            if byte == b'\n' && after_cr {
                continue;
            }

            buffer[kept] = byte;
            kept += 1;
        }

        kept
    }
}

#[cfg(test)]
mod tests {
    use super::CrLfCoalescer;

    /// Runs `bursts` through one coalescer and returns what came out of each.
    fn coalesce(bursts: &[&[u8]]) -> Vec<Vec<u8>> {
        let mut coalescer = CrLfCoalescer::default();

        bursts
            .iter()
            .map(|burst| {
                let mut buffer = burst.to_vec();
                let kept = coalescer.retain(&mut buffer);
                buffer.truncate(kept);
                buffer
            })
            .collect()
    }

    #[test]
    fn test_crlf_in_one_burst_is_one_enter() {
        assert_eq!(coalesce(&[b"\r\n"]), vec![b"\r".to_vec()]);
    }

    #[test]
    fn test_crlf_split_between_bursts_is_one_enter() {
        assert_eq!(
            coalesce(&[b"\r", b"\n"]),
            vec![b"\r".to_vec(), b"".to_vec()]
        );
    }

    #[test]
    fn test_lone_lf_is_kept() {
        assert_eq!(coalesce(&[b"\n"]), vec![b"\n".to_vec()]);
        assert_eq!(
            coalesce(&[b"a", b"\n"]),
            vec![b"a".to_vec(), b"\n".to_vec()]
        );
    }

    #[test]
    fn test_only_the_lf_that_follows_a_cr_is_dropped() {
        // The second `\n` follows an `\n`, not a `\r`, and is a keypress of its
        // own.
        assert_eq!(coalesce(&[b"\r\n\n"]), vec![b"\r\n".to_vec()]);
        // Two Enters in a row.
        assert_eq!(coalesce(&[b"\r\n\r\n"]), vec![b"\r\r".to_vec()]);
        // A `\r` that is not part of a pair is left where it is.
        assert_eq!(coalesce(&[b"\r\r\n"]), vec![b"\r\r".to_vec()]);
    }

    #[test]
    fn test_other_bytes_pass_through() {
        assert_eq!(
            coalesce(&[b"\x1b[A", b"hello"]),
            vec![b"\x1b[A".to_vec(), b"hello".to_vec()]
        );
    }

    #[test]
    fn test_cr_state_does_not_outlive_an_intervening_byte() {
        // `\r` `a` `\n`: the `\n` is not the second half of that Enter.
        assert_eq!(
            coalesce(&[b"\r", b"a", b"\n"]),
            vec![b"\r".to_vec(), b"a".to_vec(), b"\n".to_vec()]
        );
    }

    #[test]
    fn test_empty_burst_keeps_pending_cr_state() {
        assert_eq!(
            coalesce(&[b"\r", b"", b"\n"]),
            vec![b"\r".to_vec(), b"".to_vec(), b"".to_vec()]
        );
    }
}
