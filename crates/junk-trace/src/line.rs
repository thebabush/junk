//! One line of a trace: a comment, an event or a data line, and how each reads and writes.

use alloc::borrow::ToOwned;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use core::str::FromStr;

use crate::{Stamp, StampError};

/// Which way the bytes of a data line went, seen from the host.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    /// Host to device: a write.
    Tx,
    /// Device to host: a notification, an indication or a read response.
    Rx,
}

impl Direction {
    /// The field as the trace writes it: `tx` or `rx`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Direction::Tx => "tx",
            Direction::Rx => "rx",
        }
    }
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `tx` or `rx`, exactly; anything else is [`LineError::BadDir`].
impl FromStr for Direction {
    type Err = LineError;

    fn from_str(text: &str) -> Result<Direction, LineError> {
        match text {
            "tx" => Ok(Direction::Tx),
            "rx" => Ok(Direction::Rx),
            other => Err(LineError::BadDir(other.to_owned())),
        }
    }
}

/// Bytes that crossed the link: `<stamp> <tx|rx> <channel> <hex>`.
///
/// The channel is whatever name the writer used (`v1.write`, `v2.notify`, …); this crate
/// keeps it as text and a device crate maps it to its own channels.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DataLine {
    /// When.
    pub at: Stamp,
    /// Which way.
    pub dir: Direction,
    /// The characteristic, by the writer's name for it.
    pub chan: String,
    /// The payload, at least one byte.
    pub bytes: Vec<u8>,
}

/// Something that happened around the link: `! <stamp> <free text>`.
///
/// Connections, disconnections, MTU changes, values on unmapped handles, app-side log
/// lines: kept for orientation, never interpreted here.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct EventLine {
    /// When.
    pub at: Stamp,
    /// Everything after the stamp, verbatim.
    pub text: String,
}

/// One line of a trace, without its newline.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Line {
    /// A `#` line: the text after the `#`, leading space and all.
    Comment(String),
    /// A `! ` line.
    Event(EventLine),
    /// Anything else.
    Data(DataLine),
}

impl Line {
    /// Reads one line, given without its newline.
    ///
    /// A line starting with `#` is a comment, one starting with `! ` an event, anything
    /// else must be a data line. Hex is read case-insensitively; it is written lowercase.
    ///
    /// # Errors
    ///
    /// [`LineError::Blank`] for an empty or whitespace-only line, otherwise whatever the
    /// event or data syntax rejects; see [`LineError`].
    pub fn parse(text: &str) -> Result<Line, LineError> {
        if text.trim().is_empty() {
            return Err(LineError::Blank);
        }
        if let Some(comment) = text.strip_prefix('#') {
            return Ok(Line::Comment(comment.to_owned()));
        }
        if let Some(event) = text.strip_prefix("! ") {
            return parse_event(event).map(Line::Event);
        }
        parse_data(text).map(Line::Data)
    }
}

/// Same as [`Line::parse`].
impl FromStr for Line {
    type Err = LineError;

    fn from_str(text: &str) -> Result<Line, LineError> {
        Line::parse(text)
    }
}

impl fmt::Display for Line {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Line::Comment(text) => write!(f, "#{text}"),
            Line::Event(event) => event.fmt(f),
            Line::Data(data) => data.fmt(f),
        }
    }
}

impl fmt::Display for EventLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "! {} {}", self.at, self.text)
    }
}

impl fmt::Display for DataLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} {} ", self.at, self.dir, self.chan)?;
        for byte in &self.bytes {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// Reads what follows `! `: a stamp, a space, then the text (which may be empty).
fn parse_event(rest: &str) -> Result<EventLine, LineError> {
    let (stamp, text) = match rest.split_once(' ') {
        Some((stamp, text)) => (stamp, Some(text)),
        None => (rest, None),
    };
    let at = Stamp::parse(stamp)?;
    let text = text.ok_or(LineError::MissingField("text"))?;
    Ok(EventLine {
        at,
        text: text.to_owned(),
    })
}

/// Reads `<stamp> <tx|rx> <channel> <hex>`, single spaces, nothing after.
fn parse_data(line: &str) -> Result<DataLine, LineError> {
    let mut fields = line.split(' ');
    let at = Stamp::parse(field(&mut fields, "stamp")?)?;
    let dir = field(&mut fields, "dir")?.parse()?;
    let chan = field(&mut fields, "chan")?.to_owned();
    let bytes = decode_hex(field(&mut fields, "hex")?)?;
    if fields.next().is_some() {
        return Err(LineError::TrailingFields);
    }
    Ok(DataLine {
        at,
        dir,
        chan,
        bytes,
    })
}

/// The next field, which must exist and be non-empty (an empty one is a doubled space).
fn field<'a>(
    fields: &mut impl Iterator<Item = &'a str>,
    name: &'static str,
) -> Result<&'a str, LineError> {
    match fields.next() {
        Some(text) if !text.is_empty() => Ok(text),
        Some(_) | None => Err(LineError::MissingField(name)),
    }
}

/// The value of one hex digit, either case.
fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Reads an even number of hex digits, either case, no separators.
fn decode_hex(text: &str) -> Result<Vec<u8>, LineError> {
    let mut bytes = Vec::with_capacity(text.len() / 2);
    let mut digits = text.bytes().map(hex_digit);
    while let Some(high) = digits.next() {
        let (Some(high), Some(Some(low))) = (high, digits.next()) else {
            return Err(LineError::BadHex(text.to_owned()));
        };
        bytes.push((high << 4) | low);
    }
    Ok(bytes)
}

/// Why one line did not read.
///
/// A field that is absent, or present but empty (two spaces in a row), is
/// [`LineError::MissingField`]; the other variants describe a field that is there and wrong.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum LineError {
    /// The line is empty or only whitespace.
    Blank,
    /// The stamp of an event or data line did not read.
    BadStamp(StampError),
    /// The direction field is neither `tx` nor `rx`; carries what it was.
    BadDir(String),
    /// A field is absent or empty; the text names it (`stamp`, `dir`, `chan`, `hex`, `text`).
    MissingField(&'static str),
    /// The hex field has an odd number of digits or a character that is not one; carries
    /// what it was.
    BadHex(String),
    /// A data line goes on after its hex field.
    TrailingFields,
}

impl fmt::Display for LineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LineError::Blank => f.write_str("blank line"),
            LineError::BadStamp(err) => write!(f, "bad timestamp: {err}"),
            LineError::BadDir(text) => write!(f, "bad direction {text:?}, expected tx or rx"),
            LineError::MissingField(name) => write!(f, "missing {name} field"),
            LineError::BadHex(text) => {
                write!(f, "bad hex {text:?}, expected an even number of hex digits")
            }
            LineError::TrailingFields => f.write_str("unexpected text after the hex field"),
        }
    }
}

impl core::error::Error for LineError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            LineError::BadStamp(err) => Some(err),
            LineError::Blank
            | LineError::BadDir(_)
            | LineError::MissingField(_)
            | LineError::BadHex(_)
            | LineError::TrailingFields => None,
        }
    }
}

impl From<StampError> for LineError {
    fn from(err: StampError) -> Self {
        LineError::BadStamp(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec;

    fn stamp(text: &str) -> Stamp {
        Stamp::parse(text).unwrap_or_else(|err| panic!("{text}: {err}"))
    }

    fn line(text: &str) -> Line {
        Line::parse(text).unwrap_or_else(|err| panic!("{text}: {err}"))
    }

    #[test]
    fn dir_round_trips() {
        assert_eq!("tx".parse::<Direction>(), Ok(Direction::Tx));
        assert_eq!("rx".parse::<Direction>(), Ok(Direction::Rx));
        assert_eq!(Direction::Tx.to_string(), "tx");
        assert_eq!(Direction::Rx.to_string(), "rx");
        assert_eq!(
            "TX".parse::<Direction>(),
            Err(LineError::BadDir("TX".to_owned()))
        );
        assert_eq!(
            "".parse::<Direction>(),
            Err(LineError::BadDir(String::new()))
        );
    }

    #[test]
    fn comment_keeps_text_after_hash() {
        assert_eq!(line("# a comment"), Line::Comment(" a comment".to_owned()));
        assert_eq!(line("#no space"), Line::Comment("no space".to_owned()));
        assert_eq!(line("#"), Line::Comment(String::new()));
        assert_eq!(line("# trailing  ").to_string(), "# trailing  ");
    }

    #[test]
    fn event_keeps_text_verbatim() {
        let text = "! 2026-07-02T14:16:34.477 app-error init.2 时间 设置失败";
        let parsed = line(text);
        assert_eq!(
            parsed,
            Line::Event(EventLine {
                at: stamp("2026-07-02T14:16:34.477"),
                text: "app-error init.2 时间 设置失败".to_owned(),
            })
        );
        assert_eq!(parsed.to_string(), text);

        let trailing = "! 2026-07-02T14:39:19.625 🌟+++Connect:<state = disconnected>, ";
        assert_eq!(line(trailing).to_string(), trailing);

        let empty = "! 2025-11-19T10:21:50.657-05:00 ";
        assert_eq!(
            line(empty),
            Line::Event(EventLine {
                at: stamp("2025-11-19T10:21:50.657-05:00"),
                text: String::new(),
            })
        );
        assert_eq!(line(empty).to_string(), empty);
    }

    #[test]
    fn data_round_trips() {
        let text = "2025-11-19T10:21:49.121-05:00 tx v2.cmd bc410400002400000000";
        let parsed = line(text);
        assert_eq!(
            parsed,
            Line::Data(DataLine {
                at: stamp("2025-11-19T10:21:49.121-05:00"),
                dir: Direction::Tx,
                chan: "v2.cmd".to_owned(),
                bytes: vec![0xbc, 0x41, 0x04, 0x00, 0x00, 0x24, 0x00, 0x00, 0x00, 0x00],
            })
        );
        assert_eq!(parsed.to_string(), text);
    }

    #[test]
    fn hex_reads_either_case_and_writes_lowercase() {
        let upper = line("2026-07-02T14:16:37.338 tx v2.cmd BC300000FFFF");
        let mixed = line("2026-07-02T14:16:37.338 tx v2.cmd bC300000fFfF");
        let lower = line("2026-07-02T14:16:37.338 tx v2.cmd bc300000ffff");
        assert_eq!(upper, lower);
        assert_eq!(mixed, lower);
        assert_eq!(
            upper.to_string(),
            "2026-07-02T14:16:37.338 tx v2.cmd bc300000ffff"
        );
    }

    #[test]
    fn rejects_blank() {
        assert_eq!(Line::parse(""), Err(LineError::Blank));
        assert_eq!(Line::parse("   "), Err(LineError::Blank));
        assert_eq!(Line::parse("\t"), Err(LineError::Blank));
    }

    #[test]
    fn rejects_bad_fields() {
        let good = "2026-07-02T14:16:30.474 tx v1.write 0401";
        assert!(Line::parse(good).is_ok());

        assert_eq!(
            Line::parse("2026-07-02T14:16:30.474 tz v1.write 0401"),
            Err(LineError::BadDir("tz".to_owned()))
        );
        assert_eq!(
            Line::parse("2026-07-02T14:16:30.474 tx v1.write 040"),
            Err(LineError::BadHex("040".to_owned()))
        );
        assert_eq!(
            Line::parse("2026-07-02T14:16:30.474 tx v1.write 04zz"),
            Err(LineError::BadHex("04zz".to_owned()))
        );
        assert_eq!(
            Line::parse("2026-07-02T14:16:30.474 tx v1.write 04 01"),
            Err(LineError::TrailingFields)
        );
        assert_eq!(
            Line::parse("2026-07-02T14:16:30.474 tx v1.write 0401 "),
            Err(LineError::TrailingFields)
        );
        assert_eq!(
            Line::parse("2026-07-02T14:16:30.474 tx v1.write"),
            Err(LineError::MissingField("hex"))
        );
        assert_eq!(
            Line::parse("2026-07-02T14:16:30.474 tx v1.write "),
            Err(LineError::MissingField("hex"))
        );
        assert_eq!(
            Line::parse("2026-07-02T14:16:30.474 tx"),
            Err(LineError::MissingField("chan"))
        );
        assert_eq!(
            Line::parse("2026-07-02T14:16:30.474  v1.write 0401"),
            Err(LineError::MissingField("dir"))
        );
        assert_eq!(
            Line::parse("2026-07-02T14:16:30.474"),
            Err(LineError::MissingField("dir"))
        );
        assert_eq!(
            Line::parse(" tx v1.write 0401"),
            Err(LineError::MissingField("stamp"))
        );
        assert!(matches!(
            Line::parse("garbage tx v1.write 0401"),
            Err(LineError::BadStamp(StampError::Malformed(_)))
        ));
        assert!(matches!(
            Line::parse("2026-13-01T00:00:00.000 tx v1.write 0401"),
            Err(LineError::BadStamp(StampError::OutOfRange("month")))
        ));
        assert!(matches!(
            Line::parse("2026-07-02T14:16:30.474 tx v1.write 0401\r"),
            Err(LineError::BadHex(_))
        ));
    }

    #[test]
    fn rejects_bad_events() {
        assert_eq!(
            Line::parse("! 2026-07-02T14:16:34.477"),
            Err(LineError::MissingField("text"))
        );
        assert!(matches!(
            Line::parse("! garbage text"),
            Err(LineError::BadStamp(_))
        ));
        assert!(matches!(Line::parse("! "), Err(LineError::BadStamp(_))));
        // Without the space after `!` it is not an event and fails as a data line.
        assert!(matches!(
            Line::parse("!2026-07-02T14:16:34.477 text"),
            Err(LineError::BadStamp(_))
        ));
    }

    #[test]
    fn errors_display_and_chain() {
        let err = LineError::BadStamp(StampError::OutOfRange("month"));
        assert_eq!(
            err.to_string(),
            "bad timestamp: timestamp month out of range"
        );
        assert!(core::error::Error::source(&err).is_some());
        assert!(core::error::Error::source(&LineError::TrailingFields).is_none());
        assert_eq!(
            LineError::BadDir("tz".to_owned()).to_string(),
            "bad direction \"tz\", expected tx or rx"
        );
        assert_eq!(
            LineError::MissingField("hex").to_string(),
            "missing hex field"
        );
    }
}
