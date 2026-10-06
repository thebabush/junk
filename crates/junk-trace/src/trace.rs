//! A whole trace: its lines in order, read from and written back to the v1 text.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use crate::{DataLine, EventLine, Line, LineError};

/// A trace: every line of the file, in order, comments included.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Trace {
    /// The lines, top to bottom.
    pub lines: Vec<Line>,
}

impl Trace {
    /// Reads a whole trace.
    ///
    /// Lines end with `\n`; the last one may go without. A `\r` is not a line ending and
    /// stays part of its line, which a data line then rejects. Empty text is an empty trace.
    ///
    /// # Errors
    ///
    /// [`ParseError`] naming the first line that did not read, 1-based, and why.
    pub fn parse(text: &str) -> Result<Trace, ParseError> {
        if text.is_empty() {
            return Ok(Trace::default());
        }
        let body = text.strip_suffix('\n').unwrap_or(text);
        let lines = body
            .split('\n')
            .enumerate()
            .map(|(index, line)| {
                Line::parse(line).map_err(|error| ParseError {
                    line: index + 1,
                    error,
                })
            })
            .collect::<Result<_, _>>()?;
        Ok(Trace { lines })
    }

    /// The whole trace as text, every line newline-terminated.
    ///
    /// The exact inverse of [`Trace::parse`] for text the writer produces: lowercase hex, a
    /// final newline, no `-00:00` offset. Uppercase hex reads fine but comes back lowercase.
    #[must_use]
    pub fn render(&self) -> String {
        self.to_string()
    }

    /// The leading run of comments: every line before the first event or data line.
    pub fn header(&self) -> impl Iterator<Item = &str> {
        self.lines.iter().map_while(|line| match line {
            Line::Comment(text) => Some(text.as_str()),
            Line::Event(_) | Line::Data(_) => None,
        })
    }

    /// The data lines, in order.
    pub fn data(&self) -> impl Iterator<Item = &DataLine> {
        self.lines.iter().filter_map(|line| match line {
            Line::Data(data) => Some(data),
            Line::Comment(_) | Line::Event(_) => None,
        })
    }

    /// The event lines, in order.
    pub fn events(&self) -> impl Iterator<Item = &EventLine> {
        self.lines.iter().filter_map(|line| match line {
            Line::Event(event) => Some(event),
            Line::Comment(_) | Line::Data(_) => None,
        })
    }
}

/// Same as [`Trace::render`].
impl fmt::Display for Trace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for line in &self.lines {
            writeln!(f, "{line}")?;
        }
        Ok(())
    }
}

/// Where a trace stopped reading, and why.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ParseError {
    /// The 1-based number of the line that did not read.
    pub line: usize,
    /// What was wrong with it.
    pub error: LineError,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.error)
    }
}

impl core::error::Error for ParseError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        Some(&self.error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Direction, Stamp, StampError};
    use alloc::borrow::ToOwned;

    const SMALL: &str = "\
# junk trace v1 — small
# columns: <iso-ts> <tx|rx> <channel> <hex>
! 2026-07-02T14:16:28.957 connected mtu=247
2026-07-02T14:16:30.474 tx v1.write 04011200000000000000000000000017
2026-07-02T14:16:34.402 rx v1.notify 04000000000000000000000000000004
# a comment in the middle
2026-07-02T14:16:37.338 tx v2.cmd bc300000ffff
! 2026-07-02T14:16:40.000 disconnected
";

    fn trace(text: &str) -> Trace {
        Trace::parse(text).unwrap_or_else(|err| panic!("{err}"))
    }

    #[test]
    fn parses_and_renders_exactly() {
        let parsed = trace(SMALL);
        assert_eq!(parsed.lines.len(), 8);
        assert_eq!(parsed.render(), SMALL);
        assert_eq!(parsed.to_string(), SMALL);
        assert_eq!(trace(&parsed.render()), parsed);
    }

    #[test]
    fn empty_text_is_an_empty_trace() {
        assert_eq!(trace(""), Trace::default());
        assert_eq!(Trace::default().render(), "");
    }

    #[test]
    fn last_line_may_lack_its_newline() {
        let text = SMALL.strip_suffix('\n').unwrap_or(SMALL);
        assert_eq!(trace(text), trace(SMALL));
        assert_eq!(trace(text).render(), SMALL);
    }

    #[test]
    fn header_is_the_leading_comment_run() {
        let parsed = trace(SMALL);
        assert_eq!(
            parsed.header().collect::<Vec<_>>(),
            [
                " junk trace v1 — small",
                " columns: <iso-ts> <tx|rx> <channel> <hex>"
            ]
        );
        let no_header = trace("2026-07-02T14:16:30.474 tx v1.write 04\n# late\n");
        assert_eq!(no_header.header().count(), 0);
        let only_header = trace("# one\n# two\n");
        assert_eq!(only_header.header().count(), 2);
    }

    #[test]
    fn data_and_events_skip_the_other_kinds() {
        let parsed = trace(SMALL);
        let chans: Vec<&str> = parsed.data().map(|d| d.chan.as_str()).collect();
        assert_eq!(chans, ["v1.write", "v1.notify", "v2.cmd"]);
        let dirs: Vec<Direction> = parsed.data().map(|d| d.dir).collect();
        assert_eq!(dirs, [Direction::Tx, Direction::Rx, Direction::Tx]);
        let texts: Vec<&str> = parsed.events().map(|e| e.text.as_str()).collect();
        assert_eq!(texts, ["connected mtu=247", "disconnected"]);
        assert_eq!(
            parsed.events().next().map(|e| e.at),
            Some(Stamp::parse("2026-07-02T14:16:28.957").unwrap())
        );
    }

    #[test]
    fn uppercase_hex_renders_lowercase() {
        let upper = "2026-07-02T14:16:37.338 tx v2.cmd BC300000FFFF\n";
        let lower = "2026-07-02T14:16:37.338 tx v2.cmd bc300000ffff\n";
        assert_eq!(trace(upper), trace(lower));
        assert_eq!(trace(upper).render(), lower);
    }

    #[test]
    fn errors_carry_line_numbers() {
        let odd_hex = "\
# header
2026-07-02T14:16:30.474 tx v1.write 0401
2026-07-02T14:16:30.475 rx v1.notify 040
";
        assert_eq!(
            Trace::parse(odd_hex),
            Err(ParseError {
                line: 3,
                error: LineError::BadHex("040".to_owned()),
            })
        );

        let bad_dir = "# header\n2026-07-02T14:16:30.474 tz v1.write 0401\n";
        assert_eq!(
            Trace::parse(bad_dir),
            Err(ParseError {
                line: 2,
                error: LineError::BadDir("tz".to_owned()),
            })
        );

        let five_fields = "2026-07-02T14:16:30.474 tx v1.write 0401 extra\n";
        assert_eq!(
            Trace::parse(five_fields),
            Err(ParseError {
                line: 1,
                error: LineError::TrailingFields,
            })
        );

        let blank = "# header\n2026-07-02T14:16:30.474 tx v1.write 0401\n\n# more\n";
        assert_eq!(
            Trace::parse(blank),
            Err(ParseError {
                line: 3,
                error: LineError::Blank,
            })
        );
        assert_eq!(
            Trace::parse("\n"),
            Err(ParseError {
                line: 1,
                error: LineError::Blank,
            })
        );

        let bad_stamp = "# header\n! 2026-13-01T00:00:00.000 text\n";
        assert_eq!(
            Trace::parse(bad_stamp),
            Err(ParseError {
                line: 2,
                error: LineError::BadStamp(StampError::OutOfRange("month")),
            })
        );

        // The first bad line wins.
        let two_bad = "2026-07-02T14:16:30.474 tz v1.write 0401\n\n";
        assert_eq!(Trace::parse(two_bad).map_err(|e| e.line), Err(1));
    }

    #[test]
    fn crlf_is_not_a_line_ending() {
        let crlf = "# header\r\n2026-07-02T14:16:30.474 tx v1.write 0401\r\n";
        assert_eq!(
            Trace::parse(crlf),
            Err(ParseError {
                line: 2,
                error: LineError::BadHex("0401\r".to_owned()),
            })
        );
    }

    #[test]
    fn error_display_and_source() {
        let err = ParseError {
            line: 3,
            error: LineError::BadHex("040".to_owned()),
        };
        assert_eq!(
            err.to_string(),
            "line 3: bad hex \"040\", expected an even number of hex digits"
        );
        assert!(core::error::Error::source(&err).is_some());
    }
}
