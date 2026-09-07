use std::sync::Arc;

mod session;
pub use session::{OnboardProfileSession, read_onboard_profile_on};

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::onboard_profiles::{
        OnboardProfilesFeature, ProfileData, ProfileDescription, ProfileDirectoryEntry, ProfileMode,
    },
};

use super::{WriteError, open_feature, with_route};
use crate::{DeviceRoute, backend::HidBackend};

/// A read-only snapshot of the active mode and stored user profiles.
#[derive(Debug)]
pub struct OnboardProfilesSnapshot {
    /// Device-reported memory descriptor.
    pub description: ProfileDescription,
    /// Active configuration mode during the read.
    pub mode: ProfileMode,
    /// Active profile sector during the read.
    pub active_sector: u16,
    /// Validated directory entries paired with their complete profile data.
    pub profiles: Vec<(ProfileDirectoryEntry, ProfileData)>,
}

/// Reads onboard profile memory without mode changes or writes.
pub async fn dump_onboard_profiles(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<OnboardProfilesSnapshot, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| {
        dump_onboard_profiles_on_channel(channel, index)
    })
    .await
}

async fn dump_onboard_profiles_on_channel(
    channel: Arc<HidppChannel>,
    index: u8,
) -> Result<OnboardProfilesSnapshot, WriteError> {
    let mut device = Device::new(channel, index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let feature = open_feature::<OnboardProfilesFeature>(&mut device).await?;
    let description = feature
        .description()
        .await
        .map_err(|error| WriteError::Hidpp(format!("read onboard descriptor: {error:?}")))?;
    description.validate_layout().map_err(|error| {
        WriteError::Hidpp(format!(
            "unsupported onboard descriptor {description:?}: {error:?}"
        ))
    })?;
    let read = async {
        let mode = feature.mode().await?;
        let active_sector = feature.active_sector().await?;
        let mut profiles = Vec::new();
        for entry in feature.directory(&description).await? {
            let data = feature.profile(entry.sector, &description).await?;
            profiles.push((entry, data));
        }
        if feature.mode().await? != mode || feature.active_sector().await? != active_sector {
            return Err(hidpp::protocol::v20::Hidpp20Error::UnsupportedResponse);
        }
        Ok(OnboardProfilesSnapshot {
            description,
            mode,
            active_sector,
            profiles,
        })
    }
    .await;
    read.map_err(|error| {
        WriteError::Hidpp(format!(
            "read onboard profiles with {description:?}: {error:?}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::channel::scripted::{ScriptedRawHidChannel, scripted_channel};

    fn reply(request: &[u8]) -> Option<Vec<u8>> {
        reply_with_sector_size(request, 256)
    }

    pub(super) fn reply_with_sector_size(request: &[u8], sector_size: u16) -> Option<Vec<u8>> {
        let mut payload = [0; 16];
        let long = match (request[2], request[3] >> 4) {
            (0, 1) => {
                payload[0] = 2;
                payload[2] = request[6];
                false
            }
            (0, 0) => {
                if request[4..6] == [0x81, 0] {
                    payload[0] = 7;
                }
                false
            }
            (7, 0) => {
                payload = [1, 1, 1, 1, 1, 3, 2, 1, 0, 0, 1, 0, 0, 0, 0, 0];
                payload[7..9].copy_from_slice(&sector_size.to_be_bytes());
                true
            }
            (7, 2) => {
                payload[0] = 1;
                false
            }
            (7, 4) => {
                payload[1] = 1;
                false
            }
            (7, 5) => {
                let sector = u16::from_be_bytes([request[4], request[5]]);
                let offset = usize::from(u16::from_be_bytes([request[6], request[7]]));
                let mut data = vec![0; usize::from(sector_size)];
                let (directory_crc, profile_crc) = match sector_size {
                    256 => ([0xb9, 0x0d], [0x68, 0x42]),
                    1024 => ([0x4a, 0x04], [0xa7, 0x17]),
                    1025 => ([0xf3, 0x7e], [0xd2, 0x0d]),
                    _ => panic!("unsupported test sector size"),
                };
                let checksum_offset = data.len() - 2;
                match sector {
                    0 => {
                        data.fill(0xff);
                        data[..8].copy_from_slice(&[0, 1, 1, 0, 0xff, 0xff, 0, 0]);
                        data[checksum_offset..].copy_from_slice(&directory_crc);
                    }
                    1 => {
                        data[0] = 2;
                        data[3..5].copy_from_slice(&1200u16.to_le_bytes());
                        data[32..36].copy_from_slice(&[0x80, 0, 0, 8]);
                        data[checksum_offset..].copy_from_slice(&profile_crc);
                    }
                    _ => panic!("unexpected sector {sector}"),
                }
                payload.copy_from_slice(&data[offset..offset + 16]);
                true
            }
            _ => return None,
        };
        let mut response = vec![0; if long { 20 } else { 7 }];
        response[0] = if long { 0x11 } else { 0x10 };
        response[1..4].copy_from_slice(&request[1..4]);
        let length = response.len() - 4;
        response[4..].copy_from_slice(&payload[..length]);
        Some(response)
    }

    #[tokio::test]
    async fn larger_and_unaligned_sectors_keep_all_bytes_and_validate_the_tail_checksum() {
        for size in [1024, 1025] {
            let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
                reply_with_sector_size(request, size)
            });
            let channel = scripted_channel(raw).await;
            let snapshot = dump_onboard_profiles_on_channel(channel, 0xff)
                .await
                .expect("complete sector snapshot");
            assert_eq!(snapshot.profiles[0].1.raw.len(), usize::from(size));
            assert_eq!(snapshot.profiles[0].1.dpi_stages[0], 1200);
            let reports = handle.written_reports();
            let offsets: Vec<_> = reports
                .iter()
                .filter(|report| report[2] == 7 && report[3] >> 4 == 5)
                .map(|report| u16::from_be_bytes([report[6], report[7]]))
                .collect();
            assert_eq!(offsets.len(), usize::from(size).div_ceil(16) * 2);
            assert_eq!(offsets.last(), Some(&(size - 16)));
        }
    }

    #[tokio::test]
    async fn snapshot_reads_profiles_without_mode_or_memory_writes() {
        let (raw, handle) = ScriptedRawHidChannel::with_responder(reply);
        let channel = scripted_channel(raw).await;
        let snapshot = dump_onboard_profiles_on_channel(channel, 0xff)
            .await
            .expect("profile snapshot");
        assert_eq!(snapshot.profiles.len(), 1);
        assert_eq!(snapshot.profiles[0].1.dpi_stages[0], 1200);
        assert_eq!(snapshot.profiles[0].1.assignments[0], [0x80, 0, 0, 8]);
        let reports = handle.written_reports();
        let profile_reports: Vec<_> = reports.iter().filter(|report| report[2] == 7).collect();
        assert_eq!(profile_reports.len(), 37);
        assert!(
            profile_reports
                .iter()
                .all(|report| matches!(report[3] >> 4, 0 | 2 | 4 | 5))
        );
        assert_eq!(
            profile_reports
                .iter()
                .filter(|report| report[3] >> 4 == 5)
                .count(),
            32
        );
    }

    #[tokio::test]
    async fn unknown_profile_format_fails_before_memory_access() {
        let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(|request| {
            let mut response = reply(request)?;
            if request[2] == 7 && request[3] >> 4 == 0 {
                response[5] = 9;
            }
            Some(response)
        });
        let channel = scripted_channel(raw).await;
        let error = dump_onboard_profiles_on_channel(channel, 0xff)
            .await
            .expect_err("unsupported format");
        assert!(error.to_string().contains("profile_format: 9"));
        assert!(
            handle
                .written_reports()
                .iter()
                .filter(|report| report[2] == 7)
                .all(|report| report[3] >> 4 == 0)
        );
    }

    #[tokio::test]
    async fn short_memory_reply_is_not_zero_padded_into_a_profile() {
        let (raw, _) = ScriptedRawHidChannel::with_dynamic_responder(|request| {
            let mut response = reply(request)?;
            if request[2] == 7 && request[3] >> 4 == 5 {
                response.truncate(7);
                response[0] = 0x10;
            }
            Some(response)
        });
        let channel = scripted_channel(raw).await;
        dump_onboard_profiles_on_channel(channel, 0xff)
            .await
            .expect_err("short memory reply");
    }

    #[tokio::test]
    async fn a_mode_change_during_the_read_rejects_the_snapshot() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let mode_reads = AtomicUsize::new(0);
        let (raw, _) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
            let mut response = reply(request)?;
            if request[2] == 7
                && request[3] >> 4 == 2
                && mode_reads.fetch_add(1, Ordering::Relaxed) > 0
            {
                response[4] = 2;
            }
            Some(response)
        });
        let channel = scripted_channel(raw).await;
        dump_onboard_profiles_on_channel(channel, 0xff)
            .await
            .expect_err("mode changed during read");
    }

    #[tokio::test]
    async fn disconnect_during_memory_read_returns_an_error() {
        let (raw, _) = ScriptedRawHidChannel::with_failing_writes(reply, |request| {
            request[2] == 7 && request[3] >> 4 == 5
        });
        let channel = scripted_channel(raw).await;
        tokio::time::timeout(
            Duration::from_secs(5),
            dump_onboard_profiles_on_channel(channel, 0xff),
        )
        .await
        .expect("bounded diagnostic")
        .expect_err("disconnected device");
    }
}

#[cfg(test)]
mod write_tests;
