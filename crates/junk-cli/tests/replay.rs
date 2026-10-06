//! The offline path end to end: `junk replay` on both fixtures under `fixtures/colmi-r10`,
//! run as the built binary, and the CSVs it writes.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/colmi-r10");
const QRING: &str = "qring-sync-2026-07-02.trace";
const THERING: &str = "thering-realtime-2025-11-19.trace";

/// A fresh directory under the system temp dir, removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let dir = std::env::temp_dir().join(format!("junk-cli-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir");
        TempDir(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn arg(&self) -> String {
        self.0.display().to_string()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Runs `junk` with `args`.
fn junk(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_junk"))
        .args(args)
        .output()
        .expect("the binary runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn fixture(name: &str) -> String {
    format!("{FIXTURES}/{name}")
}

/// The lines of `file` under `dir`: the header, then the rows.
fn lines(dir: &Path, file: &str) -> Vec<String> {
    let path = dir.join(file);
    let text = fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
    assert!(text.ends_with('\n'), "{file} ends with a newline");
    text.lines().map(str::to_owned).collect()
}

/// `file`'s rows, header dropped.
fn rows(dir: &Path, file: &str) -> Vec<String> {
    let mut lines = lines(dir, file);
    lines.remove(0);
    lines
}

/// Every CSV under `dir`, name and content.
fn csvs(dir: &Path) -> Vec<(String, String)> {
    let mut files: Vec<(String, String)> = fs::read_dir(dir)
        .expect("readable")
        .map(|entry| {
            let entry = entry.expect("entry");
            let name = entry.file_name().to_string_lossy().into_owned();
            let text = fs::read_to_string(entry.path()).expect("readable");
            (name, text)
        })
        .collect();
    files.sort();
    files
}

fn replay_qring(dir: &TempDir) -> Output {
    junk(&[
        "replay",
        &fixture(QRING),
        "--tz",
        "-240",
        "--csv",
        &dir.arg(),
    ])
}

#[test]
fn qring_replay_matches_and_writes_the_csvs() {
    let dir = TempDir::new("qring");
    let output = replay_qring(&dir);
    let out = stdout(&output);
    assert!(output.status.success(), "{out}\n{}", stderr(&output));
    assert!(out.contains("118 expected, 118 written, match"), "{out}");
    assert!(out.contains("answers: 118, ok 118, err 0"), "{out}");

    let hr = lines(dir.path(), "hr.csv");
    assert_eq!(hr[0], "at,bpm,source");
    assert_eq!(hr[1], "2026-07-02T00:00:00.000-04:00,63,periodic");
    assert_eq!(hr[2], "2026-07-02T00:05:00.000-04:00,80,periodic");
    assert_eq!(hr[3], "2026-07-02T00:10:00.000-04:00,74,periodic");
    let stamps: Vec<&str> = hr[1..]
        .iter()
        .map(|row| row.split(',').next().expect("a stamp"))
        .collect();
    assert!(
        stamps.iter().all(|stamp| stamp.ends_with("-04:00")),
        "{stamps:?}"
    );
    let mut sorted = stamps.clone();
    sorted.sort_unstable();
    assert_eq!(stamps, sorted, "sorted by stamp");
    assert_eq!(hr.len() - 1, 177, "through 14:35, each minute once");

    let sleep = rows(dir.path(), "sleep.csv");
    assert_eq!(sleep.len(), 21);
    assert_eq!(
        sleep[0],
        "2026-07-02T03:07:00.000-04:00,2026-07-02T11:49:00.000-04:00,0,light,24"
    );
    assert_eq!(
        sleep[20],
        "2026-07-02T03:07:00.000-04:00,2026-07-02T11:49:00.000-04:00,20,light,49"
    );
    assert_eq!(
        lines(dir.path(), "sleep.csv")[0],
        "start,end,index,kind,minutes"
    );

    let steps = rows(dir.path(), "steps.csv");
    assert!(
        steps.contains(&"2026-07-02T01:00:00.000-04:00,3600,28,1120,19".to_owned()),
        "{steps:?}"
    );

    assert_eq!(
        rows(dir.path(), "spo2.csv")[0],
        "2026-07-02T00:00:00.000-04:00,98"
    );
    assert_eq!(
        rows(dir.path(), "temperature.csv")[0],
        "2026-07-02T00:00:00.000-04:00,367"
    );
    assert_eq!(
        rows(dir.path(), "stress.csv")[0],
        "2026-07-02T00:00:00.000-04:00,43"
    );

    let hrv = rows(dir.path(), "hrv.csv");
    assert_eq!(hrv[0], "2026-07-02T00:00:00.000-04:00,30");
    assert_eq!(hrv[1], "2026-07-02T01:00:00.000-04:00,43");
    assert!(
        !hrv.iter().any(|row| row.starts_with("2026-07-02T00:30")),
        "{hrv:?}"
    );

    assert!(
        !dir.path().join("workouts.csv").exists(),
        "no records: no file"
    );
}

#[test]
fn replaying_twice_leaves_every_csv_byte_identical() {
    let dir = TempDir::new("twice");
    assert!(replay_qring(&dir).status.success());
    let before = csvs(dir.path());
    assert_eq!(before.len(), 7);
    assert!(replay_qring(&dir).status.success());
    assert_eq!(csvs(dir.path()), before);
}

#[test]
fn thering_replay_writes_the_workout() {
    let dir = TempDir::new("thering");
    let output = junk(&["replay", &fixture(THERING), "--csv", &dir.arg()]);
    let out = stdout(&output);
    assert!(output.status.success(), "{out}\n{}", stderr(&output));
    assert!(out.contains("5 expected, 5 written, match"), "{out}");
    assert!(out.contains("answers: 5, ok 5, err 0"), "{out}");
    assert!(out.contains("Workout 70"), "{out}");

    let workouts = lines(dir.path(), "workouts.csv");
    assert_eq!(
        workouts[0],
        "start_ring,sport_type,duration_s,rate_avg,rate_min,rate_max,steps,distance,calories"
    );
    assert_eq!(workouts.len(), 2);
    let fields: Vec<&str> = workouts[1].split(',').collect();
    assert_eq!(fields[1], "7", "sport_type");
    assert_eq!(fields[2], "61", "duration_s");
    assert_eq!(
        fields[0], "1763518848",
        "the 77 01 ack's timestamp, 0x691d2980"
    );
    assert_eq!(csvs(dir.path()).len(), 1, "only the kind with rows");
}

#[test]
fn a_file_that_is_not_a_trace_fails_naming_the_line() {
    let dir = TempDir::new("not-a-trace");
    let path = dir.path().join("notes.txt");
    fs::write(&path, "# a header\nhello world\n").expect("written");
    let output = junk(&["replay", &path.display().to_string()]);
    assert!(!output.status.success());
    let err = stderr(&output);
    assert!(err.contains("line 2"), "{err}");
    assert!(err.contains("notes.txt"), "{err}");

    let missing = junk(&[
        "replay",
        &dir.path().join("missing.trace").display().to_string(),
    ]);
    assert!(!missing.status.success());
    assert!(stderr(&missing).contains("missing.trace"));
}

#[test]
fn an_unanswered_request_is_an_error_answer_and_exit_status_one() {
    let dir = TempDir::new("unanswered");
    let path = dir.path().join("battery.trace");
    // The app asks for the battery and the ring never answers: the disconnect at the end
    // of the replay fails the request.
    fs::write(
        &path,
        "# junk trace v1 — one unanswered battery request\n\
         2026-07-02T14:16:37.044 tx v1.write 03010000000000000000000000000004\n",
    )
    .expect("written");
    let output = junk(&["replay", &path.display().to_string()]);
    let out = stdout(&output);
    assert_eq!(output.status.code(), Some(1), "{out}");
    assert!(out.contains("1 expected, 1 written, match"), "{out}");
    assert!(out.contains("answers: 1, ok 0, err 1"), "{out}");
    assert!(
        stderr(&output).contains("answer 0 (line 0)"),
        "{}",
        stderr(&output)
    );
}
