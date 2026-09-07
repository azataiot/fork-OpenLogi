use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

use openlogi_agent_core::orchestrator::SharedRuntime;
use openlogi_core::hid::onboard_profile::{
    OnboardApplyResult, OnboardBackupSummary, OnboardProfileEdit, OnboardProfileFailure,
    OnboardProfileView, ProfileEditId,
};
use openlogi_hid::{DeviceRoute, OnboardProfileSession, WriteError};
use tokio::sync::Mutex;

use super::profile_backups::ProfileBackups;

#[derive(Default)]
pub(super) struct OnboardProfiles {
    state: Mutex<Preparations>,
}

#[derive(Default)]
struct Preparations {
    sequence: u64,
    sessions: Vec<(DeviceRoute, PreparedProfile)>,
}

struct PreparedProfile {
    id: ProfileEditId,
    generation: u64,
    session: OnboardProfileSession,
}

pub(super) enum ProfileOperation {
    Edit(OnboardProfileEdit),
    Restore(ProfileEditId),
}

impl OnboardProfiles {
    pub(super) async fn read(
        &self,
        shared: &SharedRuntime,
        route: &DeviceRoute,
    ) -> Result<OnboardProfileView, WriteError> {
        let mut state = self.state.lock().await;
        let generation = shared.capture_rearm_generation.load(Ordering::Relaxed);
        state.sessions.retain(|(route, prepared)| {
            prepared.generation == generation
                && shared
                    .channel_registry
                    .lookup(route)
                    .is_some_and(|channel| prepared.session.matches(&channel))
        });
        let session = shared
            .device(route)
            .run_profile(
                |channel| async move { openlogi_hid::read_onboard_profile_on(&channel).await },
            )
            .await?;
        state.sequence = state.sequence.checked_add(1).ok_or_else(|| {
            failure(
                OnboardProfileFailure::StaleSession,
                "preparation sequence exhausted",
                None,
            )
        })?;
        let id = ProfileEditId {
            run: succession::Run::mint().get(),
            sequence: state.sequence,
        };
        let mut view = session.view(id);
        let backups = tokio::task::spawn_blocking(|| backup_store()?.list().map_err(backup_error))
            .await
            .map_err(|error| task_error(&error))??;
        view.backups = backups
            .into_iter()
            .filter(|(_, backup)| session.can_restore(backup))
            .map(|(id, backup)| OnboardBackupSummary {
                id,
                created_unix_seconds: backup.created_unix_seconds,
            })
            .collect();
        check_generation(generation, shared)?;
        state.sessions.retain(|(current, _)| current != route);
        state.sessions.push((
            route.clone(),
            PreparedProfile {
                id,
                generation,
                session,
            },
        ));
        Ok(view)
    }

    pub(super) async fn submit(
        self: Arc<Self>,
        shared: SharedRuntime,
        route: DeviceRoute,
        id: ProfileEditId,
        operation: ProfileOperation,
    ) -> Result<OnboardApplyResult, WriteError> {
        complete_on_agent(async move { self.apply(&shared, &route, id, operation).await }).await
    }

    async fn apply(
        &self,
        shared: &SharedRuntime,
        route: &DeviceRoute,
        id: ProfileEditId,
        operation: ProfileOperation,
    ) -> Result<OnboardApplyResult, WriteError> {
        let prepared = {
            let mut state = self.state.lock().await;
            let index = state
                .sessions
                .iter()
                .position(|(current_route, prepared)| current_route == route && prepared.id == id)
                .ok_or_else(|| {
                    failure(
                        OnboardProfileFailure::StaleSession,
                        "preparation expired; read the profile again",
                        None,
                    )
                })?;
            state.sessions.swap_remove(index).1
        };
        let PreparedProfile {
            session,
            generation,
            ..
        } = prepared;
        let registry = shared.channel_registry.clone();
        shared
            .device(route)
            .run_profile(|channel| async move {
                check_generation(generation, shared)?;
                let change = match operation {
                    ProfileOperation::Edit(edit) => session.prepare(&channel, &edit)?,
                    ProfileOperation::Restore(backup_id) => {
                        let backup = tokio::task::spawn_blocking(move || {
                            backup_store()?.load(backup_id).map_err(backup_error)
                        })
                        .await
                        .map_err(|error| task_error(&error))??;
                        session.prepare_restore(&channel, &backup)?
                    }
                };
                let changed = change.before() != change.after().raw;
                if changed {
                    let created = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_err(backup_error)?
                        .as_secs();
                    let backup = session.backup(&change, created);
                    tokio::task::spawn_blocking(move || {
                        backup_store()?.save(id, &backup).map_err(backup_error)
                    })
                    .await
                    .map_err(|error| task_error(&error))??;
                }
                check_generation(generation, shared)?;
                if !registry.is_unique_current(&channel) {
                    return Err(failure(
                        OnboardProfileFailure::StaleSession,
                        "device connection changed before the write",
                        changed.then_some(id),
                    ));
                }
                session
                    .apply(&channel, &change)
                    .await
                    .map_err(|error| match error {
                        WriteError::OnboardProfile { kind, message, .. } => {
                            failure(kind, message, changed.then_some(id))
                        }
                        error => error,
                    })?;
                Ok(if changed {
                    OnboardApplyResult::Written { backup_id: id }
                } else {
                    OnboardApplyResult::Unchanged
                })
            })
            .await
    }
}

fn check_generation(expected: u64, shared: &SharedRuntime) -> Result<(), WriteError> {
    if shared.capture_rearm_generation.load(Ordering::Relaxed) != expected {
        return Err(failure(
            OnboardProfileFailure::StaleSession,
            "device reconnected or host resumed; read the profile again",
            None,
        ));
    }
    Ok(())
}

async fn complete_on_agent<T: Send + 'static>(
    operation: impl Future<Output = Result<T, WriteError>> + Send + 'static,
) -> Result<T, WriteError> {
    // The worker owns the lease and readback even when the requesting IPC future is dropped.
    tokio::spawn(operation)
        .await
        .map_err(|error| task_error(&error))?
}

fn backup_store() -> Result<ProfileBackups, WriteError> {
    openlogi_core::paths::state_dir()
        .map(|path| ProfileBackups::new(path.join("onboard-profiles")))
        .map_err(backup_error)
}

fn backup_error(error: impl std::fmt::Display) -> WriteError {
    failure(OnboardProfileFailure::Backup, error.to_string(), None)
}

fn task_error(error: &tokio::task::JoinError) -> WriteError {
    failure(
        OnboardProfileFailure::Uncertain,
        format!("profile task interrupted: {error}; read again before any further operation"),
        None,
    )
}

fn failure(
    kind: OnboardProfileFailure,
    message: impl Into<String>,
    backup_id: Option<ProfileEditId>,
) -> WriteError {
    WriteError::OnboardProfile {
        kind,
        message: message.into(),
        backup_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reconnect_generation_and_missing_preparations_fail_closed() {
        use openlogi_agent_core::{observable::ObservableState, orchestrator::Orchestrator};
        let runtime = Orchestrator::new(
            openlogi_core::config::Config::default(),
            Arc::new(ObservableState::new("test".into())),
        )
        .shared();
        check_generation(0, &runtime).unwrap();
        runtime.capture_rearm_generation.store(1, Ordering::Relaxed);
        assert!(matches!(
            check_generation(0, &runtime),
            Err(WriteError::OnboardProfile {
                kind: OnboardProfileFailure::StaleSession,
                ..
            })
        ));
        let profiles = Arc::new(OnboardProfiles::default());
        let route = DeviceRoute::Direct {
            vendor_id: 0xff00,
            product_id: 0xabcd,
        };
        assert!(matches!(
            profiles.read(&runtime, &route).await,
            Err(WriteError::DeviceNotFound)
        ));
        let result = profiles
            .submit(
                runtime,
                route,
                ProfileEditId {
                    run: 1,
                    sequence: 1,
                },
                ProfileOperation::Edit(OnboardProfileEdit::default()),
            )
            .await;
        assert!(matches!(
            result,
            Err(WriteError::OnboardProfile {
                kind: OnboardProfileFailure::StaleSession,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn client_cancellation_does_not_cancel_the_agent_operation() {
        let (started, starting) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let (completed, completion) = tokio::sync::oneshot::channel();
        let request = tokio::spawn(complete_on_agent(async move {
            started.send(()).unwrap();
            released.await.unwrap();
            completed.send(()).unwrap();
            Ok(())
        }));
        starting.await.unwrap();
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
        release.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), completion)
            .await
            .unwrap()
            .unwrap();
    }
}
