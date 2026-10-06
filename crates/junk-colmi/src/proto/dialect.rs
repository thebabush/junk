//! Dialects: the ways Colmi rings differ, as data rather than code paths (SPEC §3.4).

use junk_core::ChannelSet;

use crate::wire::Capabilities;
use crate::{V2_CMD, V2_NOTIFY};

/// Which services the ring has.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Transport {
    /// The V1 service only: 16-byte frames, no big data.
    V1,
    /// Both services: big data on V2 as well.
    Both,
}

/// How live heart rate is done.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum LiveHr {
    /// `0x69`/`0x6a`: a one-shot manual measurement.
    Manual69,
    /// `0x77`/`0x78`: a phone-initiated workout streams samples, and the stored record is
    /// fetched with `bc 41`–`bc 45` afterwards. The R10.
    Workout77,
}

/// Where sleep comes from.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum SleepSource {
    /// `bc 27` on V2: the "new sleep protocol" the capability bitmap announces.
    BigData27,
    /// The legacy `0x44` frames on V1.
    Legacy44,
}

/// What this ring speaks, built up as the session reveals it.
///
/// [`Dialect::from_channels`] starts from what resolved at connect,
/// [`Dialect::apply_capabilities`] reads the `0x01` ack, and [`Dialect::apply_firmware`]
/// picks the `KNOWN` row for whatever the bitmap does not cover. Every step only ever
/// widens the conservative default, so an unknown ring gets the least it can be assumed to
/// have, never a refusal.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Dialect {
    /// Which services the ring has.
    pub transport: Transport,
    /// Whether `0xbc` big-data requests can be sent.
    pub big_data: bool,
    /// How live heart rate is done.
    pub live_hr: LiveHr,
    /// Where sleep comes from.
    pub sleep: SleepSource,
    /// Whether skin temperature is measured (`bc 25`).
    pub temperature: bool,
    /// The firmware prefixes of the row that matched, empty until one does.
    pub fw_prefix: &'static [&'static str],
}

/// The firmware prefix table: what each family of firmware selects that the capability
/// bitmap does not say. One row per family seen; a new ring is a new row and a trace.
const KNOWN: &[(&[&str], LiveHr)] = &[(&["RT03CR"], LiveHr::Workout77)];

impl Dialect {
    /// The least any ring can be assumed to have: V1 only, no big data, manual live HR,
    /// legacy sleep, no temperature.
    pub const CONSERVATIVE: Dialect = Dialect {
        transport: Transport::V1,
        big_data: false,
        live_hr: LiveHr::Manual69,
        sleep: SleepSource::Legacy44,
        temperature: false,
        fw_prefix: &[],
    };

    /// The dialect the resolved channels allow: both V2 channels present means the ring
    /// has the big-data service; anything less is treated as V1 only.
    #[must_use]
    pub fn from_channels(resolved: &ChannelSet) -> Dialect {
        let v2 = resolved.contains(V2_CMD) && resolved.contains(V2_NOTIFY);
        Dialect {
            transport: if v2 { Transport::Both } else { Transport::V1 },
            big_data: v2,
            ..Dialect::CONSERVATIVE
        }
    }

    /// Takes what the `0x01` ack says: temperature, and which sleep protocol.
    pub fn apply_capabilities(&mut self, caps: &Capabilities) {
        self.temperature = caps.temperature;
        self.sleep = if caps.new_sleep_protocol {
            SleepSource::BigData27
        } else {
            SleepSource::Legacy44
        };
    }

    /// Picks the `KNOWN` row whose prefix `fw` starts with, if there is one. Returns
    /// whether one matched; nothing changes when none does.
    pub fn apply_firmware(&mut self, fw: &str) -> bool {
        let row = KNOWN
            .iter()
            .find(|(prefixes, _)| prefixes.iter().any(|prefix| fw.starts_with(prefix)));
        let Some(&(prefixes, live_hr)) = row else {
            return false;
        };
        self.live_hr = live_hr;
        self.fw_prefix = prefixes;
        true
    }
}

impl Default for Dialect {
    fn default() -> Self {
        Self::CONSERVATIVE
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{Cmd, Frame, RingFrame};
    use crate::{V1_NOTIFY, V1_WRITE};

    fn channels(chans: &[junk_core::Channel]) -> ChannelSet {
        chans.iter().copied().collect()
    }

    /// The capability bitmap of the `QRing` fixture's `0x01` ack.
    fn fixture_caps() -> Capabilities {
        let body = [
            0x01, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x20, 0x00, 0x00, 0x30,
        ];
        match RingFrame::decode(&Frame {
            cmd: Cmd::SetTime,
            body,
        }) {
            Ok(RingFrame::SetTimeAck(caps)) => caps,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn channels_decide_the_transport() {
        let both = Dialect::from_channels(&channels(&[V1_WRITE, V1_NOTIFY, V2_CMD, V2_NOTIFY]));
        assert_eq!(both.transport, Transport::Both);
        assert!(both.big_data);
        assert_eq!(
            both,
            Dialect {
                transport: Transport::Both,
                big_data: true,
                ..Dialect::CONSERVATIVE
            }
        );

        let v1 = Dialect::from_channels(&channels(&[V1_WRITE, V1_NOTIFY]));
        assert_eq!(v1, Dialect::CONSERVATIVE);
        assert_eq!(Dialect::default(), Dialect::CONSERVATIVE);

        // Half a V2 service is no V2 service.
        let half = Dialect::from_channels(&channels(&[V1_WRITE, V1_NOTIFY, V2_NOTIFY]));
        assert_eq!(half, Dialect::CONSERVATIVE);
        let half = Dialect::from_channels(&channels(&[V1_WRITE, V1_NOTIFY, V2_CMD]));
        assert_eq!(half, Dialect::CONSERVATIVE);
    }

    #[test]
    fn capabilities_set_temperature_and_sleep() {
        let mut dialect = Dialect::CONSERVATIVE;
        dialect.apply_capabilities(&fixture_caps());
        assert!(dialect.temperature);
        assert_eq!(dialect.sleep, SleepSource::BigData27);
        // Everything else is untouched.
        assert_eq!(dialect.transport, Transport::V1);
        assert_eq!(dialect.live_hr, LiveHr::Manual69);

        let none = Capabilities {
            temperature: false,
            new_sleep_protocol: false,
            ..fixture_caps()
        };
        dialect.apply_capabilities(&none);
        assert!(!dialect.temperature);
        assert_eq!(dialect.sleep, SleepSource::Legacy44);
    }

    #[test]
    fn firmware_picks_a_row_or_leaves_the_dialect_alone() {
        let mut dialect = Dialect::CONSERVATIVE;
        assert!(dialect.apply_firmware("RT03CR_1.00.02_260319"));
        assert_eq!(dialect.live_hr, LiveHr::Workout77);
        assert_eq!(dialect.fw_prefix, ["RT03CR"]);

        let mut unknown = Dialect::CONSERVATIVE;
        assert!(!unknown.apply_firmware("XX99_1.0"));
        assert_eq!(unknown, Dialect::CONSERVATIVE);
        assert!(!unknown.apply_firmware(""));
        assert_eq!(unknown, Dialect::CONSERVATIVE);
        // A prefix is a prefix, not a substring.
        assert!(!unknown.apply_firmware("_RT03CR"));
        assert_eq!(unknown, Dialect::CONSERVATIVE);
    }
}
