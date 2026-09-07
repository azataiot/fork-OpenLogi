use std::sync::{Arc, Weak};

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::{
        onboard_profiles::{
            AssignmentEdit, OnboardProfilesFeature, ProfileAssignment, ProfileChange, ProfileData,
            ProfileDescription, ProfileEdit, ProfileMode, ProfileWriteError, ProfileWriteOutcome,
        },
        report_rate::{ReportRateFeature, ReportRateList},
    },
};
use openlogi_core::hid::onboard_profile::{
    OnboardAssignment, OnboardProfileBackup, OnboardProfileContents, OnboardProfileDescriptor,
    OnboardProfileEdit, OnboardProfileFailure, OnboardProfileSettings, OnboardProfileView,
    ProfileEditId, StoredOnboardAssignment,
};

use super::super::{WriteError, open_feature};
use crate::{DeviceRoute, SharedChannel};

/// A bounded profile read borrowed from one live connection.
pub struct OnboardProfileSession {
    connection: Weak<HidppChannel>,
    route: DeviceRoute,
    description: ProfileDescription,
    sector: u16,
    bytes: Vec<u8>,
    supported_intervals: ReportRateList,
    active_report_interval_ms: Result<u8, String>,
}

/// Read the active profile, keeping invalid bytes available only for explicit recovery.
pub async fn read_onboard_profile_on(
    shared: &SharedChannel,
) -> Result<OnboardProfileSession, WriteError> {
    let mut device = Device::new(Arc::clone(shared.channel()), shared.device_index())
        .await
        .map_err(|_| WriteError::DeviceUnreachable {
            index: shared.device_index(),
        })?;
    let feature = open_feature::<OnboardProfilesFeature>(&mut device).await?;
    let rate = open_feature::<ReportRateFeature>(&mut device).await?;
    let read = async {
        let description = feature.description().await?;
        description.validate_layout()?;
        let sector = feature.active_sector().await?;
        let directory = feature.directory(&description).await?;
        if feature.mode().await? != ProfileMode::Onboard
            || !directory
                .iter()
                .any(|entry| entry.enabled && entry.sector == sector)
        {
            return Err(hidpp::protocol::v20::Hidpp20Error::UnsupportedResponse);
        }
        let bytes = feature.raw_profile(sector, &description).await?;
        let supported_intervals = rate.get_report_rate_list().await?;
        let active_report_interval_ms = rate
            .get_report_rate()
            .await
            .and_then(|interval| {
                if (1..=8).contains(&interval)
                    && supported_intervals.bits() & (1 << (interval - 1)) != 0
                {
                    Ok(interval)
                } else {
                    Err(hidpp::protocol::v20::Hidpp20Error::UnsupportedResponse)
                }
            })
            .map_err(|error| format!("{error:?}"));
        if feature.mode().await? != ProfileMode::Onboard || feature.active_sector().await? != sector
        {
            return Err(hidpp::protocol::v20::Hidpp20Error::UnsupportedResponse);
        }
        Ok(OnboardProfileSession {
            connection: Arc::downgrade(shared.channel()),
            route: shared.route().clone(),
            description,
            sector,
            bytes,
            supported_intervals,
            active_report_interval_ms,
        })
    }
    .await;
    read.map_err(|error| WriteError::Hidpp(format!("read onboard profile: {error:?}")))
}

impl OnboardProfileSession {
    /// Convert the validated read to an IPC view without exposing writable raw memory.
    #[must_use]
    pub fn view(&self, edit_id: ProfileEditId) -> OnboardProfileView {
        let contents = match ProfileData::decode(self.bytes.clone(), &self.description) {
            Ok(profile) => OnboardProfileContents::Valid(OnboardProfileSettings {
                report_interval_ms: profile.report_interval_ms,
                dpi_stages: profile.dpi_stages,
                default_dpi_index: profile.default_dpi_index,
                shift_dpi_index: profile.shift_dpi_index,
                assignments: profile
                    .assignments
                    .into_iter()
                    .map(|bytes| {
                        ProfileAssignment::decode(bytes).map_or(
                            StoredOnboardAssignment::Unknown(bytes),
                            |assignment| {
                                StoredOnboardAssignment::Known(assignment_to_core(assignment))
                            },
                        )
                    })
                    .collect(),
            }),
            Err(_) => OnboardProfileContents::InvalidProfile,
        };
        OnboardProfileView {
            edit_id,
            sector: self.sector,
            description: descriptor_to_core(self.description),
            supported_intervals_ms: (1..=8)
                .filter(|ms| self.supported_intervals.bits() & (1 << (ms - 1)) != 0)
                .collect(),
            contents,
            backups: Vec::new(),
            active_report_interval_ms: self.active_report_interval_ms.clone(),
        }
    }

    /// Whether the authoritative channel is still the preparation's connection.
    #[must_use]
    pub fn matches(&self, shared: &SharedChannel) -> bool {
        shared.matches(&self.route) && self.connection.ptr_eq(&Arc::downgrade(shared.channel()))
    }

    /// Validate editor input against this connection's source bytes and capabilities.
    pub fn prepare(
        &self,
        shared: &SharedChannel,
        edit: &OnboardProfileEdit,
    ) -> Result<ProfileChange, WriteError> {
        self.check_connection(shared)?;
        let original =
            ProfileData::decode(self.bytes.clone(), &self.description).map_err(|error| {
                failure(
                    OnboardProfileFailure::InvalidEdit,
                    format!("profile requires recovery: {error:?}"),
                )
            })?;
        let edit = ProfileEdit {
            report_interval_ms: edit.report_interval_ms,
            assignments: edit
                .assignments
                .iter()
                .map(|edit| AssignmentEdit {
                    index: edit.index,
                    assignment: assignment_from_core(edit.assignment),
                })
                .collect(),
        };
        ProfileChange::new(&original, self.description, &edit, self.supported_intervals)
            .map_err(|error| failure(OnboardProfileFailure::InvalidEdit, error.to_string()))
    }

    /// Whether this backup can restore this explicit read without overwriting an unrelated valid profile.
    #[must_use]
    pub fn can_restore(&self, backup: &OnboardProfileBackup) -> bool {
        backup.route == self.route
            && backup.sector == self.sector
            && backup.description == descriptor_to_core(self.description)
            && ProfileData::decode(backup.original.clone(), &self.description).is_ok()
            && (self.bytes == backup.updated
                || self.bytes == backup.original
                || ProfileData::decode(self.bytes.clone(), &self.description).is_err())
    }

    /// Prepare an exact restore from a validated server-owned backup.
    pub fn prepare_restore(
        &self,
        shared: &SharedChannel,
        backup: &OnboardProfileBackup,
    ) -> Result<ProfileChange, WriteError> {
        self.check_connection(shared)?;
        if !self.can_restore(backup) {
            return Err(failure(
                OnboardProfileFailure::StaleProfile,
                "backup does not match this profile state",
            ));
        }
        ProfileChange::for_restore(&self.bytes, &backup.original, self.description)
            .map_err(|error| failure(OnboardProfileFailure::Backup, error.to_string()))
    }

    /// Build the immutable record that must reach durable storage before writing.
    #[must_use]
    pub fn backup(
        &self,
        change: &ProfileChange,
        created_unix_seconds: u64,
    ) -> OnboardProfileBackup {
        OnboardProfileBackup {
            format_version: 1,
            created_unix_seconds,
            route: self.route.clone(),
            description: descriptor_to_core(self.description),
            sector: self.sector,
            original: change.before().to_vec(),
            updated: change.after().raw.clone(),
        }
    }

    /// Apply once through the same authoritative connection after the caller has saved its backup.
    pub async fn apply(
        &self,
        shared: &SharedChannel,
        change: &ProfileChange,
    ) -> Result<ProfileWriteOutcome, WriteError> {
        self.check_connection(shared)?;
        if change.before() != self.bytes {
            return Err(failure(
                OnboardProfileFailure::StaleProfile,
                "change belongs to a different read",
            ));
        }
        let mut device = Device::new(Arc::clone(shared.channel()), shared.device_index())
            .await
            .map_err(|_| WriteError::DeviceUnreachable {
                index: shared.device_index(),
            })?;
        let feature = open_feature::<OnboardProfilesFeature>(&mut device).await?;
        feature
            .write_profile(self.sector, change)
            .await
            .map_err(|error| {
                let kind = match &error {
                    ProfileWriteError::Uncertain { .. } | ProfileWriteError::VerificationFailed => {
                        OnboardProfileFailure::Uncertain
                    }
                    _ => OnboardProfileFailure::StaleProfile,
                };
                failure(kind, error.to_string())
            })
    }

    fn check_connection(&self, shared: &SharedChannel) -> Result<(), WriteError> {
        if !self.matches(shared) {
            return Err(failure(
                OnboardProfileFailure::StaleSession,
                "device connection changed; read the profile again",
            ));
        }
        Ok(())
    }
}

fn failure(kind: OnboardProfileFailure, message: impl Into<String>) -> WriteError {
    WriteError::OnboardProfile {
        kind,
        message: message.into(),
        backup_id: None,
    }
}

fn descriptor_to_core(value: ProfileDescription) -> OnboardProfileDescriptor {
    OnboardProfileDescriptor {
        memory_model: value.memory_model,
        profile_format: value.profile_format,
        macro_format: value.macro_format,
        profile_count: value.profile_count,
        rom_profile_count: value.rom_profile_count,
        button_count: value.button_count,
        sector_count: value.sector_count,
        sector_size: value.sector_size,
        mechanical_layout: value.mechanical_layout,
        various_info: value.various_info,
    }
}

fn assignment_to_core(value: ProfileAssignment) -> OnboardAssignment {
    match value {
        ProfileAssignment::MousePrimary => OnboardAssignment::MousePrimary,
        ProfileAssignment::MouseSecondary => OnboardAssignment::MouseSecondary,
        ProfileAssignment::MouseMiddle => OnboardAssignment::MouseMiddle,
        ProfileAssignment::MouseBack => OnboardAssignment::MouseBack,
        ProfileAssignment::MouseForward => OnboardAssignment::MouseForward,
        ProfileAssignment::DpiNext => OnboardAssignment::DpiNext,
        ProfileAssignment::DpiPrevious => OnboardAssignment::DpiPrevious,
        ProfileAssignment::DpiShift => OnboardAssignment::DpiShift,
    }
}

fn assignment_from_core(value: OnboardAssignment) -> ProfileAssignment {
    match value {
        OnboardAssignment::MousePrimary => ProfileAssignment::MousePrimary,
        OnboardAssignment::MouseSecondary => ProfileAssignment::MouseSecondary,
        OnboardAssignment::MouseMiddle => ProfileAssignment::MouseMiddle,
        OnboardAssignment::MouseBack => ProfileAssignment::MouseBack,
        OnboardAssignment::MouseForward => ProfileAssignment::MouseForward,
        OnboardAssignment::DpiNext => ProfileAssignment::DpiNext,
        OnboardAssignment::DpiPrevious => ProfileAssignment::DpiPrevious,
        OnboardAssignment::DpiShift => ProfileAssignment::DpiShift,
    }
}
