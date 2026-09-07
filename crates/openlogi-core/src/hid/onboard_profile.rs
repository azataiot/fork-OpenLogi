//! Onboard editor requests, validated views, and durable backup data.

use serde::{Deserialize, Serialize};

use super::DeviceRoute;

/// A preparation identifier unique within one agent run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProfileEditId {
    /// Agent run identity.
    pub run: u64,
    /// Monotonic preparation number within the run.
    pub sequence: u64,
}

impl std::fmt::Display for ProfileEditId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:016x}-{:016x}", self.run, self.sequence)
    }
}

/// Memory geometry bound to an onboard edit or backup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardProfileDescriptor {
    /// Memory layout identifier.
    pub memory_model: u8,
    /// Stored profile format.
    pub profile_format: u8,
    /// Stored macro format.
    pub macro_format: u8,
    /// Number of writable profile slots.
    pub profile_count: u8,
    /// Number of factory profile slots.
    pub rom_profile_count: u8,
    /// Physical button count.
    pub button_count: u8,
    /// User sector count including the directory.
    pub sector_count: u8,
    /// Complete sector length.
    pub sector_size: u16,
    /// Mechanical layout flags.
    pub mechanical_layout: u8,
    /// Device information flags.
    pub various_info: u8,
}

/// A supported stored action, separate from host event bindings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OnboardAssignment {
    /// Primary mouse click.
    MousePrimary,
    /// Secondary mouse click.
    MouseSecondary,
    /// Middle mouse click.
    MouseMiddle,
    /// Back mouse button.
    MouseBack,
    /// Forward mouse button.
    MouseForward,
    /// Select the next DPI stage.
    DpiNext,
    /// Select the previous DPI stage.
    DpiPrevious,
    /// Hold the shifted DPI stage.
    DpiShift,
}

/// A supported action or an assignment that the editor must preserve.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StoredOnboardAssignment {
    /// An editable action.
    Known(OnboardAssignment),
    /// An uninterpreted four-byte assignment.
    Unknown([u8; 4]),
}

/// Parsed fields from a checksum-valid profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardProfileSettings {
    /// Stored report interval in milliseconds.
    pub report_interval_ms: u8,
    /// Stored DPI stages, including unused zero entries.
    pub dpi_stages: [u16; 5],
    /// Default DPI index.
    pub default_dpi_index: u8,
    /// Shifted DPI index.
    pub shift_dpi_index: u8,
    /// Assignments in physical button order.
    pub assignments: Vec<StoredOnboardAssignment>,
}

/// Whether the current bytes can safely form the basis of an ordinary edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OnboardProfileContents {
    /// A validated profile.
    Valid(OnboardProfileSettings),
    /// A bounded raw read that requires recovery from a valid backup.
    InvalidProfile,
}

/// One stored backup available for an explicit restore.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardBackupSummary {
    /// The preparation that created this backup.
    pub id: ProfileEditId,
    /// Backup creation time in UTC seconds since the Unix epoch.
    pub created_unix_seconds: u64,
}

/// Agent-owned preparation shown in the onboard editor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardProfileView {
    /// Single-use preparation bound to the live connection.
    pub edit_id: ProfileEditId,
    /// Active user sector.
    pub sector: u16,
    /// Descriptor used for validation.
    pub description: OnboardProfileDescriptor,
    /// Firmware-advertised report intervals in milliseconds.
    pub supported_intervals_ms: Vec<u8>,
    /// Parsed settings or a recoverable invalid profile.
    pub contents: OnboardProfileContents,
    /// Stored backups, newest first.
    pub backups: Vec<OnboardBackupSummary>,
    /// Active report interval, or the reason its read was unavailable.
    pub active_report_interval_ms: Result<u8, String>,
}

/// An explicit replacement for one physical button assignment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardAssignmentEdit {
    /// Zero-based physical button index.
    pub index: u8,
    /// Replacement action.
    pub assignment: OnboardAssignment,
}

/// Changes submitted together by the editor's Apply action.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardProfileEdit {
    /// A stored report interval to replace, or no change.
    pub report_interval_ms: Option<u8>,
    /// Button assignments to replace.
    pub assignments: Vec<OnboardAssignmentEdit>,
}

/// Result of a completed onboard operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OnboardApplyResult {
    /// No byte differed, so no flash write occurred.
    Unchanged,
    /// Full readback matched the saved profile.
    Written {
        /// Backup containing the exact original sector.
        backup_id: ProfileEditId,
    },
}

/// Failure categories that distinguish safe rejection from uncertain writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OnboardProfileFailure {
    /// Invalid or unsupported edits.
    InvalidEdit,
    /// The preparation belongs to an expired or replaced connection.
    StaleSession,
    /// The descriptor, active sector, or original bytes changed.
    StaleProfile,
    /// Required profile capability or mode is unavailable.
    Unsupported,
    /// A backup could not be saved or validated.
    Backup,
    /// A write may have changed flash without confirmed completion.
    Uncertain,
}

/// Durable original and intended sector bytes, never supplied directly by an IPC client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardProfileBackup {
    /// Storage format version.
    pub format_version: u8,
    /// Creation time in UTC seconds since the Unix epoch.
    pub created_unix_seconds: u64,
    /// Explicitly selected hardware route.
    pub route: DeviceRoute,
    /// Descriptor associated with the original read.
    pub description: OnboardProfileDescriptor,
    /// The user profile sector.
    pub sector: u16,
    /// Exact bytes before the operation.
    pub original: Vec<u8>,
    /// Exact intended replacement bytes.
    pub updated: Vec<u8>,
}

/// An invalid backup or a failure to encode its stored representation.
#[derive(Debug, thiserror::Error)]
pub enum OnboardBackupError {
    /// An unsupported version or invalid sector geometry.
    #[error("unsupported profile backup format or geometry")]
    InvalidFormat,
    /// An input exceeds the bounded backup size.
    #[error("profile backup exceeds its size limit")]
    TooLarge,
    /// TOML encoding failed.
    #[error("profile backup encoding failed: {0}")]
    Encode(#[from] toml::ser::Error),
    /// TOML parsing failed.
    #[error("profile backup parsing failed: {0}")]
    Decode(#[from] toml::de::Error),
}

impl OnboardProfileBackup {
    /// Encode a bounded backup for durable storage.
    pub fn encode(&self) -> Result<String, OnboardBackupError> {
        self.validate()?;
        Ok(toml::to_string(self)?)
    }

    /// Parse a bounded backup without treating its bytes as a verified hardware identity.
    pub fn decode(text: &str) -> Result<Self, OnboardBackupError> {
        if text.len() > 1_000_000 {
            return Err(OnboardBackupError::TooLarge);
        }
        let backup: Self = toml::from_str(text)?;
        backup.validate()?;
        Ok(backup)
    }

    fn validate(&self) -> Result<(), OnboardBackupError> {
        let size = usize::from(self.description.sector_size);
        if self.format_version != 1
            || size < 256
            || !size.is_multiple_of(16)
            || self.original.len() != size
            || self.updated.len() != size
            || self.sector == 0
            || self.sector >= u16::from(self.description.sector_count)
        {
            return Err(OnboardBackupError::InvalidFormat);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_backup_preserves_unknown_bytes_and_rejects_bad_geometry_or_versions() {
        let backup = OnboardProfileBackup {
            format_version: 1,
            created_unix_seconds: 1,
            route: DeviceRoute::Direct {
                vendor_id: 0xff00,
                product_id: 0xabcd,
            },
            description: OnboardProfileDescriptor {
                memory_model: 1,
                profile_format: 1,
                macro_format: 1,
                profile_count: 1,
                rom_profile_count: 1,
                button_count: 3,
                sector_count: 2,
                sector_size: 256,
                mechanical_layout: 0,
                various_info: 1,
            },
            sector: 1,
            original: vec![0x5a; 256],
            updated: vec![0xa5; 256],
        };
        let encoded = backup.encode().unwrap();
        assert_eq!(OnboardProfileBackup::decode(&encoded).unwrap(), backup);
        let mut invalid = backup.clone();
        invalid.format_version = 2;
        assert!(invalid.encode().is_err(), "unknown backup version");
        invalid = backup.clone();
        invalid.original.pop();
        assert!(invalid.encode().is_err(), "truncated original");
        invalid = backup;
        invalid.sector = 0;
        assert!(invalid.encode().is_err(), "directory sector");
    }
}
