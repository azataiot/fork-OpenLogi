//! Validated reads and bounded edits of HID++ onboard profile memory.

use num_enum::TryFromPrimitive;
use openlogi_hidpp_derive::Feature;

use crate::{feature::FeatureEndpoint, protocol::v20::Hidpp20Error};

// These layouts are reverse-engineered, not from a Logitech byte-level spec.
// References: libratbag src/hidpp20.c and Solaar lib/logitech_receiver/hidpp20.py.
mod edit;
mod write;

pub use edit::{AssignmentEdit, ProfileAssignment, ProfileChange, ProfileEdit, ProfileEditError};
pub use write::{ProfileWriteError, ProfileWriteOutcome, ProfileWriteStage};

const PROFILE_SIZE: usize = 256;
const ASSIGNMENTS_OFFSET: usize = 32;
const ASSIGNMENT_SIZE: usize = 4;
const MAX_BUTTONS: u8 = 16;

/// The memory layout advertised by feature `0x8100`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ProfileDescription {
    /// Memory layout identifier.
    pub memory_model: u8,
    /// Stored profile format identifier.
    pub profile_format: u8,
    /// Stored macro format identifier.
    pub macro_format: u8,
    /// Number of user profile slots.
    pub profile_count: u8,
    /// Number of factory profile slots.
    pub rom_profile_count: u8,
    /// Number of button assignments per profile.
    pub button_count: u8,
    /// Number of user memory sectors, including the directory.
    pub sector_count: u8,
    /// Bytes per memory sector.
    pub sector_size: u16,
    /// Raw mechanical layout flags.
    pub mechanical_layout: u8,
    /// Raw device information flags.
    pub various_info: u8,
}

impl ProfileDescription {
    fn decode(payload: &[u8]) -> Result<Self, Hidpp20Error> {
        let payload: &[u8; 16] = payload
            .try_into()
            .map_err(|_| Hidpp20Error::UnsupportedResponse)?;
        Ok(Self {
            memory_model: payload[0],
            profile_format: payload[1],
            macro_format: payload[2],
            profile_count: payload[3],
            rom_profile_count: payload[4],
            button_count: payload[5],
            sector_count: payload[6],
            sector_size: u16::from_be_bytes([payload[7], payload[8]]),
            mechanical_layout: payload[9],
            various_info: payload[10],
        })
    }

    /// Rejects formats and memory bounds that this reader cannot interpret.
    pub fn validate_layout(&self) -> Result<(), Hidpp20Error> {
        if self.memory_model != 1
            || self.profile_format != 1
            || self.macro_format != 1
            || usize::from(self.sector_size) < PROFILE_SIZE
            || self.profile_count == 0
            || self.profile_count >= self.sector_count
            || usize::from(self.profile_count) * 4 > usize::from(self.sector_size) - 2
            || !(1..=MAX_BUTTONS).contains(&self.button_count)
        {
            return Err(Hidpp20Error::UnsupportedResponse);
        }
        Ok(())
    }
}

/// The current source of device configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive)]
#[repr(u8)]
pub enum ProfileMode {
    /// The device applies its stored profile.
    Onboard = 1,
    /// The host controls the device configuration.
    Host = 2,
}

/// One validated user profile directory entry.
#[derive(Debug, Clone, Copy)]
pub struct ProfileDirectoryEntry {
    /// User memory sector containing the profile.
    pub sector: u16,
    /// Whether the slot is enabled.
    pub enabled: bool,
}

/// A checksum-validated format-1 profile with uninterpreted assignment bytes.
#[derive(Debug, Clone)]
pub struct ProfileData {
    /// Complete sector bytes, including unknown fields and checksum.
    pub raw: Vec<u8>,
    /// Stored report interval in milliseconds.
    pub report_interval_ms: u8,
    /// Stored default DPI stage index.
    pub default_dpi_index: u8,
    /// Stored shifted DPI stage index.
    pub shift_dpi_index: u8,
    /// Five stored DPI values in profile order.
    pub dpi_stages: [u16; 5],
    /// Raw four-byte assignments in physical button order.
    pub assignments: Vec<[u8; ASSIGNMENT_SIZE]>,
}

impl ProfileData {
    /// Decode a complete sector only after validating its layout and checksum.
    pub fn decode(raw: Vec<u8>, description: &ProfileDescription) -> Result<Self, Hidpp20Error> {
        description.validate_layout()?;
        validate_sector(&raw, description)?;
        let dpi_stages = std::array::from_fn(|index| {
            let offset = 3 + index * 2;
            u16::from_le_bytes([raw[offset], raw[offset + 1]])
        });
        let assignments = (0..usize::from(description.button_count))
            .map(|index| {
                let offset = ASSIGNMENTS_OFFSET + index * ASSIGNMENT_SIZE;
                std::array::from_fn(|byte| raw[offset + byte])
            })
            .collect();
        Ok(Self {
            report_interval_ms: raw[0],
            default_dpi_index: raw[1],
            shift_dpi_index: raw[2],
            raw,
            dpi_stages,
            assignments,
        })
    }
}

/// Validated profile access for feature `0x8100`.
#[derive(Clone, Feature)]
#[creatable(id = 0x8100, version = 0)]
pub struct OnboardProfilesFeature {
    endpoint: FeatureEndpoint,
}

impl OnboardProfilesFeature {
    /// Reads the descriptor without changing the device mode.
    pub async fn description(&self) -> Result<ProfileDescription, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.long_payload()?;
        ProfileDescription::decode(&payload)
    }

    /// Reads the active configuration mode.
    pub async fn mode(&self) -> Result<ProfileMode, Hidpp20Error> {
        let payload = self.endpoint.call(2, [0; 3]).await?.extend_payload();
        ProfileMode::try_from(payload[0]).map_err(|_| Hidpp20Error::UnsupportedResponse)
    }

    /// Reads the active profile sector without changing it.
    pub async fn active_sector(&self) -> Result<u16, Hidpp20Error> {
        let payload = self.endpoint.call(4, [0; 3]).await?.extend_payload();
        Ok(u16::from_be_bytes([payload[0], payload[1]]))
    }

    /// Reads and validates the user profile directory.
    pub async fn directory(
        &self,
        description: &ProfileDescription,
    ) -> Result<Vec<ProfileDirectoryEntry>, Hidpp20Error> {
        let bytes = self.read_sector(0, description).await?;
        decode_directory(&bytes, description)
    }

    /// Reads one user profile and retains all of its original bytes.
    pub async fn profile(
        &self,
        sector: u16,
        description: &ProfileDescription,
    ) -> Result<ProfileData, Hidpp20Error> {
        ProfileData::decode(self.raw_profile(sector, description).await?, description)
    }

    /// Read bounded profile bytes for recovery inspection without accepting their checksum.
    pub async fn raw_profile(
        &self,
        sector: u16,
        description: &ProfileDescription,
    ) -> Result<Vec<u8>, Hidpp20Error> {
        if sector == 0 {
            return Err(Hidpp20Error::UnsupportedResponse);
        }
        self.read_sector(sector, description).await
    }

    async fn read_sector(
        &self,
        sector: u16,
        description: &ProfileDescription,
    ) -> Result<Vec<u8>, Hidpp20Error> {
        description.validate_layout()?;
        if sector >= u16::from(description.sector_count) {
            return Err(Hidpp20Error::UnsupportedResponse);
        }
        let mut bytes = vec![0; usize::from(description.sector_size)];
        for offset in (0..description.sector_size).step_by(16) {
            // The final 16-byte read must stay inside a non-aligned sector.
            let offset = offset.min(description.sector_size - 16);
            let mut args = [0; 16];
            args[..2].copy_from_slice(&sector.to_be_bytes());
            args[2..4].copy_from_slice(&offset.to_be_bytes());
            let payload = self.endpoint.call_long(5, args).await?.long_payload()?;
            let start = usize::from(offset);
            bytes[start..start + 16].copy_from_slice(&payload);
        }
        Ok(bytes)
    }
}

fn decode_directory(
    bytes: &[u8],
    description: &ProfileDescription,
) -> Result<Vec<ProfileDirectoryEntry>, Hidpp20Error> {
    description.validate_layout()?;
    validate_sector(bytes, description)?;
    let mut entries: Vec<ProfileDirectoryEntry> = Vec::new();
    for chunk in bytes[..usize::from(description.profile_count) * 4]
        .as_chunks::<4>()
        .0
    {
        let sector = u16::from_be_bytes([chunk[0], chunk[1]]);
        if sector == u16::MAX {
            break;
        }
        if sector == 0
            || sector >= u16::from(description.sector_count)
            || chunk[2] > 1
            || entries.iter().any(|entry| entry.sector == sector)
        {
            return Err(Hidpp20Error::UnsupportedResponse);
        }
        entries.push(ProfileDirectoryEntry {
            sector,
            enabled: chunk[2] == 1,
        });
    }
    Ok(entries)
}

fn validate_sector(bytes: &[u8], description: &ProfileDescription) -> Result<(), Hidpp20Error> {
    if bytes.len() != usize::from(description.sector_size) || bytes.len() < PROFILE_SIZE {
        return Err(Hidpp20Error::UnsupportedResponse);
    }
    let checksum_offset = bytes.len() - 2;
    let expected = u16::from_be_bytes([bytes[checksum_offset], bytes[checksum_offset + 1]]);
    if crc_ccitt(&bytes[..checksum_offset]) != expected {
        return Err(Hidpp20Error::UnsupportedResponse);
    }
    Ok(())
}

fn crc_ccitt(bytes: &[u8]) -> u16 {
    let mut crc = 0xffff;
    for byte in bytes {
        crc ^= u16::from(*byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 == 0 {
                crc << 1
            } else {
                (crc << 1) ^ 0x1021
            };
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn description() -> ProfileDescription {
        ProfileDescription::decode(&[1, 1, 1, 2, 1, 8, 3, 1, 0, 0, 1, 0, 0, 0, 0, 0])
            .expect("valid descriptor")
    }

    fn seal<const N: usize>(mut bytes: [u8; N]) -> [u8; N] {
        let crc = crc_ccitt(&bytes[..N - 2]);
        bytes[N - 2..].copy_from_slice(&crc.to_be_bytes());
        bytes
    }

    #[test]
    fn descriptor_uses_big_endian_sector_size_and_rejects_truncation() {
        assert_eq!(description().sector_size, 256);
        assert_eq!(description().button_count, 8);
        assert!(
            ProfileDescription::decode(&[1; 3]).is_err(),
            "short reply must fail"
        );
    }

    #[test]
    fn descriptor_accepts_sector_sizes_larger_than_the_profile_layout() {
        let descriptor = ProfileDescription {
            sector_size: 1024,
            ..description()
        };
        descriptor
            .validate_layout()
            .expect("1024-byte sectors contain a format-1 profile");
    }

    #[test]
    fn unknown_layout_and_impossible_geometry_cannot_read_memory() {
        let descriptor = description();
        descriptor.validate_layout().expect("supported layout");
        for invalid in [
            ProfileDescription {
                profile_format: 9,
                ..descriptor
            },
            ProfileDescription {
                memory_model: 9,
                ..descriptor
            },
            ProfileDescription {
                macro_format: 9,
                ..descriptor
            },
            ProfileDescription {
                sector_size: 15,
                ..descriptor
            },
            ProfileDescription {
                profile_count: 0,
                ..descriptor
            },
            ProfileDescription {
                profile_count: 3,
                ..descriptor
            },
            ProfileDescription {
                button_count: 17,
                ..descriptor
            },
        ] {
            assert!(
                invalid.validate_layout().is_err(),
                "invalid descriptor {invalid:?}"
            );
        }
    }

    #[test]
    fn checksum_matches_the_standard_check_vector() {
        assert_eq!(crc_ccitt(b"123456789"), 0x29b1);
    }

    #[test]
    fn mode_rejects_unknown_wire_values() {
        assert_eq!(
            ProfileMode::try_from(1).expect("onboard mode"),
            ProfileMode::Onboard
        );
        assert_eq!(
            ProfileMode::try_from(2).expect("host mode"),
            ProfileMode::Host
        );
        assert!(ProfileMode::try_from(0).is_err(), "unknown mode");
    }

    #[test]
    fn directory_rejects_duplicates_directory_references_and_unknown_flags() {
        for entry in [[0, 1, 1, 0], [0, 0, 1, 0], [0, 2, 2, 0]] {
            let mut bytes = [0xff; 256];
            bytes[..4].copy_from_slice(&[0, 1, 1, 0]);
            bytes[4..8].copy_from_slice(&entry);
            assert!(
                decode_directory(&seal(bytes), &description()).is_err(),
                "invalid directory entry {entry:?}"
            );
        }
    }

    #[test]
    fn directory_rejects_corruption_and_out_of_range_sectors() {
        let mut bytes = [0xff; 256];
        bytes[..8].copy_from_slice(&[0, 1, 1, 0, 0xff, 0xff, 0, 0]);
        let mut bytes = seal(bytes);
        let entries = decode_directory(&bytes, &description()).expect("valid directory");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].sector, 1);
        assert!(entries[0].enabled);
        bytes[0] ^= 1;
        assert!(
            decode_directory(&bytes, &description()).is_err(),
            "corrupt CRC"
        );
        let bytes = seal(bytes);
        assert!(
            decode_directory(&bytes, &description()).is_err(),
            "invalid sector"
        );
    }

    #[test]
    fn profile_keeps_assignment_bytes_and_decodes_little_endian_dpi() {
        let mut bytes = [0; 256];
        bytes[0] = 2;
        bytes[3..5].copy_from_slice(&1200u16.to_le_bytes());
        bytes[32..36].copy_from_slice(&[0x80, 0, 0, 8]);
        let bytes = seal(bytes);
        let profile = ProfileData::decode(bytes.to_vec(), &description()).expect("valid profile");
        assert_eq!(profile.dpi_stages[0], 1200);
        assert_eq!(profile.report_interval_ms, 2);
        assert_eq!(profile.assignments[0], [0x80, 0, 0, 8]);
        assert_eq!(profile.raw, bytes);
        let mut corrupted = bytes;
        corrupted[32] ^= 1;
        assert!(
            ProfileData::decode(corrupted.to_vec(), &description()).is_err(),
            "corrupt assignment"
        );
    }
}
