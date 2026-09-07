use hidpp::{channel::HidppChannel, device::Device, feature::monochrome_led::MonochromeLedFeature};
use std::sync::Arc;

use super::{WriteError, open_feature, with_route};
use crate::{DeviceRoute, backend::HidBackend};
pub use hidpp::feature::monochrome_led::{LedInfo, LedKind, LedMode, LedModes, LedState};

/// Current monochrome LED ownership, descriptors, and effect states.
#[derive(Debug)]
pub struct MonochromeLedSnapshot {
    /// Whether software controls the LED groups.
    pub software_control: bool,
    /// Descriptor and current effect for each logical group.
    pub leds: Vec<(LedInfo, LedState)>,
}

/// Read monochrome LED capabilities and state without changing ownership.
pub async fn dump_monochrome_leds(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<MonochromeLedSnapshot, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| {
        read_on_channel(channel, index)
    })
    .await
}

async fn read_on_channel(
    channel: Arc<HidppChannel>,
    index: u8,
) -> Result<MonochromeLedSnapshot, WriteError> {
    let mut device = Device::new(channel, index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let feature = open_feature::<MonochromeLedFeature>(&mut device).await?;
    let read = async {
        let software_control = feature.software_control().await?;
        let count = feature.count().await?;
        let mut leds = Vec::with_capacity(usize::from(count));
        for index in 0..count {
            leds.push((feature.info(index).await?, feature.state(index).await?));
        }
        if software_control != feature.software_control().await? {
            return Err(hidpp::protocol::v20::Hidpp20Error::UnsupportedResponse);
        }
        Ok(MonochromeLedSnapshot {
            software_control,
            leds,
        })
    }
    .await;
    read.map_err(|error| WriteError::Hidpp(format!("read monochrome LEDs: {error:?}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::scripted::{ScriptedRawHidChannel, scripted_channel};

    #[tokio::test]
    async fn monochrome_snapshot_uses_only_reads_and_checks_reply_length() {
        for short_info in [false, true] {
            let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
                let mut data = [0; 16];
                let long = match (request[2], request[3] >> 4) {
                    (0, 1) => {
                        data[0] = 2;
                        data[2] = request[6];
                        false
                    }
                    (0, 0) => {
                        data[0] = 7;
                        false
                    }
                    (7, 0) => {
                        data[0] = 1;
                        false
                    }
                    (7, 1) => {
                        data[..6].copy_from_slice(&[0, 4, 1, 0, 0x83, 0]);
                        !short_info
                    }
                    (7, 2) => false,
                    (7, 4) => {
                        data[..9].copy_from_slice(&[0, 2, 0, 0, 0xff, 0, 0, 0, 0]);
                        true
                    }
                    _ => return None,
                };
                let mut reply = vec![0; if long { 20 } else { 7 }];
                reply[0] = if long { 0x11 } else { 0x10 };
                reply[1..4].copy_from_slice(&request[1..4]);
                let length = reply.len() - 4;
                reply[4..].copy_from_slice(&data[..length]);
                Some(reply)
            });
            let channel = scripted_channel(raw).await;
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(1),
                read_on_channel(channel, 0xff),
            )
            .await
            .unwrap();
            if short_info {
                assert!(result.is_err(), "short LED info must fail");
            } else {
                let snapshot = result.unwrap();
                assert!(!snapshot.software_control);
                assert_eq!(snapshot.leds.len(), 1);
                assert_eq!(snapshot.leds[0].0.kind, LedKind::Logo);
                assert_eq!(snapshot.leds[0].1.mode, LedMode::On);
            }
            assert!(
                handle
                    .written_reports()
                    .iter()
                    .all(|request| request[2] == 0 || [0, 1, 2, 4].contains(&(request[3] >> 4)))
            );
        }
    }
}
