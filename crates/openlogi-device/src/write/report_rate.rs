use std::sync::Arc;

use hidpp::{channel::HidppChannel, device::Device, feature::report_rate::ReportRateFeature};

use super::{WriteError, open_feature, with_route};
use crate::{DeviceRoute, backend::HidBackend};

/// Supported and active device report intervals in milliseconds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportRateInfo {
    /// Firmware-advertised intervals, in ascending order.
    pub supported_intervals_ms: Vec<u8>,
    /// Active interval, validated against the advertised list.
    pub current_interval_ms: u8,
}

/// Read the report intervals without changing the active rate.
pub async fn get_report_rate_info(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<ReportRateInfo, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| {
        read_on_channel(channel, index)
    })
    .await
}

async fn read_on_channel(
    channel: Arc<HidppChannel>,
    index: u8,
) -> Result<ReportRateInfo, WriteError> {
    let mut device = Device::new(channel, index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let feature = open_feature::<ReportRateFeature>(&mut device).await?;
    read_feature(&feature).await
}

async fn read_feature(feature: &ReportRateFeature) -> Result<ReportRateInfo, WriteError> {
    let mask = feature.get_report_rate_list().await.map_err(|error| {
        WriteError::Hidpp(format!("read supported report intervals: {error:?}"))
    })?;
    let supported_intervals_ms: Vec<u8> = (1..=8)
        .filter(|ms| mask.bits() & (1 << (ms - 1)) != 0)
        .collect();
    let current_interval_ms = feature
        .get_report_rate()
        .await
        .map_err(|error| WriteError::Hidpp(format!("read active report interval: {error:?}")))?;
    if !supported_intervals_ms.contains(&current_interval_ms) {
        return Err(WriteError::Hidpp(format!(
            "active report interval {current_interval_ms} ms is not advertised in {supported_intervals_ms:?}"
        )));
    }
    Ok(ReportRateInfo {
        supported_intervals_ms,
        current_interval_ms,
    })
}

/// Set an interval only after validating the device's advertised values.
pub async fn set_report_interval(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    interval_ms: u8,
) -> Result<(), WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| {
        set_on_channel(channel, index, interval_ms)
    })
    .await
}

async fn set_on_channel(
    channel: Arc<HidppChannel>,
    index: u8,
    interval_ms: u8,
) -> Result<(), WriteError> {
    let mut device = Device::new(channel, index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let feature = open_feature::<ReportRateFeature>(&mut device).await?;
    let info = read_feature(&feature).await?;
    if !info.supported_intervals_ms.contains(&interval_ms) {
        return Err(WriteError::Hidpp(format!(
            "report interval {interval_ms} ms is not advertised"
        )));
    }
    feature
        .set_report_rate(interval_ms)
        .await
        .map_err(|error| WriteError::Hidpp(format!("set report interval: {error:?}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::scripted::{ScriptedRawHidChannel, scripted_channel};

    #[tokio::test]
    async fn report_rate_reads_advertised_intervals_and_rejects_invalid_current_values() {
        for current in [0, 1, 2, 8, 9] {
            let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
                let mut reply = vec![0x10, request[1], request[2], request[3], 0, 0, 0];
                match (request[2], request[3] >> 4) {
                    (0, 1) => {
                        reply[4] = 2;
                        reply[6] = request[6];
                    }
                    (0, 0) => reply[4] = 7,
                    (7, 0) => reply[4] = 0x81,
                    (7, 1) => reply[4] = current,
                    _ => return None,
                }
                Some(reply)
            });
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(1),
                read_on_channel(scripted_channel(raw).await, 0xff),
            )
            .await
            .unwrap();
            if [1, 8].contains(&current) {
                let info = result.unwrap();
                assert_eq!(info.supported_intervals_ms, [1, 8]);
                assert_eq!(info.current_interval_ms, current);
            } else {
                result.unwrap_err();
            }
            assert_eq!(handle.written_reports().len(), 4);
            assert!(
                handle
                    .written_reports()
                    .iter()
                    .all(|request| request[3] >> 4 <= 1)
            );
        }
    }
    #[tokio::test]
    async fn report_rate_writes_only_advertised_intervals() {
        for target in [0, 2, 8, 9] {
            let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
                let mut reply = vec![0x10, request[1], request[2], request[3], 0, 0, 0];
                match (request[2], request[3] >> 4) {
                    (0, 1) => {
                        reply[4] = 2;
                        reply[6] = request[6];
                    }
                    (0, 0) => reply[4] = 7,
                    (7, 0) => reply[4] = 0x81,
                    (7, 1) => reply[4] = 1,
                    (7, 2) => assert_eq!(request[4], target),
                    _ => return None,
                }
                Some(reply)
            });
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(1),
                set_on_channel(scripted_channel(raw).await, 0xff, target),
            )
            .await
            .unwrap();
            assert_eq!(result.is_ok(), target == 8);
            assert_eq!(
                handle
                    .written_reports()
                    .iter()
                    .filter(|request| request[2] == 7 && request[3] >> 4 == 2)
                    .count(),
                usize::from(target == 8)
            );
        }
    }
}
