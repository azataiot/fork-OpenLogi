use std::sync::{Arc, Mutex};

use hidpp::feature::{
    onboard_profiles::{
        AssignmentEdit, OnboardProfilesFeature, ProfileAssignment, ProfileChange, ProfileEdit,
        ProfileWriteError, ProfileWriteOutcome, ProfileWriteStage,
    },
    report_rate::ReportRateList,
};

use super::*;
use crate::channel::scripted::{
    ScriptedRawHidChannel, ScriptedRawHidHandle, feature_error, scripted_channel,
};

struct Flash {
    report_interval: u8,
    bytes: Vec<u8>,
    staged: Vec<u8>,
    reject: Option<u8>,
    drop_ack: Option<u8>,
    ignore_commit: bool,
    host_mode: bool,
}

struct Fixture {
    shared: crate::SharedChannel,
    feature: Arc<OnboardProfilesFeature>,
    change: ProfileChange,
    handle: ScriptedRawHidHandle,
    flash: Arc<Mutex<Flash>>,
}

impl Flash {
    fn respond(&mut self, request: &[u8]) -> Option<Vec<u8>> {
        let function = request[3] >> 4;
        if request[2] == 0 && function == 0 && request[4..6] == [0x80, 0x60] {
            return Some(vec![0x10, request[1], request[2], request[3], 8, 0, 0]);
        }
        if request[2] == 8 && function <= 1 {
            let value = if function == 0 {
                0x83
            } else {
                self.report_interval
            };
            return Some(vec![0x10, request[1], request[2], request[3], value, 0, 0]);
        }
        if request[2] == 7 {
            if self.reject == Some(function) {
                return Some(feature_error(request, 2));
            }
            if self.drop_ack == Some(function) {
                return None;
            }
            match function {
                5 if request[4..6] == [0, 1] => {
                    let offset = usize::from(u16::from_be_bytes([request[6], request[7]]));
                    let mut reply = vec![0x11, request[1], request[2], request[3]];
                    reply.extend_from_slice(&self.bytes[offset..offset + 16]);
                    return Some(reply);
                }
                6 => {
                    assert_eq!(&request[4..10], &[0, 1, 0, 0, 4, 0]);
                    self.staged.clear();
                }
                7 => {
                    assert_eq!(request.len(), 20);
                    self.staged.extend_from_slice(&request[4..]);
                    assert!(self.staged.len() <= 1024);
                }
                8 => {
                    assert_eq!(self.staged.len(), 1024);
                    if !self.ignore_commit {
                        self.bytes = self.staged.clone();
                    }
                }
                _ => {}
            }
            if (6..=8).contains(&function) {
                return Some(vec![0x10, request[1], request[2], request[3], 0, 0, 0]);
            }
        }
        let mut reply = super::tests::reply_with_sector_size(request, 1024)?;
        if request[2] == 7 && function == 2 && self.host_mode {
            reply[4] = 2;
        }
        Some(reply)
    }
}

async fn fixture() -> Fixture {
    let mut bytes = vec![0; 1024];
    bytes[0] = 2;
    bytes[3..5].copy_from_slice(&1200_u16.to_le_bytes());
    bytes[32..36].copy_from_slice(&[0x80, 0, 0, 8]);
    bytes[1022..].copy_from_slice(&[0xa7, 0x17]);
    let flash = Arc::new(Mutex::new(Flash {
        report_interval: 1,
        bytes,
        staged: Vec::new(),
        reject: None,
        drop_ack: None,
        ignore_commit: false,
        host_mode: false,
    }));
    let observed = Arc::clone(&flash);
    let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
        let mut flash = observed.lock().unwrap();
        flash.respond(request)
    });
    let channel = scripted_channel(raw).await;
    let shared = crate::SharedChannel::new(
        Arc::clone(&channel),
        DeviceRoute::Direct {
            vendor_id: 0xff00,
            product_id: 0xabcd,
        },
    );
    let mut device = Device::new(channel, 0xff).await.unwrap();
    let feature = open_feature::<OnboardProfilesFeature>(&mut device)
        .await
        .unwrap();
    let description = feature.description().await.unwrap();
    let original = feature.profile(1, &description).await.unwrap();
    let change = ProfileChange::new(
        &original,
        description,
        &ProfileEdit {
            report_interval_ms: Some(8),
            assignments: vec![AssignmentEdit {
                index: 1,
                assignment: ProfileAssignment::MousePrimary,
            }],
        },
        ReportRateList::MS_2 | ReportRateList::MS_8,
    )
    .unwrap();
    Fixture {
        shared,
        feature,
        change,
        handle,
        flash,
    }
}

fn writes(handle: &ScriptedRawHidHandle) -> Vec<Vec<u8>> {
    handle
        .written_reports()
        .into_iter()
        .filter(|report| report[2] == 7 && (6..=8).contains(&(report[3] >> 4)))
        .collect()
}

#[tokio::test]
async fn profile_writer_transfers_one_complete_sector_and_restores_every_original_byte() {
    let f = fixture().await;
    let outcome = f.feature.write_profile(1, &f.change).await.unwrap();
    assert_eq!(outcome, ProfileWriteOutcome::Written);
    assert_eq!(f.flash.lock().unwrap().bytes, f.change.after().raw);
    let reports = writes(&f.handle);
    assert_eq!(reports.len(), 66);
    let transferred: Vec<u8> = reports[1..65]
        .iter()
        .flat_map(|report| report[4..].iter().copied())
        .collect();
    assert_eq!(transferred, f.change.after().raw);
    f.feature
        .write_profile(1, &f.change.restoration().unwrap())
        .await
        .unwrap();
    assert_eq!(f.flash.lock().unwrap().bytes, f.change.before());
    assert_eq!(writes(&f.handle).len(), 132);
}

#[tokio::test]
async fn profile_writer_rejects_stale_unknown_bytes_and_inactive_targets_before_writes() {
    for case in 0..3 {
        let f = fixture().await;
        let target = u16::from(case != 2);
        if case == 0 {
            let mut flash = f.flash.lock().unwrap();
            flash.bytes[500] = 1;
            flash.bytes[1022..].copy_from_slice(&[0xcf, 0xac]);
        } else if case == 1 {
            f.flash.lock().unwrap().host_mode = true;
        }
        let error = f
            .feature
            .write_profile(target, &f.change)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ProfileWriteError::StaleProfile | ProfileWriteError::InactiveProfile
        ));
        assert!(writes(&f.handle).is_empty());
    }
}

#[tokio::test]
async fn profile_writer_stops_on_first_write_error_without_commit_or_retry() {
    for (function, stage, count) in [
        (6, ProfileWriteStage::Begin, 1),
        (7, ProfileWriteStage::Data { offset: 0 }, 2),
        (8, ProfileWriteStage::Commit, 66),
    ] {
        let f = fixture().await;
        f.flash.lock().unwrap().reject = Some(function);
        let error = f.feature.write_profile(1, &f.change).await.unwrap_err();
        assert!(
            matches!(error, ProfileWriteError::Uncertain { stage: actual, .. } if actual == stage)
        );
        assert_eq!(writes(&f.handle).len(), count);
    }
}

#[tokio::test]
async fn profile_writer_requires_complete_readback_and_does_not_retry_a_timeout() {
    let f = fixture().await;
    f.flash.lock().unwrap().ignore_commit = true;
    let error = f.feature.write_profile(1, &f.change).await.unwrap_err();
    assert!(matches!(error, ProfileWriteError::VerificationFailed));
    assert_eq!(writes(&f.handle).len(), 66);

    let f = fixture().await;
    f.flash.lock().unwrap().drop_ack = Some(7);
    let error = tokio::time::timeout(
        std::time::Duration::from_secs(8),
        f.feature.write_profile(1, &f.change),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(matches!(
        error,
        ProfileWriteError::Uncertain {
            stage: ProfileWriteStage::Data { offset: 0 },
            ..
        }
    ));
    assert_eq!(writes(&f.handle).len(), 2);
}

#[tokio::test]
async fn profile_restore_recovers_a_corrupt_sector_without_accepting_stale_recovery_reads() {
    let f = fixture().await;
    let description = f.feature.description().await.unwrap();
    let mut corrupt = f.flash.lock().unwrap().bytes.clone();
    corrupt[500] ^= 1;
    f.flash.lock().unwrap().bytes.clone_from(&corrupt);
    let recovery = ProfileChange::for_restore(&corrupt, f.change.before(), description).unwrap();
    f.feature.write_profile(1, &recovery).await.unwrap();
    assert_eq!(f.flash.lock().unwrap().bytes, f.change.before());
    let count = writes(&f.handle).len();
    let error = f.feature.write_profile(1, &recovery).await.unwrap_err();
    assert!(matches!(error, ProfileWriteError::StaleProfile));
    assert_eq!(writes(&f.handle).len(), count);
}

#[tokio::test]
async fn onboard_preparation_rejects_a_replacement_connection_and_preserves_corrupt_reads_for_restore()
 {
    use openlogi_core::hid::onboard_profile::{
        OnboardProfileContents, OnboardProfileEdit, ProfileEditId,
    };
    let first = fixture().await;
    let session = read_onboard_profile_on(&first.shared).await.unwrap();
    let second = fixture().await;
    let edit = OnboardProfileEdit {
        report_interval_ms: Some(8),
        assignments: vec![],
    };
    session.prepare(&second.shared, &edit).unwrap_err();
    session.prepare(&first.shared, &edit).unwrap();
    first.flash.lock().unwrap().bytes[500] ^= 1;
    let corrupt = read_onboard_profile_on(&first.shared).await.unwrap();
    assert!(matches!(
        corrupt
            .view(ProfileEditId {
                run: 3,
                sequence: 4
            })
            .contents,
        OnboardProfileContents::InvalidProfile
    ));
    corrupt.prepare(&first.shared, &edit).unwrap_err();
    assert!(writes(&first.handle).is_empty());
}

#[tokio::test]
async fn profile_view_distinguishes_live_report_rate_from_saved_rate() {
    use openlogi_core::hid::onboard_profile::{OnboardProfileContents, ProfileEditId};
    let fixture = fixture().await;
    let session = read_onboard_profile_on(&fixture.shared).await.unwrap();
    let view = session.view(ProfileEditId {
        run: 1,
        sequence: 1,
    });
    assert_eq!(view.active_report_interval_ms, Ok(1));
    let OnboardProfileContents::Valid(settings) = view.contents else {
        panic!("valid stored profile")
    };
    assert_eq!(settings.report_interval_ms, 2);
    assert!(writes(&fixture.handle).is_empty());
    for invalid in [0, 3, 9] {
        fixture.flash.lock().unwrap().report_interval = invalid;
        let session = read_onboard_profile_on(&fixture.shared).await.unwrap();
        let view = session.view(ProfileEditId {
            run: 1,
            sequence: 2,
        });
        assert!(
            view.active_report_interval_ms.is_err(),
            "invalid interval {invalid}"
        );
        assert!(matches!(view.contents, OnboardProfileContents::Valid(_)));
    }
}
