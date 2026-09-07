//! Monochrome LED software control, feature `0x1300`.

use num_enum::TryFromPrimitive;
use openlogi_hidpp_derive::Feature;

use crate::{feature::FeatureEndpoint, protocol::v20::Hidpp20Error};

// Layouts are reverse-engineered from libratbag src/hidpp20.c and src/hidpp20.h.
/// The purpose of a logical LED group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive)]
#[repr(u8)]
pub enum LedKind {
    /// Battery indicator.
    Battery = 1,
    /// DPI stage indicator.
    Dpi = 2,
    /// Profile indicator.
    Profile = 3,
    /// Product logo.
    Logo = 4,
    /// Decorative lighting.
    Cosmetic = 5,
}

/// One firmware LED effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive)]
#[repr(u16)]
pub enum LedMode {
    /// Unlit.
    Off = 1,
    /// Steady illumination.
    On = 2,
    /// Repeated flashes.
    Blink = 4,
    /// Sequential illumination.
    Travel = 8,
    /// Increasing brightness.
    RampUp = 0x10,
    /// Decreasing brightness.
    RampDown = 0x20,
    /// Heartbeat effect.
    Heartbeat = 0x40,
    /// Breathing effect.
    Breathing = 0x80,
}

bitflags::bitflags! {
    /// Device-reported supported effects, retaining unknown bits.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct LedModes: u16 {
        /// Unlit.
        const OFF = 1;
        /// Steady illumination.
        const ON = 2;
        /// Repeated flashes.
        const BLINK = 4;
        /// Sequential illumination.
        const TRAVEL = 8;
        /// Increasing brightness.
        const RAMP_UP = 0x10;
        /// Decreasing brightness.
        const RAMP_DOWN = 0x20;
        /// Heartbeat effect.
        const HEARTBEAT = 0x40;
        /// Breathing effect.
        const BREATHING = 0x80;
    }
}

/// Firmware capabilities of one logical LED group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LedInfo {
    /// Logical LED index.
    pub index: u8,
    /// Indicator purpose.
    pub kind: LedKind,
    /// Number of physical LEDs in this group.
    pub physical_count: u8,
    /// Supported effect flags.
    pub modes: LedModes,
    /// Uninterpreted nonvolatile configuration capabilities.
    pub nvconfig_caps: u8,
}

impl LedInfo {
    fn decode(index: u8, bytes: &[u8]) -> Result<Self, Hidpp20Error> {
        if bytes.len() < 6 || bytes[0] != index {
            return Err(Hidpp20Error::UnsupportedResponse);
        }
        Ok(Self {
            index,
            kind: LedKind::try_from(bytes[1]).map_err(|_| Hidpp20Error::UnsupportedResponse)?,
            physical_count: bytes[2],
            modes: LedModes::from_bits_retain(u16::from_be_bytes([bytes[3], bytes[4]])),
            nvconfig_caps: bytes[5],
        })
    }
}

/// Current effect and its uninterpreted, mode-dependent parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LedState {
    /// Logical LED index.
    pub index: u8,
    /// Active effect.
    pub mode: LedMode,
    /// Three big-endian parameters in wire order.
    pub parameters: [u16; 3],
}

impl LedState {
    fn decode(index: u8, bytes: &[u8]) -> Result<Self, Hidpp20Error> {
        if bytes.len() < 9 || bytes[0] != index {
            return Err(Hidpp20Error::UnsupportedResponse);
        }
        Ok(Self {
            index,
            // The mode is little-endian, unlike capabilities and effect parameters.
            mode: LedMode::try_from(u16::from_le_bytes([bytes[1], bytes[2]]))
                .map_err(|_| Hidpp20Error::UnsupportedResponse)?,
            parameters: std::array::from_fn(|i| {
                u16::from_be_bytes([bytes[3 + i * 2], bytes[4 + i * 2]])
            }),
        })
    }
}

/// Access to monochrome LED descriptors and current software-control state.
#[derive(Clone, Feature)]
#[creatable(id = 0x1300, version = 0)]
pub struct MonochromeLedFeature {
    endpoint: FeatureEndpoint,
}

impl MonochromeLedFeature {
    /// Read the number of logical LED groups.
    pub async fn count(&self) -> Result<u8, Hidpp20Error> {
        let reply = self.endpoint.call(0, [0; 3]).await?;
        tracing::debug!(?reply, "monochrome LED count");
        Ok(reply.extend_payload()[0])
    }

    /// Read one logical LED descriptor.
    pub async fn info(&self, index: u8) -> Result<LedInfo, Hidpp20Error> {
        let reply = self.endpoint.call(1, [index, 0, 0]).await?;
        tracing::debug!(index, ?reply, "monochrome LED descriptor");
        LedInfo::decode(index, &reply.long_payload()?)
    }

    /// Read whether the host currently owns LED control.
    pub async fn software_control(&self) -> Result<bool, Hidpp20Error> {
        let reply = self.endpoint.call(2, [0; 3]).await?;
        tracing::debug!(?reply, "monochrome LED ownership");
        match reply.extend_payload()[0] {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(Hidpp20Error::UnsupportedResponse),
        }
    }

    /// Read one logical LED's current effect.
    pub async fn state(&self, index: u8) -> Result<LedState, Hidpp20Error> {
        let reply = self.endpoint.call(4, [index, 0, 0]).await?;
        tracing::debug!(index, ?reply, "monochrome LED state");
        LedState::decode(index, &reply.long_payload()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monochrome_led_descriptors_keep_unknown_capability_bits() {
        let bytes = [2, 4, 1, 0x80, 0x83, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let info = LedInfo::decode(2, &bytes).unwrap();
        assert_eq!(info.kind, LedKind::Logo);
        assert_eq!(info.physical_count, 1);
        assert_eq!(info.modes.bits(), 0x8083);
        assert_eq!(info.nvconfig_caps, 3);
        assert!(LedInfo::decode(1, &bytes).is_err(), "wrong LED index");
        assert!(LedInfo::decode(2, &bytes[..3]).is_err(), "short reply");
        let mut unknown_kind = bytes;
        unknown_kind[1] = 0xff;
        assert!(LedInfo::decode(2, &unknown_kind).is_err(), "unknown kind");
    }

    #[test]
    fn monochrome_led_state_uses_little_endian_mode_and_big_endian_parameters() {
        let bytes = [2, 0x80, 0, 0, 75, 3, 0xe8, 1, 0x2c, 0, 0, 0, 0, 0, 0, 0];
        let state = LedState::decode(2, &bytes).unwrap();
        assert_eq!(state.mode, LedMode::Breathing);
        assert_eq!(state.parameters, [75, 1000, 300]);
        assert!(LedState::decode(1, &bytes).is_err(), "wrong LED index");
        assert!(LedState::decode(2, &bytes[..3]).is_err(), "short reply");
        let mut unknown_mode = bytes;
        unknown_mode[1..3].copy_from_slice(&0x8000_u16.to_le_bytes());
        assert!(LedState::decode(2, &unknown_mode).is_err(), "unknown mode");
    }
}
