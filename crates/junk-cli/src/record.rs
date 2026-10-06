//! Recording a session as a trace under `fixtures/` (SPEC §5, Stage B4): the link wrapper
//! that writes lines down, and the file with its header.

use std::fs;
use std::path::Path;

use anyhow::Context as _;
use junk_core::{Channel, Link};
use junk_pump::record::RecordingLink;
use junk_trace::{Line, Stamp, StampError, Trace};

/// How a device family names its channels in a trace.
type ChannelName = fn(Channel) -> Option<&'static str>;

/// `link`, recording everything that crosses it with the wall clock's stamps and the
/// channel names of the device family (`junk_colmi::channel_name`, say).
///
/// # Errors
///
/// [`StampError`] if the wall clock is outside the trace format's range.
pub fn recording<L: Link>(
    link: L,
    name: ChannelName,
) -> Result<RecordingLink<L, impl FnMut() -> Stamp, ChannelName>, StampError> {
    Ok(RecordingLink::new(link, junk_app::wall_clock()?, name))
}

/// Writes `lines` to `path` as a trace, headed by what `junk <command>` recorded from
/// `device` starting at `started`.
///
/// # Errors
///
/// If the file cannot be written.
pub fn write_trace(
    path: &Path,
    command: &str,
    device: &str,
    started: &Stamp,
    lines: Vec<Line>,
) -> anyhow::Result<()> {
    let date = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        started.year(),
        started.month(),
        started.day(),
        started.hour(),
        started.minute()
    );
    let mut all = vec![
        Line::Comment(format!(" junk trace v1 — junk {command}, {device}, {date}")),
        Line::Comment(format!(" source: junk {command} --record")),
        Line::Comment(" columns: <iso-ts> <tx|rx> <channel> <hex>".to_owned()),
        Line::Comment(
            " lines starting with # are comments; \"! \" lines are link events kept for orientation"
                .to_owned(),
        ),
    ];
    all.extend(lines);
    let trace = Trace { lines: all };
    fs::write(path, trace.render()).with_context(|| format!("cannot write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use junk_trace::{DataLine, Direction};

    #[test]
    fn the_header_precedes_the_lines_and_reads_back() {
        let dir = std::env::temp_dir().join(format!("junk-cli-record-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap_or_else(|err| panic!("{err}"));
        let path = dir.join("session.trace");
        let started =
            Stamp::parse("2026-09-06T12:00:00.000-04:00").unwrap_or_else(|err| panic!("{err}"));
        let line = Line::Data(DataLine {
            at: started,
            dir: Direction::Tx,
            chan: "v1.write".to_owned(),
            bytes: vec![0x04, 0x01],
        });
        write_trace(
            &path,
            "sync",
            "COLMI R10_F300",
            &started,
            vec![line.clone()],
        )
        .unwrap_or_else(|err| panic!("{err}"));
        let text = fs::read_to_string(&path).unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(
            text,
            "\
# junk trace v1 — junk sync, COLMI R10_F300, 2026-09-06 12:00
# source: junk sync --record
# columns: <iso-ts> <tx|rx> <channel> <hex>
# lines starting with # are comments; \"! \" lines are link events kept for orientation
2026-09-06T12:00:00.000-04:00 tx v1.write 0401
"
        );
        let trace = Trace::parse(&text).unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(trace.header().count(), 4);
        assert_eq!(trace.data().count(), 1);
        assert_eq!(trace.lines.last(), Some(&line));
        let _ = fs::remove_dir_all(&dir);
    }
}
