//! `junk`: the command-line shell of the junk protocol stack (SPEC §4), for a Colmi ring
//! and a Sony WH-1000XM4.
//!
//! - `junk scan` lists the rings in range.
//! - `junk sync` connects to one, runs the app's session, writes the logs as CSV and can
//!   record the whole exchange as a trace (SPEC §5, Stage B).
//! - `junk live` streams a workout's heart rate and fetches the stored record (Stage C).
//! - `junk replay` drives the driver from a captured trace with no hardware, the path the
//!   tests take (Stage A).
//! - `junk sony status` connects to a Sony WH-1000XM4 over Bluetooth Classic, reads every
//!   setting it lists and prints them; `junk sony replay` does the same from a trace.
//!
//! Answers and samples go to stdout; what the device says on its own, and every failure, to
//! stderr.
//!
//! Failures are printed as one line. The stack's error types already put their cause in
//! their text, so they are turned into messages at the boundary ([`flat`]) rather than
//! kept as chains, which would say everything twice.
#![warn(missing_docs)]

mod ble;
mod csv;
mod record;
mod session;
mod sony;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow};
use clap::{Args, Parser, Subcommand};
use junk_app::{Clock, Progress, Samples};
use junk_ble::{BleConfig, BleLink, DEFAULT_SCAN_TIMEOUT, Found};
use junk_colmi::{ColmiAdapter, ColmiDriver};
use junk_trace::{Line, Stamp, Trace};
use sony::{SonyArgs, SonyCommand};

/// The UTC offset the `QRing` fixture was captured in (a UTC-4 zone in summer), for traces
/// whose stamps carry none.
const DEFAULT_TZ_MIN: i16 = -240;

/// The junk protocol stack's command line, for a Colmi ring and a Sony WH-1000XM4.
#[derive(Parser)]
#[command(name = "junk", version)]
struct Cli {
    /// What to do.
    #[command(subcommand)]
    command: Command,
}

/// The commands.
#[derive(Subcommand)]
enum Command {
    /// Lists the rings in range: name, id, RSSI.
    Scan(ScanArgs),
    /// Connects to a ring, runs the app's session, writes CSVs, optionally records a trace.
    Sync(SyncArgs),
    /// Starts a workout on the ring, prints its heart rate live, then fetches the record.
    Live(LiveArgs),
    /// Replays a captured trace through the driver, offline, and reports what it did.
    Replay(ReplayArgs),
    /// Connects to a ring just to list every characteristic it has, then disconnects.
    Gatt(ConnectArgs),
    /// Reads everything a Sony WH-1000XM4 reports, from the headphones or from a trace.
    Sony(SonyArgs),
}

/// `junk scan`.
#[derive(Args)]
struct ScanArgs {
    /// How long to listen, in seconds.
    #[arg(long, default_value_t = DEFAULT_SCAN_TIMEOUT.as_secs())]
    timeout: u64,
}

/// How to find and open the ring; shared by `sync` and `live`.
#[derive(Args)]
struct ConnectArgs {
    /// The ring: a substring of its name (case aside) or its exact id. Without it, the one
    /// ring found.
    #[arg(long)]
    device: Option<String>,
    /// How long to scan for it, in seconds.
    #[arg(long, default_value_t = DEFAULT_SCAN_TIMEOUT.as_secs())]
    timeout: u64,
    /// The ATT MTU to assume when the platform does not report the negotiated one.
    #[arg(long)]
    mtu: Option<u16>,
}

impl ConnectArgs {
    /// The link and scan configuration these arguments ask for.
    fn config(&self) -> BleConfig {
        let defaults = BleConfig::default();
        BleConfig {
            assumed_mtu: self.mtu.unwrap_or(defaults.assumed_mtu),
            scan_timeout: Duration::from_secs(self.timeout),
        }
    }
}

/// `junk sync`.
#[derive(Args)]
struct SyncArgs {
    /// How to find the ring.
    #[command(flatten)]
    connect: ConnectArgs,
    /// How many days of logs to read, today first.
    #[arg(long, default_value_t = 7)]
    days: u8,
    /// Where to write the CSVs, merged with what is already there.
    #[arg(long)]
    csv: Option<PathBuf>,
    /// Where to write a trace of the whole session.
    #[arg(long)]
    record: Option<PathBuf>,
}

/// `junk live`.
#[derive(Args)]
struct LiveArgs {
    /// How to find the ring.
    #[command(flatten)]
    connect: ConnectArgs,
    /// The sport type, as the app numbers them.
    #[arg(long, default_value_t = 7)]
    sport: u8,
    /// How long to stream before stopping, in seconds; Ctrl-C stops earlier.
    #[arg(long, default_value_t = 60)]
    seconds: u64,
    /// Where to write a trace of the whole session.
    #[arg(long)]
    record: Option<PathBuf>,
}

/// `junk replay`.
#[derive(Args)]
struct ReplayArgs {
    /// The trace to replay.
    trace: PathBuf,
    /// The UTC offset, in minutes, for a trace whose stamps carry none.
    #[arg(long, default_value_t = DEFAULT_TZ_MIN, allow_negative_numbers = true)]
    tz: i16,
    /// Where to write the CSVs of every answer, merged with what is already there.
    #[arg(long)]
    csv: Option<PathBuf>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        // `IOBluetooth` delivers an RFCOMM channel through the main run loop, so this one
        // keeps the main thread (see `junk-rfcomm`); every other command runs on tokio's.
        Command::Sony(SonyArgs {
            command: SonyCommand::Status(args),
        }) => sony::status_on_main_loop(args),
        command => run(command),
    };
    match result {
        Ok(code) => code,
        Err(err) => {
            eprintln!("junk: {err:#}");
            ExitCode::FAILURE
        }
    }
}

/// Runs `command` on a multi-threaded tokio runtime, as `#[tokio::main]` would.
fn run(command: Command) -> anyhow::Result<ExitCode> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("cannot start the async runtime")?;
    runtime.block_on(async {
        match command {
            Command::Scan(args) => scan(&args).await,
            Command::Sync(args) => sync(&args).await,
            Command::Live(args) => live(&args).await,
            Command::Replay(args) => replay(&args),
            Command::Gatt(args) => gatt(&args).await,
            Command::Sony(SonyArgs {
                command: SonyCommand::Replay(args),
            }) => sony::replay(&args).await,
            Command::Sony(SonyArgs {
                command: SonyCommand::Status(_),
            }) => unreachable!("`sony status` takes the main-loop path"),
        }
    })
}

/// `err` as a message, its cause included by its own text and not repeated as a chain.
pub fn flat(err: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("{err}")
}

/// `junk scan`: one line per ring.
async fn scan(args: &ScanArgs) -> anyhow::Result<ExitCode> {
    let adapter = junk_ble::adapter().await.map_err(flat)?;
    let config = BleConfig {
        scan_timeout: Duration::from_secs(args.timeout),
        ..BleConfig::default()
    };
    let found = ble::scan(&adapter, &config).await?;
    if found.is_empty() {
        eprintln!("no ring found in {} s", args.timeout);
    }
    for peripheral in &found {
        println!("{}", junk_ble::describe(peripheral));
    }
    Ok(ExitCode::SUCCESS)
}

/// `junk gatt`: every characteristic of the ring, one per line, with what it supports,
/// and the values of the standard Device Information service, where the version strings
/// live.
async fn gatt(args: &ConnectArgs) -> anyhow::Result<ExitCode> {
    let config = args.config();
    let adapter = junk_ble::adapter().await.map_err(flat)?;
    let found = ble::choose(ble::scan(&adapter, &config).await?, args.device.as_deref())?;
    eprintln!("discovering {}", junk_ble::describe(&found));
    let dis = junk_core::Uuid::from_u128(0x0000_180a_0000_1000_8000_0080_5f9b_34fb);
    let inspection = junk_ble::inspect(&adapter, &found.id, &[dis])
        .await
        .map_err(flat)?;
    for info in &inspection.chars {
        let mut props = Vec::new();
        if info.writable {
            props.push("write");
        }
        if info.writable_without_response {
            props.push("write-no-rsp");
        }
        if info.notify {
            props.push("notify");
        }
        if info.indicate {
            props.push("indicate");
        }
        if info.readable {
            props.push("read");
        }
        println!("{}  {}  {}", info.service, info.uuid, props.join(","));
    }
    for (service, uuid, value) in &inspection.values {
        let text: String = value
            .iter()
            .map(|&b| {
                if (0x20..=0x7e).contains(&b) {
                    char::from(b)
                } else {
                    '.'
                }
            })
            .collect();
        println!(
            "{service}  {uuid}  read  {}  {text:?}",
            junk_app::hex(value)
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// Scans for and opens the ring `args` name.
async fn open(args: &ConnectArgs) -> anyhow::Result<(BleLink, Found)> {
    let config = args.config();
    let adapter = junk_ble::adapter().await.map_err(flat)?;
    let found = ble::choose(ble::scan(&adapter, &config).await?, args.device.as_deref())?;
    eprintln!("connecting to {}", junk_ble::describe(&found));
    let link = BleLink::open(&adapter, &found.id, config)
        .await
        .map_err(flat)?;
    Ok((link, found))
}

/// Writes the trace of a recorded session, then hands its outcome on.
///
/// The trace is written even when the session failed; when both failed, the session's
/// failure is printed here, since only one can be returned.
fn write_record<T>(
    path: &Path,
    command: &str,
    device: &str,
    started: &Stamp,
    lines: Vec<Line>,
    result: anyhow::Result<T>,
) -> anyhow::Result<T> {
    if let Err(err) = record::write_trace(path, command, device, started, lines) {
        if let Err(session_err) = &result {
            eprintln!("junk: {session_err:#}");
        }
        return Err(err);
    }
    eprintln!("recorded {}", path.display());
    result
}

/// `junk sync`.
async fn sync(args: &SyncArgs) -> anyhow::Result<ExitCode> {
    let (link, found) = open(&args.connect).await?;
    let clock = Clock::now();
    // Ctrl-C stops between requests: the session ends tidily and keeps what it collected.
    let until = interrupted();
    let (result, session) = match args.record.as_deref() {
        None => {
            let run = junk_app::sync(link, clock, args.days, until, |progress| {
                session::print(&progress);
            })
            .await;
            (run.result, run.session)
        }
        Some(path) => {
            let started = clock.stamp()?;
            let link = record::recording(link, junk_colmi::channel_name)?;
            let run = junk_app::sync(link, clock, args.days, until, |progress| {
                session::print(&progress);
            })
            .await;
            let (_link, lines) = run.link.into_inner();
            let result = write_record(
                path,
                "sync",
                junk_ble::name_of(&found),
                &started,
                lines,
                run.result,
            );
            (result, run.session)
        }
    };
    // Whatever was collected is written, even when the session did not get to the end.
    if let Some(dir) = &args.csv {
        let rows = csv::rows(&session.samples).map_err(flat)?;
        if !rows.is_empty() {
            rows.write_merged(dir).map_err(flat)?;
            eprintln!("wrote CSVs under {}", dir.display());
        }
    }
    result.map(|()| ExitCode::SUCCESS)
}

/// `junk live`.
async fn live(args: &LiveArgs) -> anyhow::Result<ExitCode> {
    let (link, found) = open(&args.connect).await?;
    let clock = Clock::now();
    let started = Instant::now();
    // The stream's samples are printed as `<seconds since start> <bpm>`, so the observer
    // keeps the sequence number it last showed.
    let mut last_seq = None;
    let observe = |progress: Progress<'_>| session::print_live(&progress, started, &mut last_seq);
    // Ctrl-C stops the stream early; the script still stops the workout and fetches it.
    let until = interrupted();
    let result = match args.record.as_deref() {
        None => {
            junk_app::live(link, args.sport, args.seconds, until, observe)
                .await
                .result
        }
        Some(path) => {
            let stamp = clock.stamp()?;
            let link = record::recording(link, junk_colmi::channel_name)?;
            let run = junk_app::live(link, args.sport, args.seconds, until, observe).await;
            let (_link, lines) = run.link.into_inner();
            write_record(
                path,
                "live",
                junk_ble::name_of(&found),
                &stamp,
                lines,
                run.result,
            )
        }
    };
    result.map(|()| ExitCode::SUCCESS)
}

/// A future that finishes on Ctrl-C: what a script takes to stop early.
async fn interrupted() {
    let _ = tokio::signal::ctrl_c().await;
}

/// `junk replay`: Stage A with no hardware.
fn replay(args: &ReplayArgs) -> anyhow::Result<ExitCode> {
    let path = &args.trace;
    let text =
        fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let trace = Trace::parse(&text).map_err(|err| anyhow!("{}: {err}", path.display()))?;
    let adapter = ColmiAdapter::for_trace(&trace, args.tz)
        .with_context(|| format!("{}: no data lines to replay", path.display()))?;
    let mut driver = ColmiDriver::new();
    let out = junk_trace::replay(&trace, &adapter, &mut driver);

    let mut failed = false;
    print!(
        "writes: {} expected, {} written, ",
        out.expected.len(),
        out.written.len()
    );
    match out.writes_match() {
        Ok(()) => println!("match"),
        Err(mismatch) => {
            println!("mismatch");
            println!("{mismatch}");
            failed = true;
        }
    }
    let mut samples = Samples::default();
    let (mut ok, mut err) = (0, 0);
    for (index, answer) in out.answers.iter().enumerate() {
        match &answer.result {
            Ok(resp) => {
                ok += 1;
                samples.absorb(resp);
            }
            Err(error) => {
                err += 1;
                let line = answer
                    .line
                    .map_or_else(|| "no request".to_owned(), |line| format!("line {line}"));
                eprintln!("answer {index} ({line}): {error}");
            }
        }
    }
    let rows = csv::rows(&samples).context("a sample's time")?;
    println!("answers: {}, ok {ok}, err {err}", out.answers.len());
    println!("skipped: {}", out.skipped.len());
    println!("unknown channels: {}", out.unknown_channels.len());
    println!("timers fired: {}", out.timers_fired.len());
    let mut tally: BTreeMap<&str, usize> = BTreeMap::new();
    for event in &out.events {
        *tally.entry(session::event_kind(event)).or_default() += 1;
    }
    let mut kinds = String::new();
    for (kind, count) in &tally {
        let _ = write!(
            kinds,
            "{}{kind} {count}",
            if kinds.is_empty() { " (" } else { ", " }
        );
    }
    if !kinds.is_empty() {
        kinds.push(')');
    }
    println!("events: {}{kinds}", out.events.len());
    if err > 0 {
        failed = true;
    }

    if let Some(dir) = &args.csv {
        rows.write_merged(dir).map_err(flat)?;
    }
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}
