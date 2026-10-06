//! `junk sony`: read everything a Sony WH-1000XM4 reports about itself.
//!
//! `status` opens the headphones over Bluetooth Classic RFCOMM, `replay` runs the very
//! same session offline from a trace. Both drive [`SonyDriver`] through a [`Pump`] over a
//! [`Framed`] link and send one request, [`Req::Status`]: the CLI is read-only and has no
//! way to send anything else (`Req::Raw` is not reachable from here).
//!
//! The headset's own words go to stderr as the session runs, one line each; the report goes
//! to stdout, once, at the end.

use std::cell::Cell;
use std::fmt::{Debug, Display, Write as _};
use std::fs;
use std::future::{Future, pending};
use std::path::PathBuf;
use std::pin::pin;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context as _, anyhow};
use clap::{Args, Subcommand};
use junk_app::{Clock, hex};
use junk_core::{Link, ProtoError};
use junk_pump::framed::Framed;
use junk_pump::trace_link::TraceLink;
use junk_pump::{Pump, PumpConfig, PumpEvent, RequestError, Stop};
use junk_rfcomm::RfcommLink;
use junk_sony::payload::{
    AsmId, AsmStep, AtmosphericPressure, ConnectionOrder, EqBandInformationType,
    FileTransferSupport, GsTitle, GsValue, NcAsmCapability, NcAsmState, PairedDevices,
    PairingCapability, PlaybackHolder,
};
use junk_sony::proto::{DeviceInfo, Ev, Reading, Req, Resp, Status};
use junk_sony::{SonyDriver, SonyFraming, channel_by_name, channel_name};
use junk_trace::Trace;

use crate::{flat, record, write_record};

/// How long a session may take before it is given up, in seconds: init is up to about twenty
/// steps, each with a three-second reply timeout of its own.
const DEFAULT_TIMEOUT: u64 = 40;

/// How long the pump gets to close the link once the session is over.
const CLOSE: Duration = Duration::from_secs(3);

/// `junk sony`.
#[derive(Args)]
pub struct SonyArgs {
    /// What to do.
    #[command(subcommand)]
    pub command: SonyCommand,
}

/// The `junk sony` commands.
#[derive(Subcommand)]
pub enum SonyCommand {
    /// Connects to a WH-1000XM4 over Bluetooth Classic and prints everything it reports.
    ///
    /// Opens the headphones' RFCOMM channel, runs Sony's init, reads every setting the
    /// headset lists (read-only: nothing is ever written to it) and prints the result.
    ///
    /// The headphones must be paired with this Mac and switched on. They should preferably
    /// not be connected to a phone or another app for control: the headset serves one
    /// control session at a time.
    ///
    /// Events the headset sends on its own go to stderr as the session runs. The report
    /// goes to stdout. Ctrl-C ends the session early.
    Status(StatusArgs),
    /// Replays a captured trace through the driver, offline, and prints the same report.
    Replay(ReplayArgs),
}

/// What to print and how; shared by `status` and `replay`.
#[derive(Args)]
pub struct OutputArgs {
    /// Print one JSON document instead of the text report. It always carries the raw init
    /// replies.
    #[arg(long)]
    json: bool,
    /// After the text report, print the raw init and capability replies as hex lines: the
    /// data type byte, then the payload, command id first.
    #[arg(long)]
    raw: bool,
}

/// `junk sony status`.
#[derive(Args)]
pub struct StatusArgs {
    /// The headphones' Bluetooth address, `AA:BB:CC:DD:EE:FF`.
    ///
    /// There is no lookup by name. Find it with `system_profiler SPBluetoothDataType` (the
    /// headphones' entry has an `Address:` line) or in System Settings, Bluetooth, the (i)
    /// button beside the headphones.
    #[arg(long)]
    address: String,
    /// What to print and how.
    #[command(flatten)]
    output: OutputArgs,
    /// Where to write a trace of the whole session, as a real capture of a WH-1000XM4.
    #[arg(long)]
    record: Option<PathBuf>,
    /// The hard limit on the whole session, in seconds.
    #[arg(long, default_value_t = DEFAULT_TIMEOUT)]
    timeout: u64,
}

/// `junk sony replay`.
#[derive(Args)]
pub struct ReplayArgs {
    /// The trace to replay.
    trace: PathBuf,
    /// What to print and how.
    #[command(flatten)]
    output: OutputArgs,
}

/// `junk sony status`, with this thread as the main thread: it keeps turning the main run
/// loop, which `IOBluetooth` delivers an RFCOMM channel through, while the session runs on
/// a second thread with a runtime of its own (see the `junk-rfcomm` docs).
pub fn status_on_main_loop(args: StatusArgs) -> anyhow::Result<ExitCode> {
    let worker = std::thread::Builder::new()
        .name("sony-status".to_owned())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("cannot start the async runtime")?;
            runtime.block_on(status(&args))
        })
        .context("cannot start the session thread")?;
    junk_rfcomm::turn_main_loop(|| worker.is_finished());
    worker
        .join()
        .map_err(|_| anyhow!("the session thread panicked"))?
}

/// `junk sony status`.
async fn status(args: &StatusArgs) -> anyhow::Result<ExitCode> {
    let link = RfcommLink::open(&args.address).map_err(flat)?;
    eprintln!("connecting to {}", link.address());
    let limit = Duration::from_secs(args.timeout);
    let status = match args.record.as_deref() {
        None => drive(link, limit, interrupted()).await.1?,
        Some(path) => {
            let started = Clock::now().stamp()?;
            let link = record::recording(link, channel_name)?;
            let (link, result) = drive(link, limit, interrupted()).await;
            let (_link, lines) = link.into_inner();
            write_record(
                path,
                "sony status",
                "Sony WH-1000XM4, real capture",
                &started,
                lines,
                result,
            )?
        }
    };
    print_report(&status, &args.output)?;
    Ok(ExitCode::SUCCESS)
}

/// `junk sony replay`.
pub async fn replay(args: &ReplayArgs) -> anyhow::Result<ExitCode> {
    let path = &args.trace;
    let text =
        fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let trace = Trace::parse(&text).map_err(|err| anyhow!("{}: {err}", path.display()))?;
    if trace.data().next().is_none() {
        return Err(anyhow!("{}: no data lines to replay", path.display()));
    }
    let link = TraceLink::new(&trace, channel_by_name);
    if link.unknown_channels() > 0 {
        return Err(anyhow!(
            "{}: not a Sony trace: it names a channel other than rfcomm",
            path.display()
        ));
    }
    let limit = Duration::from_secs(DEFAULT_TIMEOUT);
    let (_link, result) = drive(link, limit, pending()).await;
    print_report(&*result?, &args.output)?;
    Ok(ExitCode::SUCCESS)
}

/// A future that finishes on Ctrl-C.
async fn interrupted() {
    let _ = tokio::signal::ctrl_c().await;
}

/// Runs init and one [`Req::Status`] over `link`, showing the headset's events on stderr,
/// and gives the link back with the outcome.
///
/// The session ends with a status, with the headset dropping the link, with `until`, or
/// with `limit`, whichever comes first; then the pump is shut down so the link is closed.
async fn drive<L: Link>(
    link: L,
    limit: Duration,
    until: impl Future<Output = ()>,
) -> (L, anyhow::Result<Box<Status>>) {
    let (mut pump, handle, mut events) = Pump::new(
        SonyDriver::new(),
        Framed::new(link, SonyFraming),
        PumpConfig::default(),
    );
    // The version of a headset whose init the driver refused, for the failure's message.
    let refused = Cell::new(None);
    let mut ended = false;
    let result = {
        let mut run = pin!(pump.run());
        let script = async {
            let mut request = pin!(handle.request(Req::Status));
            loop {
                tokio::select! {
                    Some(event) = events.recv() => show(&event, &refused),
                    answer = &mut request => break answer,
                }
            }
        };
        let result = tokio::select! {
            stopped = &mut run => {
                ended = true;
                Err(match stopped {
                    Ok(Stop::Disconnected | Stop::Shutdown) => {
                        anyhow!("the headset closed the connection before answering")
                    }
                    Err(err) => flat(err),
                })
            }
            answer = script => match answer {
                Ok(Resp::Status(status)) => Ok(status),
                Ok(Resp::Raw(_)) => Err(anyhow!("the driver answered a status with a raw reply")),
                Err(err) => Err(failure(&err, refused.get())),
            },
            () = until => Err(anyhow!("interrupted")),
            () = tokio::time::sleep(limit) => Err(anyhow!(
                "no status after {} s: is the headset on, in range, and free of another controller?",
                limit.as_secs()
            )),
        };
        handle.shutdown();
        if !ended {
            let _ = tokio::time::timeout(CLOSE, &mut run).await;
        }
        result
    };
    while let Ok(event) = events.try_recv() {
        show(&event, &refused);
    }
    let (_driver, framed) = pump.into_parts();
    (framed.into_inner(), result)
}

/// Why a request got no answer, as one line; `refused` is the protocol version the driver
/// turned down, if it did.
fn failure(err: &RequestError, refused: Option<u16>) -> anyhow::Error {
    match (err, refused) {
        (RequestError::Proto(ProtoError::Unsupported(_)), Some(version)) => anyhow!(
            "unsupported protocol version {version:#06x}: the driver does not talk to this headset"
        ),
        _ => flat(err),
    }
}

/// One line on stderr for something the pump or the headset said on its own.
fn show(event: &PumpEvent<Ev>, refused: &Cell<Option<u16>>) {
    match event {
        PumpEvent::Connected { mtu, .. } => eprintln!("connected: mtu {mtu}"),
        PumpEvent::Disconnected => eprintln!("disconnected"),
        PumpEvent::LinkError(err) => eprintln!("link error: {err}"),
        PumpEvent::Event(Ev::ProtocolVersion { version, supported }) => {
            if !supported {
                refused.set(Some(*version));
            }
            eprintln!(
                "protocol version {version:#06x} ({})",
                if *supported {
                    "supported"
                } else {
                    "not supported"
                }
            );
        }
        PumpEvent::Event(Ev::Report(report)) => eprintln!("report: {report:?}"),
        PumpEvent::Event(Ev::Dropped { error, bytes }) => {
            eprintln!("dropped frame ({error}): {}", hex(bytes));
        }
        PumpEvent::Event(Ev::Unparsed { bytes }) => eprintln!("unparsed frame: {}", hex(bytes)),
        PumpEvent::Event(Ev::MissingChannels(channels)) => {
            eprintln!("the link lacks the byte stream: {channels:?}");
        }
    }
}

/// Prints `status` on stdout, as JSON or text.
fn print_report(status: &Status, output: &OutputArgs) -> anyhow::Result<()> {
    if output.json {
        println!("{}", serde_json::to_string_pretty(status).map_err(flat)?);
    } else {
        print!("{}", report(status, output.raw));
    }
    Ok(())
}

/// What a reading says when it has no value, or `value` of it when it has.
fn read<T>(reading: &Reading<T>, value: impl FnOnce(&T) -> String) -> String {
    match reading {
        Reading::NotSupported => "not supported".to_owned(),
        Reading::NoReply => "no reply".to_owned(),
        Reading::Malformed => "malformed reply".to_owned(),
        Reading::Value(v) => value(v),
    }
}

/// A reading shown by its `Debug` form.
fn read_debug<T: Debug>(reading: &Reading<T>) -> String {
    read(reading, |v| format!("{v:?}"))
}

/// `items`, comma-separated.
fn list<T: Debug>(items: &[T]) -> String {
    items
        .iter()
        .map(|item| format!("{item:?}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// One labelled line of the report.
fn row(out: &mut String, label: &str, value: impl Display) {
    let _ = writeln!(out, "  {label:<24}{value}");
}

/// The noise-cancelling and ambient state, with the level's range from the capability.
fn nc_asm(state: &NcAsmState, capability: &Reading<NcAsmCapability>) -> String {
    let steps = |id: AsmId| {
        capability
            .value()
            .and_then(|cap| cap.asm_step(id))
            .map_or_else(String::new, |step| format!(" of {step}"))
    };
    match state {
        NcAsmState::NoiseCancelling { on, .. } => format!("noise cancelling {on:?}"),
        NcAsmState::NoiseCancellingAndAmbient {
            effect,
            nc_value,
            asm_id,
            asm_level,
            ..
        } => format!(
            "effect {effect:?}, noise cancelling {nc_value:?}, ambient {asm_id:?} level {asm_level}{}",
            steps(*asm_id)
        ),
        NcAsmState::Ambient {
            effect,
            asm_id,
            level,
            ..
        } => format!(
            "effect {effect:?}, ambient {asm_id:?} level {level}{}",
            steps(*asm_id)
        ),
    }
}

/// What the noise-cancelling capability offers.
fn nc_asm_capability(capability: &NcAsmCapability) -> String {
    let modes = |asm: &[AsmStep]| {
        asm.iter()
            .map(|a| format!("{:?} {} steps", a.id, a.step))
            .collect::<Vec<_>>()
            .join(", ")
    };
    match capability {
        NcAsmCapability::NoiseCancelling { .. } => "noise cancelling only".to_owned(),
        NcAsmCapability::NoiseCancellingAndAmbient {
            nc_setting_type,
            nc_step,
            asm,
            ..
        } => format!(
            "noise cancelling {nc_setting_type:?} (ncStep {nc_step}); ambient: {}",
            modes(asm)
        ),
        NcAsmCapability::Ambient { asm, .. } => format!("ambient: {}", modes(asm)),
    }
}

/// The pressure half of the NC optimizer.
fn pressure(pressure: AtmosphericPressure) -> String {
    match pressure {
        AtmosphericPressure::Unmeasured => "not measured".to_owned(),
        AtmosphericPressure::Atm(tenths) => format!("{}.{} atm", tenths / 10, tenths % 10),
        AtmosphericPressure::Unknown(byte) => format!("unknown {byte:#04x}"),
    }
}

/// The equalizer rows: preset, the bands with what each is, and Clear Bass.
fn equalizer(out: &mut String, status: &Status) {
    row(
        out,
        "equalizer",
        read(&status.eq, |eq| {
            let name = status
                .capabilities
                .eq
                .value()
                .and_then(|cap| cap.presets.iter().find(|p| p.id == eq.preset))
                .filter(|p| !p.name.is_empty())
                .map_or_else(String::new, |p| format!(" ({})", p.name));
            format!("preset {:?}{name}", eq.preset)
        }),
    );
    if let Some(cap) = status.capabilities.eq.value() {
        row(
            out,
            "equalizer range",
            format!(
                "{} bands, {} levels (0 to {})",
                cap.band_count,
                cap.level_steps,
                cap.level_steps.saturating_sub(1)
            ),
        );
    }
    let values = status.eq.value().map(|eq| &eq.values);
    let bands = status.eq_bands.value();
    let named = |index: usize, value: u8| {
        let label = bands.and_then(|b| b.bands.get(index)).map_or_else(
            || format!("band {index}"),
            |band| match band.kind {
                EqBandInformationType::Hz => format!("{} Hz", band.value),
                EqBandInformationType::Khz => format!("{} kHz", band.value),
                _ if band.is_clear_bass() => "clear bass".to_owned(),
                kind => format!("{kind:?} {}", band.value),
            },
        );
        format!("{label} {value}")
    };
    match values {
        Some(values) => row(
            out,
            "equalizer bands",
            values
                .iter()
                .enumerate()
                .map(|(i, v)| named(i, *v))
                .collect::<Vec<_>>()
                .join(", "),
        ),
        None => row(out, "equalizer bands", read(&status.eq, |_| String::new())),
    }
    row(
        out,
        "clear bass",
        read(&status.eq_bands, |b| match (b.clear_bass_index(), values) {
            (Some(i), Some(values)) => values
                .get(i)
                .map_or_else(|| format!("band {i}, no value"), |v| format!("{v}")),
            (Some(i), None) => format!("band {i}"),
            (None, _) => "not listed".to_owned(),
        }),
    );
}

/// One general setting as `title -> value`.
fn general_setting(setting: &junk_sony::proto::GeneralSetting) -> String {
    let title = match setting.capability.value() {
        Some(cap) => match cap.title.title() {
            GsTitle::Raw(text) | GsTitle::Unknown(text) => text,
            known => format!("{known:?}"),
        },
        None => format!("slot {:02X}", setting.slot),
    };
    let value = read(&setting.value, |value| match value {
        GsValue::Boolean(on) => format!("{on:?}"),
        GsValue::List(index) => setting
            .capability
            .value()
            .and_then(|cap| cap.items.get(usize::from(*index)))
            .map_or_else(|| format!("choice {index}"), |item| item.name.text.clone()),
        GsValue::Other {
            setting_type,
            value,
        } => format!("type {setting_type:#04x} value {value}"),
    });
    format!("{title} -> {value}")
}

/// The text report of `status`; the raw replies only with `raw`.
fn report(status: &Status, raw: bool) -> String {
    let mut out = String::new();
    device_rows(&mut out, &status.device);
    out.push_str("\nSettings\n");
    setting_rows(&mut out, status);
    control_rows(&mut out, status);
    out.push_str("\nPaired devices\n");
    paired_rows(&mut out, status);
    if raw {
        out.push_str("\nRaw replies (data type, then payload)\n");
        for reply in &status.raw_replies {
            let _ = writeln!(
                out,
                "  {:02x}  {}",
                reply.data_type.raw(),
                hex(&reply.payload)
            );
        }
    }
    out
}

/// The pairing capability: how many devices the headset pairs with and connects at once.
fn pairing_capability(capability: &PairingCapability) -> String {
    let transfer = match capability.file_transfer {
        FileTransferSupport::Possible => ", file transfer possible".to_owned(),
        FileTransferSupport::Impossible => String::new(),
        FileTransferSupport::Unknown(byte) => format!(", file transfer unknown {byte:#04x}"),
    };
    format!(
        "up to {} paired, {} connected{transfer}",
        capability.max_paired, capability.max_connected
    )
}

/// One paired device: name, address, whether it is connected, and a marker if it holds playback.
fn paired_device(device: &junk_sony::payload::PairedDevice, playback: bool) -> String {
    let connection = match device.connection {
        ConnectionOrder::NotConnected => "not connected".to_owned(),
        ConnectionOrder::Connected(order) => format!("connected ({order})"),
    };
    let marker = if playback { "  <- playback" } else { "" };
    format!("{}  {}  {connection}{marker}", device.name, device.address)
}

/// The paired-device rows: one per device, or why there are none.
fn paired_device_rows(out: &mut String, paired: &Reading<PairedDevices>) {
    let Reading::Value(paired) = paired else {
        row(out, "devices", read(paired, |_| String::new()));
        return;
    };
    if paired.devices.is_empty() {
        row(out, "devices", "none paired");
    }
    let holder = paired.playback_device();
    for device in &paired.devices {
        let holds = holder.is_some_and(|holder| std::ptr::eq(holder, device));
        row(out, "device", paired_device(device, holds));
    }
    if let PlaybackHolder::Order(order) = paired.playback
        && holder.is_none()
    {
        row(
            out,
            "playback",
            format!("connection order {order}, which no listed device has"),
        );
    }
}

/// The paired-devices group: the capability, the pairing mode and the list.
fn paired_rows(out: &mut String, status: &Status) {
    row(
        out,
        "capability",
        read(&status.capabilities.pairing, pairing_capability),
    );
    row(
        out,
        "pairing mode",
        read(&status.pairing_mode, |m| {
            format!("{:?} ({:?})", m.mode, m.status)
        }),
    );
    paired_device_rows(out, &status.paired_devices);
}

/// The device group: what the headset says it is.
fn device_rows(out: &mut String, device: &DeviceInfo) {
    out.push_str("Device\n");
    row(out, "model", read(&device.model, Clone::clone));
    row(out, "firmware", read(&device.firmware, Clone::clone));
    row(
        out,
        "series",
        read(&device.model_info, |m| {
            format!("{:?}, colour {:#04x}", m.series, m.color)
        }),
    );
    row(
        out,
        "protocol version",
        read(&device.protocol_version, |v| format!("{v:#06x}")),
    );
    row(
        out,
        "unique id",
        read(&device.capability_info, |c| {
            format!("{} (capability counter {})", c.unique_id, c.counter)
        }),
    );
    row(
        out,
        "functions",
        read(&device.functions, |f| format!("{}: {}", f.len(), list(f))),
    );
}

/// The settings group, up to extra bass: battery, codec, noise cancelling, equalizer.
fn setting_rows(out: &mut String, status: &Status) {
    row(
        out,
        "battery",
        read(&status.battery, |b| {
            format!("{}% {:?}", b.level, b.charging)
        }),
    );
    row(
        out,
        "battery left/right",
        read(&status.battery_left_right, |b| {
            format!(
                "left {}% {:?}, right {}% {:?}",
                b.left.level, b.left.charging, b.right.level, b.right.charging
            )
        }),
    );
    row(
        out,
        "battery cradle",
        read(&status.battery_cradle, |b| {
            format!("{}% {:?}", b.level, b.charging)
        }),
    );
    row(out, "codec", read_debug(&status.codec));
    row(
        out,
        "noise cancelling",
        read(&status.nc_asm, |state| {
            nc_asm(state, &status.capabilities.nc_asm)
        }),
    );
    row(
        out,
        "noise cancelling caps",
        read(&status.capabilities.nc_asm, nc_asm_capability),
    );
    equalizer(out, status);
    row(
        out,
        "extra bass",
        read(&status.ebb, |level| {
            let range = status
                .capabilities
                .ebb
                .value()
                .map_or_else(String::new, |c| format!(" (range {}..{})", c.min, c.max));
            format!("{level}{range}")
        }),
    );
}

/// The settings group, from DSEE on: the ones that are not about sound levels.
fn control_rows(out: &mut String, status: &Status) {
    row(out, "dsee", read_debug(&status.dsee));
    row(
        out,
        "dsee indicator",
        read(&status.upscaling_indicator, |u| {
            format!("{:?} {:?}", u.kind, u.status)
        }),
    );
    row(out, "connection mode", read_debug(&status.connection_mode));
    row(out, "voice guidance", read_debug(&status.voice_guidance));
    row(
        out,
        "voice guidance language",
        read_debug(&status.voice_guidance_language),
    );
    row(
        out,
        "pause when taken off",
        read_debug(&status.pause_when_taken_off),
    );
    row(
        out,
        "auto power off",
        read(&status.auto_power_off, |a| {
            format!("active {:?}, timer {:?}", a.active, a.timer)
        }),
    );
    row(out, "speak-to-chat", read_debug(&status.speak_to_chat));
    row(
        out,
        "speak-to-chat config",
        read(&status.speak_to_chat_config, |c| {
            format!(
                "sensitivity {:?}, focus on voice {:?}, timeout {:?}",
                c.sensitivity, c.focus_on_voice, c.timeout
            )
        }),
    );
    row(
        out,
        "assignable buttons",
        read(&status.assignable_settings, |presets| list(presets)),
    );
    if status.general_settings.is_empty() {
        row(out, "general settings", "none listed");
    }
    for setting in &status.general_settings {
        row(out, "general setting", general_setting(setting));
    }
    row(
        out,
        "nc optimizer",
        read(&status.nc_optimizer, |n| {
            format!(
                "personal value {}, pressure {}",
                n.personal_value,
                pressure(n.pressure)
            )
        }),
    );
    row(
        out,
        "left/right connection",
        read(&status.connection_status, |c| {
            format!("left {:?}, right {:?}", c.left, c.right)
        }),
    );
    row(out, "serial", read(&status.serial, Clone::clone));
}
