//! What the payload of a Sony frame says: the typed side of the v1 command tables.
//!
//! A frame's payload starts with a command id and, in nearly every group, an "inquired
//! type" byte. [`decode_report`] turns a payload the headset sent, whether it answers a
//! GET or announces a change on its own, into a [`Report`]. Both layouts are the same
//! (in Sony's app the RET and NTFY of the same member share a layout), so one decoder
//! serves the reply to a read and the notification.
//!
//! Every layout here is read out of Sony's own app and none has been
//! checked against an XM4. Three rules hold for all of them:
//!
//! - **The app ignores bytes past the layout, so does this.** A payload longer than its
//!   layout decodes. A payload that is too short is a typed
//!   [`ProtoError::Malformed`], never a panic. Voice
//!   guidance is the exception: the app validates its exact lengths and so does this.
//! - **Every enum keeps the byte it did not recognise.** Sony's names are the variant
//!   names; each enum has an `Unknown(u8)` (or `Other(u8)`, where Sony itself has an
//!   `OTHER`) so a firmware that sends a new value is read, not refused.
//! - **A byte whose meaning is not known stays a `u8`.** Where a
//!   field's meaning is unknown, the field is a raw byte whose docs say so.
//!
//! The command tables are two: table one is carried by data type `0C` and table two by
//! `0E` (the link layer of Sony's app). Command ids overlap between them, so the decoder takes the
//! [`Table`] as well as the payload.

use alloc::string::String;
use alloc::vec::Vec;

use junk_core::ProtoError;

use crate::wire::DataType;

/// Declares an enum over one byte, Sony's names for the values it names and one variant
/// that carries any other byte, with `from_raw` and `raw` as inverses.
macro_rules! byte_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident, $other:ident {
            $( $(#[$vmeta:meta])* $variant:ident = $value:literal, )+
        }
    ) => {
        $(#[$meta])*
        #[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
        $vis enum $name {
            $(
                #[doc = concat!("Wire value `", stringify!($value), "`.")]
                $(#[$vmeta])*
                $variant,
            )+
            /// A byte this crate does not name; it round-trips through
            /// [`from_raw`](Self::from_raw) and [`raw`](Self::raw).
            $other(u8),
        }

        impl $name {
            /// The value for a wire byte; a byte with no name is the catch-all variant.
            #[must_use]
            pub const fn from_raw(raw: u8) -> Self {
                match raw {
                    $( $value => Self::$variant, )+
                    other => Self::$other(other),
                }
            }

            /// The wire byte.
            #[must_use]
            pub const fn raw(self) -> u8 {
                match self {
                    $( Self::$variant => $value, )+
                    Self::$other(raw) => raw,
                }
            }
        }
    };
}

mod decode;
mod enums;
#[cfg(test)]
mod tests;
mod types;

pub use decode::decode_report;
pub use enums::{
    AlertMessageType, AsmId, AsmSettingType, AssignableSettingsPreset, AudioCodec,
    AutoPowerOffElementId, BatteryChargingStatus, CommonStatus, ConnectionMode, ConnectionState,
    DseeSetting, EqBandInformationType, EqEbbInquiredType, EqPresetId, FileTransferSupport,
    FunctionType, GsSettingType, GsStringFormat, MdrLanguage, ModeOutTime, ModelSeries,
    NcAsmEffect, NcAsmInquiredType, NcAsmSettingType, NcDualSingleValue, OnOff, PairingMode,
    StcSensitivity, UpscalingEffectStatus, UpscalingEffectType,
};
pub use types::{
    AsmStep, AtmosphericPressure, AutoPowerOff, Battery, CapabilityInfo, ConnectionOrder,
    EbbCapability, EqBand, EqBands, EqCapability, EqPreset, EqState, GsCapability, GsListItem,
    GsString, GsTitle, GsValue, LeftRightBattery, LeftRightConnection, ModelInfo, NcAsmCapability,
    NcAsmState, NcOptimizer, PairedDevice, PairedDevices, PairingCapability, PairingModeState,
    PlaybackHolder, Report, SpeakToChatConfig, UpscalingIndicator, VoiceGuidanceCapability,
};

/// Which of the two v1 command tables a frame's data type selects.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub enum Table {
    /// Table one, data types `0C` (`DATA_MDR`) and `1C` (`SHOT_MDR`).
    One,
    /// Table two, data types `0E` (`DATA_MDR_NO2`) and `1E` (`SHOT_MDR_NO2`): the
    /// peripheral and voice-guidance groups.
    Two,
}

impl Table {
    /// The table a data type carries, or `None` for a data type a v1 link does not handle
    /// (Sony's app ACKs and drops those).
    #[must_use]
    pub const fn of(data_type: DataType) -> Option<Self> {
        match data_type {
            DataType::DataMdr | DataType::ShotMdr => Some(Self::One),
            DataType::DataMdrNo2 | DataType::ShotMdrNo2 => Some(Self::Two),
            _ => None,
        }
    }
}

/// A cursor over a payload that fails with a fixed message instead of running off the end.
pub(crate) struct Reader<'a> {
    rest: &'a [u8],
    short: &'static str,
}

impl<'a> Reader<'a> {
    /// A reader over `bytes`; `short` is the message every too-short read fails with.
    pub(crate) const fn new(bytes: &'a [u8], short: &'static str) -> Self {
        Self { rest: bytes, short }
    }

    pub(crate) fn u8(&mut self) -> Result<u8, ProtoError> {
        let (&first, rest) = self
            .rest
            .split_first()
            .ok_or(ProtoError::Malformed(self.short))?;
        self.rest = rest;
        Ok(first)
    }

    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8], ProtoError> {
        if self.rest.len() < n {
            return Err(ProtoError::Malformed(self.short));
        }
        let (head, rest) = self.rest.split_at(n);
        self.rest = rest;
        Ok(head)
    }

    /// A length byte, then that many bytes, as text. Sony's strings are ASCII or UTF-8;
    /// anything else is replaced rather than refused.
    pub(crate) fn text(&mut self) -> Result<String, ProtoError> {
        let len = usize::from(self.u8()?);
        Ok(String::from_utf8_lossy(self.take(len)?).into_owned())
    }

    /// A count byte, then that many one-byte values.
    pub(crate) fn counted(&mut self) -> Result<Vec<u8>, ProtoError> {
        let n = usize::from(self.u8()?);
        Ok(self.take(n)?.to_vec())
    }

    pub(crate) const fn remaining(&self) -> usize {
        self.rest.len()
    }
}
