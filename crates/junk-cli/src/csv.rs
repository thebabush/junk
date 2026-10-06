//! The CSV files `sync` and `replay` write: one per sample kind, merged with what is
//! already on disk so that syncing twice writes the same files (SPEC §5, Stage B3).
//!
//! [`rows`] turns a session's [`Samples`] into the text of every file; from there rows are
//! kept as text. Every row starts with an ISO stamp with its offset, and a file is sorted
//! by that first column, rows with the same stamp staying in the order they came (a
//! night's sleep stages, say); a row already in the file is not written twice. Values are
//! numbers and stamps, so nothing needs quoting.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::Path;

use junk_app::{Samples, stamp_of};
use junk_colmi::wire::{WorkoutRecord, WorkoutTag};
use junk_colmi::{Bpm, SleepKind, Source};
use junk_trace::StampError;

/// Every row `samples` writes, by kind, in the order the ring gave them.
///
/// # Errors
///
/// [`StampError`] if a sample's time cannot be written as a stamp (a year past 9999).
pub fn rows(samples: &Samples) -> Result<SampleSet, StampError> {
    let mut set = SampleSet::default();
    for sample in &samples.hr {
        let source = match sample.source {
            Source::Periodic => "periodic",
            Source::Manual => "manual",
            Source::Live => "live",
        };
        let at = stamp_of(sample.at)?;
        // An invalid reading keeps its row, with the field empty.
        let bpm = match sample.bpm {
            Bpm::Valid(value) => value.to_string(),
            Bpm::Invalid => String::new(),
        };
        set.push(Kind::Hr, format!("{at},{bpm},{source}"));
    }
    for bucket in &samples.steps {
        let start = stamp_of(bucket.start)?;
        set.push(
            Kind::Steps,
            format!(
                "{start},{},{},{},{}",
                bucket.span.as_secs(),
                bucket.steps,
                bucket.cal,
                bucket.distance_m
            ),
        );
    }
    for sample in &samples.spo2 {
        let at = stamp_of(sample.at)?;
        set.push(Kind::Spo2, format!("{at},{}", sample.percent.get()));
    }
    for sample in &samples.hrv {
        let at = stamp_of(sample.at)?;
        set.push(Kind::Hrv, format!("{at},{}", sample.value));
    }
    for sample in &samples.stress {
        let at = stamp_of(sample.at)?;
        set.push(Kind::Stress, format!("{at},{}", sample.value));
    }
    for sample in &samples.temperature {
        let at = stamp_of(sample.at)?;
        set.push(Kind::Temperature, format!("{at},{}", sample.deci_celsius));
    }
    for session in &samples.sleep {
        let start = stamp_of(session.start)?;
        let end = stamp_of(session.end)?;
        for (index, stage) in session.stages.iter().enumerate() {
            let kind = sleep_kind(stage.kind);
            set.push(
                Kind::Sleep,
                format!("{start},{end},{index},{kind},{}", stage.minutes),
            );
        }
    }
    for record in &samples.workouts {
        set.push(Kind::Workouts, workout_row(record));
    }
    Ok(set)
}

/// The `kind` column of `sleep.csv`.
fn sleep_kind(kind: SleepKind) -> String {
    match kind {
        SleepKind::Light => "light".to_owned(),
        SleepKind::Deep => "deep".to_owned(),
        SleepKind::Rem => "rem".to_owned(),
        SleepKind::Awake => "awake".to_owned(),
        SleepKind::Unknown(code) => format!("unknown-{code}"),
    }
}

/// One row of `workouts.csv`: the raw TLV numbers, an empty field for a tag the record
/// lacks.
fn workout_row(record: &WorkoutRecord) -> String {
    let mut row = String::new();
    let field = |row: &mut String, tag| {
        if let Some(value) = record.get(tag) {
            let _ = write!(row, "{value}");
        }
    };
    field(&mut row, WorkoutTag::StartTime);
    let _ = write!(row, ",{}", record.sport_type);
    for tag in [
        WorkoutTag::Duration,
        WorkoutTag::RateAvg,
        WorkoutTag::RateMin,
        WorkoutTag::RateMax,
        WorkoutTag::Steps,
        WorkoutTag::Distance,
        WorkoutTag::Calories,
    ] {
        row.push(',');
        field(&mut row, tag);
    }
    row
}

/// The sample kinds, one file each.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    /// `hr.csv`: the periodic heart-rate log.
    Hr,
    /// `steps.csv`: activity buckets.
    Steps,
    /// `spo2.csv`: blood oxygen.
    Spo2,
    /// `hrv.csv`: heart-rate variability.
    Hrv,
    /// `stress.csv`: stress.
    Stress,
    /// `temperature.csv`: skin temperature.
    Temperature,
    /// `sleep.csv`: sleep stages, one row each.
    Sleep,
    /// `workouts.csv`: stored workout records.
    Workouts,
}

impl Kind {
    /// Every kind, in the order the files are written.
    pub const ALL: [Kind; 8] = [
        Kind::Hr,
        Kind::Steps,
        Kind::Spo2,
        Kind::Hrv,
        Kind::Stress,
        Kind::Temperature,
        Kind::Sleep,
        Kind::Workouts,
    ];

    /// The file's name inside the CSV directory.
    pub const fn file_name(self) -> &'static str {
        match self {
            Kind::Hr => "hr.csv",
            Kind::Steps => "steps.csv",
            Kind::Spo2 => "spo2.csv",
            Kind::Hrv => "hrv.csv",
            Kind::Stress => "stress.csv",
            Kind::Temperature => "temperature.csv",
            Kind::Sleep => "sleep.csv",
            Kind::Workouts => "workouts.csv",
        }
    }

    /// The header row, without its newline.
    pub const fn header(self) -> &'static str {
        match self {
            Kind::Hr => "at,bpm,source",
            Kind::Steps => "start,span_s,steps,cal,distance_m",
            Kind::Spo2 => "at,percent",
            Kind::Hrv | Kind::Stress => "at,value",
            Kind::Temperature => "at,deci_celsius",
            Kind::Sleep => "start,end,index,kind,minutes",
            Kind::Workouts => {
                "start_ring,sport_type,duration_s,rate_avg,rate_min,rate_max,steps,distance,calories"
            }
        }
    }
}

/// One kind's rows, each once, in the order they came.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Rows {
    order: Vec<String>,
    seen: BTreeSet<String>,
}

impl Rows {
    fn push(&mut self, row: String) {
        if self.seen.insert(row.clone()) {
            self.order.push(row);
        }
    }
}

/// The rows collected in one run, by kind, each kind's rows deduplicated and in the order
/// they came.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SampleSet {
    rows: BTreeMap<Kind, Rows>,
}

impl SampleSet {
    /// Adds `row` to `kind`; a row already there is not added twice.
    pub fn push(&mut self, kind: Kind, row: String) {
        self.rows.entry(kind).or_default().push(row);
    }

    /// `kind`'s rows, in the order they came.
    pub fn rows(&self, kind: Kind) -> impl Iterator<Item = &str> {
        self.rows
            .get(&kind)
            .into_iter()
            .flat_map(|rows| rows.order.iter().map(String::as_str))
    }

    /// How many rows `kind` has.
    pub fn len(&self, kind: Kind) -> usize {
        self.rows.get(&kind).map_or(0, |rows| rows.order.len())
    }

    /// Whether no kind has a row.
    pub fn is_empty(&self) -> bool {
        self.rows.values().all(|rows| rows.order.is_empty())
    }

    /// Writes every kind that has rows to its file under `dir`, created if missing, merged
    /// with the file's existing rows; a kind without rows leaves its file alone.
    ///
    /// # Errors
    ///
    /// [`CsvError`] if the directory cannot be made, a file cannot be read or written, or an
    /// existing file's header is not the one this kind writes.
    pub fn write_merged(&self, dir: &Path) -> Result<(), CsvError> {
        fs::create_dir_all(dir).map_err(|source| CsvError::Io {
            what: "create",
            path: dir.display().to_string(),
            source,
        })?;
        for kind in Kind::ALL {
            if self.len(kind) == 0 {
                continue;
            }
            let path = dir.join(kind.file_name());
            let name = path.display().to_string();
            let existing = match fs::read_to_string(&path) {
                Ok(text) => Some(text),
                Err(err) if err.kind() == io::ErrorKind::NotFound => None,
                Err(source) => {
                    return Err(CsvError::Io {
                        what: "read",
                        path: name,
                        source,
                    });
                }
            };
            let text =
                merge(kind.header(), existing.as_deref(), self.rows(kind)).map_err(|found| {
                    CsvError::Header {
                        path: name.clone(),
                        found,
                    }
                })?;
            fs::write(&path, text).map_err(|source| CsvError::Io {
                what: "write",
                path: name,
                source,
            })?;
        }
        Ok(())
    }
}

/// The text of one file: `header`, then the union of `existing`'s rows and `rows`, each
/// once, sorted by their first column with ties in the order they came (the file's first),
/// each line newline-terminated.
///
/// `existing` is a file's current text, whose first line must be `header`; an empty text
/// is an empty file. Blank lines in it are ignored.
///
/// # Errors
///
/// The header `existing` has instead, if it is not `header`.
pub fn merge<'a>(
    header: &str,
    existing: Option<&'a str>,
    rows: impl IntoIterator<Item = &'a str>,
) -> Result<String, String> {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut all: Vec<&str> = Vec::new();
    if let Some(text) = existing.filter(|text| !text.is_empty()) {
        let mut lines = text.lines();
        let found = lines.next().unwrap_or_default();
        if found != header {
            return Err(found.to_owned());
        }
        for row in lines.filter(|line| !line.is_empty()) {
            if seen.insert(row) {
                all.push(row);
            }
        }
    }
    for row in rows {
        if seen.insert(row) {
            all.push(row);
        }
    }
    // Stable: rows with the same first column keep their order.
    all.sort_by_key(|row| first_column(row));
    let mut text = String::with_capacity(header.len() + 1);
    text.push_str(header);
    text.push('\n');
    for row in all {
        text.push_str(row);
        text.push('\n');
    }
    Ok(text)
}

/// `row`'s first column: everything before its first comma.
fn first_column(row: &str) -> &str {
    row.split(',').next().unwrap_or_default()
}

/// Why the files could not be written.
#[derive(Debug)]
pub enum CsvError {
    /// The directory or a file could not be created, read or written.
    Io {
        /// What was being done: `create`, `read` or `write`.
        what: &'static str,
        /// The path.
        path: String,
        /// The failure.
        source: io::Error,
    },
    /// An existing file starts with a header this kind does not write.
    Header {
        /// The path.
        path: String,
        /// Its first line.
        found: String,
    },
}

impl fmt::Display for CsvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CsvError::Io { what, path, source } => write!(f, "cannot {what} {path}: {source}"),
            CsvError::Header { path, found } => {
                write!(
                    f,
                    "{path} is not one of these files: it starts with {found:?}"
                )
            }
        }
    }
}

impl std::error::Error for CsvError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CsvError::Io { source, .. } => Some(source),
            CsvError::Header { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use junk_colmi::wire::WorkoutField;
    use junk_colmi::{HrSample, SleepSession, SleepStage, StepBucket, StressSample};
    use junk_core::{Duration, Timestamp};

    const HEADER: &str = "at,value";
    const A: &str = "2026-07-02T00:00:00.000-04:00,43";
    const B: &str = "2026-07-02T00:30:00.000-04:00,39";
    const C: &str = "2026-07-02T01:00:00.000-04:00,36";
    /// Same stamp as `C`, different value: a tie on the first column.
    const C2: &str = "2026-07-02T01:00:00.000-04:00,9";

    /// 2026-07-02 00:00 in UTC-4: the `QRing` fixture day.
    const DAY: Timestamp = Timestamp {
        local_minute: 1_782_950_400 / 60,
        utc_offset_min: -240,
    };

    fn at(minute: i64) -> Timestamp {
        Timestamp {
            local_minute: DAY.local_minute + minute,
            ..DAY
        }
    }

    #[test]
    fn samples_become_rows_of_their_own_file() {
        let samples = Samples {
            hr: vec![
                HrSample {
                    at: at(5),
                    bpm: Bpm::Valid(80),
                    source: Source::Periodic,
                },
                HrSample {
                    at: at(0),
                    bpm: Bpm::Valid(63),
                    source: Source::Manual,
                },
                HrSample {
                    at: at(10),
                    bpm: Bpm::Invalid,
                    source: Source::Live,
                },
            ],
            steps: vec![StepBucket {
                start: at(60),
                span: Duration::from_secs(3600),
                steps: 28,
                cal: 1120,
                distance_m: 19,
            }],
            stress: vec![StressSample {
                at: at(0),
                value: 43,
            }],
            sleep: vec![SleepSession {
                start: at(3 * 60 + 7),
                end: at(11 * 60 + 49),
                stages: vec![
                    SleepStage {
                        kind: SleepKind::Light,
                        minutes: 24,
                    },
                    SleepStage {
                        kind: SleepKind::Unknown(9),
                        minutes: 1,
                    },
                ],
            }],
            ..Samples::default()
        };
        let set = rows(&samples).unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(
            set.rows(Kind::Hr).collect::<Vec<_>>(),
            [
                "2026-07-02T00:05:00.000-04:00,80,periodic",
                "2026-07-02T00:00:00.000-04:00,63,manual",
                "2026-07-02T00:10:00.000-04:00,,live"
            ],
            "in the order the ring gave them, an invalid reading with an empty bpm; the files sort"
        );
        assert_eq!(
            set.rows(Kind::Steps).collect::<Vec<_>>(),
            ["2026-07-02T01:00:00.000-04:00,3600,28,1120,19"]
        );
        assert_eq!(
            set.rows(Kind::Sleep).collect::<Vec<_>>(),
            [
                "2026-07-02T03:07:00.000-04:00,2026-07-02T11:49:00.000-04:00,0,light,24",
                "2026-07-02T03:07:00.000-04:00,2026-07-02T11:49:00.000-04:00,1,unknown-9,1"
            ]
        );
        assert_eq!(set.rows(Kind::Stress).count(), 1);
        assert_eq!(set.rows(Kind::Spo2).count(), 0, "a kind with no samples");

        assert!(
            rows(&Samples::default())
                .unwrap_or_else(|err| panic!("{err}"))
                .is_empty()
        );
    }

    #[test]
    fn a_workout_row_is_the_raw_numbers_with_gaps_for_missing_tags() {
        let record = WorkoutRecord {
            sport_type: 7,
            fields: vec![
                WorkoutField {
                    tag: WorkoutTag::StartTime,
                    value: 0x691d_2980u32.to_le_bytes().to_vec(),
                },
                WorkoutField {
                    tag: WorkoutTag::Duration,
                    value: 61u16.to_le_bytes().to_vec(),
                },
                WorkoutField {
                    tag: WorkoutTag::RateAvg,
                    value: vec![60],
                },
            ],
        };
        assert_eq!(workout_row(&record), "1763518848,7,61,60,,,,,");
    }

    #[test]
    fn merge_dedupes_and_sorts_and_writes_the_header_once() {
        let fresh = merge(HEADER, None, [C, A, A]).unwrap_or_else(|found| panic!("{found}"));
        assert_eq!(
            fresh,
            "at,value\n2026-07-02T00:00:00.000-04:00,43\n2026-07-02T01:00:00.000-04:00,36\n"
        );

        // Merging into what was written keeps every row once, sorted, one header.
        let again = merge(HEADER, Some(&fresh), [B, A]).unwrap_or_else(|found| panic!("{found}"));
        assert_eq!(again, format!("{HEADER}\n{A}\n{B}\n{C}\n"));
        assert_eq!(
            merge(HEADER, Some(&again), [A, B, C]),
            Ok(again.clone()),
            "idempotent"
        );

        // An empty file and blank lines are tolerated; a foreign header is not.
        assert_eq!(merge(HEADER, Some(""), [A]), Ok(format!("{HEADER}\n{A}\n")));
        assert_eq!(
            merge(HEADER, Some("at,value\n\n"), [A]),
            Ok(format!("{HEADER}\n{A}\n"))
        );
        assert_eq!(merge(HEADER, None, []), Ok(format!("{HEADER}\n")));
        assert_eq!(
            merge(HEADER, Some("time,bpm\n1,2\n"), [A]),
            Err("time,bpm".to_owned())
        );
    }

    #[test]
    fn ties_on_the_first_column_keep_their_order() {
        // `C2` sorts before `C` as text but comes after it here, and stays after it.
        assert_eq!(
            merge(HEADER, None, [C, C2, A]),
            Ok(format!("{HEADER}\n{A}\n{C}\n{C2}\n"))
        );
        // The file's order wins over a new run's.
        let file = format!("{HEADER}\n{C2}\n{C}\n");
        assert_eq!(
            merge(HEADER, Some(&file), [C, C2]),
            Ok(format!("{HEADER}\n{C2}\n{C}\n"))
        );
        assert_eq!(first_column(A), "2026-07-02T00:00:00.000-04:00");
        assert_eq!(first_column(""), "");
    }

    #[test]
    fn a_set_keeps_each_row_once_per_kind() {
        let mut set = SampleSet::default();
        assert!(set.is_empty());
        set.push(Kind::Stress, C.to_owned());
        set.push(Kind::Stress, A.to_owned());
        set.push(Kind::Stress, A.to_owned());
        set.push(Kind::Hrv, A.to_owned());
        assert!(!set.is_empty());
        assert_eq!(set.len(Kind::Stress), 2);
        assert_eq!(set.len(Kind::Hrv), 1);
        assert_eq!(set.len(Kind::Hr), 0);
        assert_eq!(
            set.rows(Kind::Stress).collect::<Vec<_>>(),
            [C, A],
            "in the order they came"
        );
        assert_eq!(set.rows(Kind::Hr).count(), 0);
    }

    #[test]
    fn write_merged_creates_the_directory_and_is_idempotent() {
        let dir = std::env::temp_dir().join(format!("junk-cli-csv-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);

        let mut set = SampleSet::default();
        set.push(Kind::Stress, C.to_owned());
        set.push(Kind::Stress, A.to_owned());
        set.write_merged(&dir).unwrap_or_else(|err| panic!("{err}"));
        let stress = dir.join("stress.csv");
        let first = fs::read_to_string(&stress).unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(first, format!("{HEADER}\n{A}\n{C}\n"));
        assert!(
            !dir.join("hrv.csv").exists(),
            "a kind without rows has no file"
        );

        let mut more = SampleSet::default();
        more.push(Kind::Stress, B.to_owned());
        more.push(Kind::Stress, A.to_owned());
        more.write_merged(&dir)
            .unwrap_or_else(|err| panic!("{err}"));
        let second = fs::read_to_string(&stress).unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(second, format!("{HEADER}\n{A}\n{B}\n{C}\n"));
        more.write_merged(&dir)
            .unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(
            fs::read_to_string(&stress).unwrap_or_else(|err| panic!("{err}")),
            second
        );

        fs::write(&stress, "time,bpm\n").unwrap_or_else(|err| panic!("{err}"));
        let err = more.write_merged(&dir).unwrap_err();
        assert!(matches!(err, CsvError::Header { .. }), "{err}");
        assert!(err.to_string().contains("time,bpm"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_kind_has_a_distinct_file_and_a_stamp_first() {
        let names: BTreeSet<&str> = Kind::ALL.iter().map(|kind| kind.file_name()).collect();
        assert_eq!(names.len(), Kind::ALL.len());
        for kind in Kind::ALL {
            assert_eq!(
                Path::new(kind.file_name())
                    .extension()
                    .and_then(|ext| ext.to_str()),
                Some("csv")
            );
            assert!(!kind.header().contains(' '));
        }
    }
}
