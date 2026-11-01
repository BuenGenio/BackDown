//! Where one message of an mbox file ends and the next begins: the rule mail merge and the
//! browser's mbox viewers share, so a file is cut into the same messages by both.
//!
//! A message starts at a "From " line that follows a blank line, or that looks like an envelope
//! line ("From sender Mon Jan  1 12:00:00 2024": it has a clock in it) for the writers that don't
//! leave a blank line before it. A "From " inside a message is quoted (">From "), so it never
//! starts one.

/// An empty line, as an mbox file ends it.
pub fn blank(line: &[u8]) -> bool {
    line == b"\n" || line == b"\r\n"
}

/// Does `line` start a message, given whether the line before it was blank.
pub fn is_from_line(line: &[u8], prev_blank: bool) -> bool {
    line.starts_with(b"From ") && (prev_blank || has_clock(line))
}

/// "12:00" somewhere in the line.
fn has_clock(l: &[u8]) -> bool {
    l.windows(5).any(|w| {
        w[0].is_ascii_digit()
            && w[1].is_ascii_digit()
            && w[2] == b':'
            && w[3].is_ascii_digit()
            && w[4].is_ascii_digit()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn where_messages_start() {
        assert!(is_from_line(
            b"From someone@example.org Mon Jan  1 12:00:00 2024\n",
            true
        ));
        // no blank line before it, but an envelope's clock
        assert!(is_from_line(
            b"From someone@example.org Mon Jan  1 12:00:00 2024\n",
            false
        ));
        // in a message's text, without either
        assert!(!is_from_line(b"From what I remember, it was fine\n", false));
        assert!(is_from_line(b"From what I remember, it was fine\n", true));
        assert!(!is_from_line(
            b">From someone Mon Jan  1 12:00:00 2024\n",
            true
        ));
        assert!(blank(b"\n") && blank(b"\r\n"));
        assert!(!blank(b" \n") && !blank(b""));
    }
}
