use thiserror::Error;

use super::{OnboardProfilesFeature, ProfileChange, ProfileMode};
use crate::protocol::v20::Hidpp20Error;

/// The flash transaction phase that did not complete reliably.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileWriteStage {
    /// Establish the sector, address, and byte count.
    Begin,
    /// Transfer one bounded data block.
    Data {
        /// Offset of the block within the sector.
        offset: u16,
    },
    /// Complete the transaction.
    Commit,
    /// Read and validate the complete result.
    Readback,
}

/// A rejected edit or an uncertain flash transaction.
#[derive(Debug, Error)]
pub enum ProfileWriteError {
    /// A read failed before the first write command.
    #[error("profile preflight read failed: {0:?}")]
    Read(#[source] Hidpp20Error),
    /// The live descriptor differs from the edit's descriptor.
    #[error("profile descriptor changed before Apply")]
    StaleDescriptor,
    /// The target is not the enabled active onboard profile.
    #[error("target must be the enabled active onboard profile")]
    InactiveProfile,
    /// The complete current sector differs from the edit's original.
    #[error("profile changed before Apply; read it again")]
    StaleProfile,
    /// A command failed after flash writing could have started.
    #[error(
        "profile write outcome uncertain at {stage:?}: {source:?}; read state before further action"
    )]
    Uncertain {
        /// Phase containing the failed command.
        stage: ProfileWriteStage,
        /// Transport or firmware failure.
        #[source]
        source: Hidpp20Error,
    },
    /// Readback did not match the intended sector or active profile.
    #[error("profile write readback mismatch; do not retry automatically")]
    VerificationFailed,
}

/// Whether Apply needed to send any flash write commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileWriteOutcome {
    /// The original and intended sector were identical.
    Unchanged,
    /// Every byte read back matched the intended sector.
    Written,
}

impl OnboardProfilesFeature {
    /// Apply a validated change to the active profile with freshness checks and full readback.
    pub async fn write_profile(
        &self,
        sector: u16,
        change: &ProfileChange,
    ) -> Result<ProfileWriteOutcome, ProfileWriteError> {
        self.check_profile_preflight(sector, change).await?;
        if change.before() == change.after().raw {
            return Ok(ProfileWriteOutcome::Unchanged);
        }

        // The transaction layout is reverse-engineered from libratbag and Solaar.
        let mut args = [0; 16];
        args[..2].copy_from_slice(&sector.to_be_bytes());
        args[4..6].copy_from_slice(&change.description.sector_size.to_be_bytes());
        self.endpoint
            .call_long(6, args)
            .await
            .map_err(|source| ProfileWriteError::Uncertain {
                stage: ProfileWriteStage::Begin,
                source,
            })?;
        for offset in (0..change.description.sector_size).step_by(16) {
            let start = usize::from(offset);
            let mut block = [0; 16];
            block.copy_from_slice(&change.after().raw[start..start + 16]);
            self.endpoint.call_long(7, block).await.map_err(|source| {
                ProfileWriteError::Uncertain {
                    stage: ProfileWriteStage::Data { offset },
                    source,
                }
            })?;
        }
        self.endpoint
            .call(8, [0; 3])
            .await
            .map_err(|source| ProfileWriteError::Uncertain {
                stage: ProfileWriteStage::Commit,
                source,
            })?;

        let readback = async {
            let actual = self.profile(sector, &change.description).await?;
            let active =
                self.mode().await? == ProfileMode::Onboard && self.active_sector().await? == sector;
            Ok::<_, Hidpp20Error>(actual.raw == change.after().raw && active)
        }
        .await
        .map_err(|source| ProfileWriteError::Uncertain {
            stage: ProfileWriteStage::Readback,
            source,
        })?;
        if !readback {
            return Err(ProfileWriteError::VerificationFailed);
        }
        Ok(ProfileWriteOutcome::Written)
    }

    async fn check_profile_preflight(
        &self,
        sector: u16,
        change: &ProfileChange,
    ) -> Result<(), ProfileWriteError> {
        let description = self.description().await.map_err(ProfileWriteError::Read)?;
        if description != change.description {
            return Err(ProfileWriteError::StaleDescriptor);
        }
        let directory = self
            .directory(&description)
            .await
            .map_err(ProfileWriteError::Read)?;
        if !directory
            .iter()
            .any(|entry| entry.enabled && entry.sector == sector)
            || self.mode().await.map_err(ProfileWriteError::Read)? != ProfileMode::Onboard
            || self
                .active_sector()
                .await
                .map_err(ProfileWriteError::Read)?
                != sector
        {
            return Err(ProfileWriteError::InactiveProfile);
        }
        let current = self
            .raw_profile(sector, &description)
            .await
            .map_err(ProfileWriteError::Read)?;
        if current != change.before() {
            return Err(ProfileWriteError::StaleProfile);
        }
        if self.mode().await.map_err(ProfileWriteError::Read)? != ProfileMode::Onboard
            || self
                .active_sector()
                .await
                .map_err(ProfileWriteError::Read)?
                != sector
        {
            return Err(ProfileWriteError::InactiveProfile);
        }
        Ok(())
    }
}
