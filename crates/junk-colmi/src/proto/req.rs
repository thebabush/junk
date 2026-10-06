//! What a user asks a Colmi ring, what it answers, and what the ring says on its own.

use alloc::string::String;
use alloc::vec::Vec;

use crate::measure::{
    Bpm, HrSample, HrvSample, SleepSession, Spo2Sample, StepBucket, StressSample, TempSample,
};
use junk_core::{Battery, Bytes, Channel, ChannelSet, Timestamp};

use crate::wire::{
    BigData, BigDataKind, Capabilities, Cmd, Frame, Notification, Platform, Prefs, RingFrame,
    WorkoutAction, WorkoutDescriptor, WorkoutSummary,
};

/// What a user can ask a Colmi ring. Each is answered by exactly one
/// [`Output::Done`](junk_core::Output::Done) carrying the [`Resp`] its doc names.
///
/// The requests that place a day carry a [`Timestamp`] for it: the driver never computes
/// "today" (SPEC §3.1), and the UTC offset of that timestamp is the one every sample of
/// the answer gets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Req {
    /// `0x01`: set the ring's clock to `at`, completed with `second`.
    ///
    /// The ring is written the *local* wall clock: it knows no zones, and everything it
    /// reports afterwards is that clock expressed as if it were UTC. A [`Timestamp`] is a
    /// local minute already; `second` finishes it. `lang` is the frame's last byte, `01`
    /// from `QRing`. Answered by [`Resp::Capabilities`]: the ack is the ring's capability
    /// bitmap, which also updates the [`Dialect`](crate::proto::Dialect) and is reported
    /// as [`Ev::Capabilities`].
    SetTime {
        /// The wall-clock minute to write.
        at: Timestamp,
        /// The second within that minute.
        second: u8,
        /// The last body byte; `1` as `QRing` sends it.
        lang: u8,
    },
    /// `0x03`: read the battery. Answered by [`Resp::Battery`].
    Battery,
    /// `0x04`: tell the ring about the phone. Answered by [`Resp::Ack`].
    PhoneName {
        /// The phone's platform.
        platform: Platform,
        /// Its OS major version.
        os_version: u8,
        /// The app's name, at most [`HostFrame::PHONE_NAME_MAX_LEN`] bytes.
        ///
        /// [`HostFrame::PHONE_NAME_MAX_LEN`]: crate::wire::HostFrame::PHONE_NAME_MAX_LEN
        name: Vec<u8>,
    },
    /// `0x0a 01`: read the user profile. Answered by [`Resp::Prefs`].
    ReadPrefs,
    /// `0x0a 02`: write the user profile. Answered by [`Resp::Ack`].
    WritePrefs(Prefs),
    /// `0x19`, then the Device Information service: the ring acks the frame, and the
    /// firmware and hardware revision strings are read from `2a26` and `2a27`. Answered
    /// by [`Resp::Version`]; the firmware string also picks the
    /// [`Dialect`](crate::proto::Dialect) row, and one no row matches is reported as
    /// [`Ev::UnknownFirmware`]. Refused as unsupported on a ring whose Device Information
    /// channels did not resolve.
    Version,
    /// `0x21`: read the daily goals. Answered by [`Resp::Goals`].
    ReadGoals,
    /// `0x16`: read the automatic HR preference. Answered by [`Resp::AutoPref`].
    ReadAutoHrPref,
    /// `0x2c`: read the automatic `SpO2` preference. Answered by [`Resp::AutoPref`].
    ReadAutoSpo2Pref,
    /// `0x36`: read the automatic stress preference. Answered by [`Resp::AutoPref`].
    ReadAutoStressPref,
    /// `0x38`: read the automatic HRV preference. Answered by [`Resp::AutoPref`].
    ReadAutoHrvPref,
    /// `0x48`: read today's running totals. Answered by [`Resp::TodayTotals`].
    TodayTotals,
    /// `0x77`: start, pause, continue or stop a phone-initiated workout. Answered by
    /// [`Resp::WorkoutCtl`]; the `0x78` samples that follow a start are [`Ev::Workout`].
    WorkoutCtl {
        /// What to do.
        action: WorkoutAction,
        /// The sport, as the app numbers them.
        sport_type: u8,
    },
    /// `0x69`: start a one-shot manual HR measurement. Answered by [`Resp::Ack`] on the
    /// first `0x69` frame the ring sends back.
    ManualHrStart {
        /// What to measure.
        kind: u8,
        /// The second byte, whatever the kind takes.
        sub: u8,
    },
    /// `0x6a`: stop it. Answered by [`Resp::Ack`] on the first `0x6a` frame back.
    ManualHrStop {
        /// What to stop measuring.
        kind: u8,
    },
    /// Any 16-byte frame, undecoded. Answered by [`Resp::Raw`] with the first reply frame
    /// of the same command byte; this is how `0x3a`, `0x3b` and `0x3c` are exercised.
    /// [`Cmd::FactoryReset`] is refused.
    Raw {
        /// The command byte.
        cmd: Cmd,
        /// The body, as is.
        body: [u8; Frame::BODY_LEN],
    },
    /// Any big-data request, undecoded. Answered by [`Resp::RawBigData`] with the next
    /// complete frame of the same kind; the `0x30` file list uses this.
    RawBigData {
        /// The kind byte.
        kind: BigDataKind,
        /// The body, as is.
        body: Vec<u8>,
    },
    /// `0x15`: the periodic HR log of one day. Answered by [`Resp::HrLog`].
    ///
    /// The ring is asked for the day by its local midnight as a UTC epoch second, which
    /// is `day_start.local_minute × 60`; a day that does not fit the ring's `u32` is
    /// refused as unsupported. The replies are a header with the packet count and the
    /// sampling interval, then packets in index order: the first with nine samples, the
    /// rest with thirteen, the last at index `count − 1`. A `15 ff` means the day has no
    /// log and answers with no samples.
    HrLog {
        /// The day's local midnight. The wire carries it as is: pass a midnight.
        day_start: Timestamp,
    },
    /// `0x37`: the stress series of one day. Answered by [`Resp::Stress`].
    ///
    /// The day is `today`'s midnight less `days_ago` days. The replies are laid out like
    /// the HR log's with a day offset in the first packet instead of a timestamp: twelve
    /// samples in the first packet, thirteen in the rest.
    Stress {
        /// Which day, `0` for today.
        days_ago: u8,
        /// Today, so the day can be placed.
        today: Timestamp,
    },
    /// `0x39`: the HRV series of one day, laid out like [`Req::Stress`]. Answered by
    /// [`Resp::Hrv`].
    Hrv {
        /// Which day, `0` for today.
        days_ago: u8,
        /// Today, so the day can be placed.
        today: Timestamp,
    },
    /// `0x43`: the activity rows of one day, a range of its 15-minute buckets. Answered
    /// by [`Resp::Activity`]; [`Req::activity_day`] asks for the whole day.
    ///
    /// The replies are `43 ff` for a day with nothing, else a header and then one row per
    /// bucket with activity, in index order. Rows carry their own date; `today` only
    /// supplies the zone.
    Activity {
        /// Which day, `0` for today. `QRing` asks for at most 29.
        days_ago: u8,
        /// Today, so the rows get their zone.
        today: Timestamp,
        /// The first bucket wanted, `0..=95`. `QRing` asks every day from `0` except the
        /// oldest it keeps, day 29, which it asks from `1`.
        first_bucket: u8,
        /// The last bucket wanted, `0..=95`; `95` is the end of the day.
        last_bucket: u8,
    },
    /// `bc 27`: sleep. Answered by [`Resp::Sleep`]. Refused as unsupported unless the
    /// [`Dialect`](crate::proto::Dialect) says sleep comes this way.
    Sleep {
        /// Today, so the days can be placed.
        today: Timestamp,
        /// The request body, one or two bytes. `QRing` sends `06 01` on the first sync of
        /// a session and `01 01` afterwards; the R09 decompilation reads a one-byte body
        /// as `00` = today, `ff` = all history. What the two bytes mean is not settled,
        /// so they are sent as given.
        selector: Vec<u8>,
    },
    /// `bc 2a`: `SpO2`. Answered by [`Resp::Spo2`].
    Spo2 {
        /// Today, so the days can be placed.
        today: Timestamp,
        /// The request byte: `ff` on the first sync of a session (all history, per the
        /// R09 decompilation) and `02` afterwards, as `QRing` sends them.
        selector: u8,
    },
    /// `bc 25`: temperature, accumulated until a frame starts with today's day byte or
    /// has no body. Answered by [`Resp::Temperature`]. Refused as unsupported unless the
    /// [`Dialect`](crate::proto::Dialect) says the ring measures it.
    Temperature {
        /// Today, so the days can be placed.
        today: Timestamp,
        /// Days back to request: `0` = today. `QRing` incremental sync uses days since
        /// its last stored sample, or `6` when there is none.
        selector: u8,
    },
    /// `bc 41`: the workout records since a ring-clock timestamp. Answered by
    /// [`Resp::Workouts`] from the `bc 42` reply.
    WorkoutList {
        /// The cursor.
        since: u32,
    },
    /// `bc 43`: the detail of one workout. Answered by [`Resp::WorkoutDetail`] from the
    /// `bc 44` descriptor and the `bc 45` packages that follow it, in order.
    WorkoutDetail {
        /// The record's sport.
        sport_type: u8,
        /// The record's start, as its summary gave it.
        start: u32,
    },
}

impl Req {
    /// The last 15-minute bucket of a day.
    pub const LAST_BUCKET: u8 = 95;

    /// [`Req::Activity`] for the whole of the day `days_ago` days before `today`'s.
    #[must_use]
    pub fn activity_day(days_ago: u8, today: Timestamp) -> Req {
        Req::Activity {
            days_ago,
            today,
            first_bucket: 0,
            last_bucket: Req::LAST_BUCKET,
        }
    }
}

/// The answer to a [`Req`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resp {
    /// The ring acknowledged; there was nothing else to say.
    Ack,
    /// Answer to [`Req::SetTime`]: what the ring says it can do.
    Capabilities(Capabilities),
    /// Answer to [`Req::Battery`].
    Battery(Battery),
    /// Answer to [`Req::ReadPrefs`].
    Prefs(Prefs),
    /// Answer to [`Req::Version`]: the Firmware Revision and Hardware Revision strings
    /// read after the ack (`RT03CR_1.00.02_260319`, `RT03CR_V1.0`).
    Version {
        /// The firmware revision.
        firmware: String,
        /// The hardware revision.
        hardware: String,
    },
    /// Answer to [`Req::ReadGoals`], as the ring numbers them.
    Goals {
        /// Steps per day.
        steps: u32,
        /// Calories per day, in whatever unit the app uses.
        calories: u32,
        /// Distance per day, metres presumably.
        distance: u32,
        /// Minutes of sport per day.
        sport_min: u16,
        /// Minutes of sleep per day.
        sleep_min: u16,
    },
    /// Answer to the four automatic-measurement reads.
    AutoPref {
        /// Whether the ring measures on its own.
        enabled: bool,
        /// Minutes between measurements, for the preferences that have one (HR, `SpO2`).
        interval_min: Option<u8>,
    },
    /// Answer to [`Req::TodayTotals`].
    TodayTotals {
        /// Steps so far today.
        steps: u32,
        /// Of which running.
        running_steps: u32,
        /// Small calories so far today.
        cal: u32,
        /// Metres so far today.
        distance_m: u32,
        /// Active minutes so far today.
        active_min: u16,
    },
    /// Answer to [`Req::WorkoutCtl`].
    WorkoutCtl {
        /// The workout's start on the ring's own clock, present when a start was acked.
        start: Option<u32>,
    },
    /// Answer to [`Req::Raw`]: the reply frame, undecoded.
    Raw(Frame),
    /// Answer to [`Req::RawBigData`]: the reply frame, reassembled and CRC-checked.
    RawBigData(BigData),
    /// Answer to [`Req::HrLog`]: one sample per non-zero slot, in slot order, each
    /// `slot × interval` minutes after the requested day's start, in its zone, and
    /// [`Source::Periodic`](crate::measure::Source::Periodic).
    HrLog(Vec<HrSample>),
    /// Answer to [`Req::Stress`]: one sample per non-zero slot, in slot order, each
    /// `slot × interval` minutes after the day's midnight.
    Stress(Vec<StressSample>),
    /// Answer to [`Req::Hrv`], laid out like [`Resp::Stress`]. The ring reports HRV
    /// hourly with the half-hour slot between zero, so those slots are absent.
    Hrv(Vec<HrvSample>),
    /// Answer to [`Req::Activity`]: one bucket per row, in the order sent. A bucket starts
    /// at its row's date plus `bucket × 15` minutes and spans an hour, which is all this
    /// ring sends; its calories are the raw field times ten when the header said so.
    Activity(Vec<StepBucket>),
    /// Answer to [`Req::Sleep`]: one session per day block. A session that started at or
    /// after 18:00 of its day began the evening before.
    Sleep(Vec<SleepSession>),
    /// Answer to [`Req::Spo2`]: per day block, one sample per hour with data, on the hour.
    /// The ring reports one value per hour (its minimum and maximum agree), and the
    /// maximum is the one taken.
    Spo2(Vec<Spo2Sample>),
    /// Answer to [`Req::Temperature`]: per day block, one sample per slot with data, slot
    /// `i` at `i × interval` minutes after the day's midnight.
    Temperature(Vec<TempSample>),
    /// Answer to [`Req::WorkoutList`]: the records, as the `bc 42` reply lists them.
    Workouts(WorkoutSummary),
    /// Answer to [`Req::WorkoutDetail`].
    WorkoutDetail {
        /// How the samples are laid out.
        descriptor: WorkoutDescriptor,
        /// The heart-rate samples, one per sampling interval, the packages' end to end; a
        /// zero byte is [`Bpm::Invalid`].
        heart_rates: Vec<Bpm>,
    },
}

/// Things a Colmi ring reports on its own, and things the driver could not place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ev {
    /// The `0x01` ack, whenever it arrives; it also answers an in-flight [`Req::SetTime`].
    Capabilities(Capabilities),
    /// `0x2f`: the ring's packet size, volunteered after connect.
    PacketSize(u8),
    /// `0x73`: new data, battery, live activity, a stored workout.
    Notification(Notification),
    /// A `0x03` reply no request was waiting for. The app polls; a late one is still news.
    Battery(Battery),
    /// `0x78`: one sample of the live workout stream, about once a second. The sequence
    /// number repeats every ten frames or so; dedupe on it.
    Workout {
        /// The sport being recorded.
        sport_type: u8,
        /// The second body byte, `01` throughout the fixture.
        flag: u8,
        /// The sample's sequence number.
        seq: u8,
        /// Heart rate; [`Bpm::Invalid`] before the first reading.
        bpm: Bpm,
    },
    /// A raw string on V1 notify, or a Device Information value no [`Req::Version`] was
    /// waiting for.
    Text(String),
    /// A well-formed reply that no transaction wanted.
    UnexpectedReply(RingFrame),
    /// A complete big-data frame of a kind the active transaction did not ask for.
    UnexpectedBigData(BigData),
    /// Bytes the wire layer rejected. Nothing else happened (invariant 5).
    Unparsed {
        /// Where they arrived.
        chan: Channel,
        /// What they were.
        bytes: Bytes,
    },
    /// The link came up without these required channels; the driver asked to disconnect.
    MissingChannels(ChannelSet),
    /// `0x19` gave a firmware string no dialect row matches. The conservative dialect is
    /// kept; this is never a refusal (SPEC §3.4).
    UnknownFirmware(String),
}
