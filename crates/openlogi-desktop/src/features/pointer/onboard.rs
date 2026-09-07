use gpui::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement, Render, SharedString,
    Subscription, Task, Window,
};
use gpui_component::{
    IndexPath,
    select::{SelectEvent, SelectItem, SelectState},
};
use openlogi_core::hid::{
    DeviceRoute, WriteError,
    onboard_profile::{
        OnboardApplyResult, OnboardAssignment, OnboardAssignmentEdit, OnboardProfileContents,
        OnboardProfileEdit, OnboardProfileFailure, OnboardProfileView, ProfileEditId,
        StoredOnboardAssignment,
    },
};
use tokio::sync::oneshot;

use crate::{
    services::ipc::Command,
    state::{AppState, StateEvent},
    ui::theme,
};

mod geometry;
mod visual;

#[derive(Clone)]
struct AssignmentOption(Option<OnboardAssignment>);

impl SelectItem for AssignmentOption {
    type Value = Option<OnboardAssignment>;
    fn title(&self) -> SharedString {
        match self.0 {
            None => tr!("onboard.keep_assignment"),
            Some(OnboardAssignment::MousePrimary) => tr!("onboard.primary"),
            Some(OnboardAssignment::MouseSecondary) => tr!("onboard.secondary"),
            Some(OnboardAssignment::MouseMiddle) => tr!("onboard.middle"),
            Some(OnboardAssignment::MouseBack) => tr!("onboard.back"),
            Some(OnboardAssignment::MouseForward) => tr!("onboard.forward"),
            Some(OnboardAssignment::DpiNext) => tr!("onboard.dpi_next"),
            Some(OnboardAssignment::DpiPrevious) => tr!("onboard.dpi_previous"),
            Some(OnboardAssignment::DpiShift) => tr!("onboard.dpi_shift"),
        }
    }
    fn value(&self) -> &Self::Value {
        &self.0
    }
}

const ACTIONS: [OnboardAssignment; 8] = [
    OnboardAssignment::MousePrimary,
    OnboardAssignment::MouseSecondary,
    OnboardAssignment::MouseMiddle,
    OnboardAssignment::MouseBack,
    OnboardAssignment::MouseForward,
    OnboardAssignment::DpiNext,
    OnboardAssignment::DpiPrevious,
    OnboardAssignment::DpiShift,
];

type Target = (String, DeviceRoute);

fn profile_error(error: &WriteError) -> String {
    match error {
        WriteError::OnboardProfile {
            backup_id: Some(id),
            ..
        } => format!("{error}. {} {id}", tr!("onboard.backup")),
        _ => error.to_string(),
    }
}

fn stage_assignment(
    edit: &mut OnboardProfileEdit,
    index: u8,
    assignment: Option<OnboardAssignment>,
) {
    edit.assignments.retain(|edit| edit.index != index);
    if let Some(assignment) = assignment {
        edit.assignments
            .push(OnboardAssignmentEdit { index, assignment });
    }
}

fn current_target(cx: &App) -> Option<Target> {
    let record = AppState::try_read(cx)?.current_record()?;
    if !record.online
        || !record
            .capabilities
            .is_some_and(|caps| caps.onboard_profiles)
    {
        return None;
    }
    Some((record.record_key(), record.route.clone()?))
}

pub struct OnboardPanel {
    target: Option<Target>,
    opened: bool,
    selected: u8,
    image_key: &'static str,
    hovered: Option<u8>,
    backups_open: bool,
    restore_confirmation: Option<ProfileEditId>,
    generation: u64,
    view: Option<OnboardProfileView>,
    edit: OnboardProfileEdit,
    selectors: Vec<Entity<SelectState<Vec<AssignmentOption>>>>,
    selector_subscriptions: Vec<Subscription>,
    button_focus: Vec<[gpui::FocusHandle; 3]>,
    task: Option<Task<()>>,
    busy: bool,
    message: Option<String>,
    _state_subscription: Subscription,
}

impl OnboardPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let subscription =
            cx.subscribe(&AppState::global(cx), |panel, _, event: &StateEvent, cx| {
                if matches!(
                    event,
                    StateEvent::DeviceSelected(_)
                        | StateEvent::InventoryChanged
                        | StateEvent::AgentChanged
                ) {
                    let target = current_target(cx);
                    if target != panel.target || matches!(event, StateEvent::AgentChanged) {
                        panel.opened = false;
                        panel.selected = 0;
                        panel.image_key = "device_image";
                        panel.hovered = None;
                        panel.restore_confirmation = None;
                        panel.generation += 1;
                        panel.view = None;
                        panel.edit = OnboardProfileEdit::default();
                        panel.selectors.clear();
                        panel.button_focus.clear();
                        panel.selector_subscriptions.clear();
                        panel.target = target;
                        if panel.busy {
                            panel.message = Some(tr!("onboard.connection_changed").to_string());
                        }
                        panel.task = None;
                        panel.busy = false;
                    }
                    cx.notify();
                }
            });
        Self {
            target: current_target(cx),
            opened: false,
            selected: 0,
            image_key: "device_image",
            hovered: None,
            backups_open: false,
            restore_confirmation: None,
            generation: 0,
            view: None,
            edit: OnboardProfileEdit::default(),
            selectors: Vec::new(),
            selector_subscriptions: Vec::new(),
            button_focus: Vec::new(),
            task: None,
            busy: false,
            message: None,
            _state_subscription: subscription,
        }
    }

    fn dirty_count(&self) -> usize {
        self.edit.assignments.len() + usize::from(self.edit.report_interval_ms.is_some())
    }

    fn discard(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.edit = OnboardProfileEdit::default();
        self.selectors.clear();
        self.button_focus.clear();
        self.selector_subscriptions.clear();
        cx.notify();
    }

    pub fn activate(&mut self, cx: &mut Context<Self>) {
        if !self.opened && current_target(cx).is_some() {
            self.opened = true;
            self.read(cx);
        }
    }

    fn select_button(&mut self, index: u8, cx: &mut Context<Self>) {
        self.selected = index;
        if let Some(button) = visual::model_buttons(cx)
            .iter()
            .find(|button| button.index == index)
        {
            self.image_key = button.image_key;
        }
        cx.notify();
    }

    fn read(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.dirty_count() > 0 {
            return;
        }
        let Some(target) = current_target(cx) else {
            return;
        };
        let Some(sender) = AppState::try_read(cx).map(AppState::ipc_sender) else {
            return;
        };
        let (reply, result) = oneshot::channel();
        if sender
            .send(Command::ReadOnboard(target.1.clone(), reply))
            .is_err()
        {
            self.message = Some(WriteError::AgentUnavailable.to_string());
            cx.notify();
            return;
        }
        self.message = None;
        self.restore_confirmation = None;
        self.target = Some(target.clone());
        self.generation += 1;
        let generation = self.generation;
        self.busy = true;
        self.view = None;
        self.edit = OnboardProfileEdit::default();
        self.selectors.clear();
        self.button_focus.clear();
        self.selector_subscriptions.clear();
        self.task = Some(cx.spawn(async move |panel, cx| {
            let result = result.await.unwrap_or(Err(WriteError::AgentUnavailable));
            let _ = panel.update(cx, |panel, cx| {
                if panel.generation != generation || current_target(cx).as_ref() != Some(&target) {
                    return;
                }
                panel.busy = false;
                match result {
                    Ok(view) => panel.view = Some(view),
                    Err(error) => panel.message = Some(error.to_string()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn apply(&mut self, backup: Option<ProfileEditId>, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(target) = current_target(cx).filter(|target| self.target.as_ref() == Some(target))
        else {
            return;
        };
        let Some(view) = self.view.take() else {
            return;
        };
        let Some(sender) = AppState::try_read(cx).map(AppState::ipc_sender) else {
            return;
        };
        let (reply, result) = oneshot::channel();
        let command = match backup {
            Some(backup) => Command::RestoreOnboard(target.1.clone(), view.edit_id, backup, reply),
            None => Command::ApplyOnboard(target.1.clone(), view.edit_id, self.edit.clone(), reply),
        };
        self.selectors.clear();
        self.button_focus.clear();
        self.selector_subscriptions.clear();
        self.edit = OnboardProfileEdit::default();
        if sender.send(command).is_err() {
            self.message = Some(WriteError::AgentUnavailable.to_string());
            cx.notify();
            return;
        }
        self.busy = true;
        let generation = self.generation;
        self.message = Some(tr!("onboard.writing").to_string());
        self.task = Some(cx.spawn(async move |panel, cx| {
            let result = result.await.unwrap_or_else(|_| {
                Err(WriteError::OnboardProfile {
                    kind: OnboardProfileFailure::Uncertain,
                    message: tr!("onboard.uncertain").to_string(),
                    backup_id: None,
                })
            });
            let _ = panel.update(cx, |panel, cx| {
                if panel.generation != generation || current_target(cx).as_ref() != Some(&target) {
                    return;
                }
                panel.busy = false;
                panel.message = Some(match result {
                    Ok(OnboardApplyResult::Unchanged) => tr!("onboard.unchanged").to_string(),
                    Ok(OnboardApplyResult::Written { backup_id }) => {
                        format!("{} {backup_id}", tr!("onboard.written"))
                    }
                    Err(error) => profile_error(&error),
                });
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn build_selectors(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selectors.is_empty() {
            return;
        }
        let Some(OnboardProfileView {
            contents: OnboardProfileContents::Valid(settings),
            ..
        }) = self.view.as_ref()
        else {
            return;
        };
        for (index, stored) in settings.assignments.iter().enumerate() {
            let options: Vec<_> = std::iter::once(AssignmentOption(None))
                .chain(
                    ACTIONS
                        .into_iter()
                        .map(|action| AssignmentOption(Some(action))),
                )
                .collect();
            let selected = match stored {
                StoredOnboardAssignment::Known(action) => ACTIONS
                    .iter()
                    .position(|value| value == action)
                    .map_or(0, |index| index + 1),
                StoredOnboardAssignment::Unknown(_) => 0,
            };
            let selector = cx.new(|cx| {
                SelectState::new(
                    options,
                    Some(IndexPath::default().row(selected)),
                    window,
                    cx,
                )
            });
            self.selector_subscriptions.push(cx.subscribe(
                &selector,
                move |panel, _, event: &SelectEvent<Vec<AssignmentOption>>, cx| {
                    if panel.busy {
                        return;
                    }
                    let SelectEvent::Confirm(action) = event;
                    let Ok(index) = u8::try_from(index) else {
                        return;
                    };
                    let action = action.flatten();
                    let stored = panel.view.as_ref().and_then(|view| match &view.contents {
                        OnboardProfileContents::Valid(settings) => {
                            settings.assignments.get(usize::from(index))
                        }
                        OnboardProfileContents::InvalidProfile => None,
                    });
                    let action = action
                        .filter(|action| stored != Some(&StoredOnboardAssignment::Known(*action)));
                    stage_assignment(&mut panel.edit, index, action);
                    cx.notify();
                },
            ));
            let handles = std::array::from_fn(|_| cx.focus_handle());
            for handle in &handles {
                self.selector_subscriptions.push(cx.on_focus(
                    handle,
                    window,
                    move |panel, _, cx| {
                        if let Ok(index) = u8::try_from(index) {
                            panel.select_button(index, cx);
                        }
                    },
                ));
            }
            self.button_focus.push(handles);
            self.selectors.push(selector);
        }
    }
}

impl Render for OnboardPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.activate(cx);
        self.build_selectors(window, cx);
        self.render_workspace(window, cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn onboard_staging_changes_only_explicit_buttons_and_keep_cancels_the_edit() {
        let mut edit = OnboardProfileEdit {
            report_interval_ms: Some(2),
            ..OnboardProfileEdit::default()
        };
        stage_assignment(&mut edit, 7, Some(OnboardAssignment::MouseBack));
        stage_assignment(&mut edit, 7, Some(OnboardAssignment::MouseForward));
        stage_assignment(&mut edit, 2, Some(OnboardAssignment::DpiNext));
        stage_assignment(&mut edit, 7, None);
        assert_eq!(
            edit.assignments,
            vec![OnboardAssignmentEdit {
                index: 2,
                assignment: OnboardAssignment::DpiNext
            }]
        );
        assert_eq!(edit.report_interval_ms, Some(2));
    }

    #[test]
    fn onboard_failure_keeps_the_recovery_backup_visible() {
        let id = ProfileEditId {
            run: 12,
            sequence: 3,
        };
        let error = WriteError::OnboardProfile {
            kind: OnboardProfileFailure::Uncertain,
            message: "readback interrupted".into(),
            backup_id: Some(id),
        };
        assert!(profile_error(&error).contains(&id.to_string()));
        assert!(profile_error(&error).contains("readback interrupted"));
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use crate::{services::assets::AssetResolver, state::ConfigPersistence};
    use openlogi_core::{
        config::Config,
        device::{Capabilities, DeviceInventory, DeviceKind, PairedDevice, ReceiverInfo},
        hid::onboard_profile::OnboardProfileDescriptor,
    };

    fn setup(
        cx: &mut gpui::TestAppContext,
    ) -> (
        Entity<OnboardPanel>,
        tokio::sync::mpsc::UnboundedReceiver<Command>,
    ) {
        let inventory = DeviceInventory {
            receiver: ReceiverInfo {
                name: "Profile Mouse".into(),
                vendor_id: 0x046d,
                product_id: 0xc07e,
                unique_id: None,
            },
            paired: vec![PairedDevice {
                slot: 0xff,
                codename: Some("Profile Mouse".into()),
                wpid: None,
                kind: DeviceKind::Mouse,
                online: true,
                battery: None,
                model_info: None,
                capabilities: Some(Capabilities {
                    onboard_profiles: true,
                    ..Capabilities::default()
                }),
            }],
        };
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let state = AppState::with_runtime(
            Config::ephemeral(),
            &[inventory],
            &[],
            &AssetResolver::new(),
            &[],
            ConfigPersistence::MemoryOnly,
            sender,
        );
        let panel = cx.update(|cx| {
            AppState::set_global(cx.new(|_| state), cx);
            cx.new(OnboardPanel::new)
        });
        (panel, receiver)
    }

    fn view() -> OnboardProfileView {
        OnboardProfileView {
            edit_id: ProfileEditId {
                run: 4,
                sequence: 2,
            },
            sector: 1,
            description: OnboardProfileDescriptor {
                memory_model: 1,
                profile_format: 1,
                macro_format: 1,
                profile_count: 1,
                rom_profile_count: 1,
                button_count: 1,
                sector_count: 2,
                sector_size: 256,
                mechanical_layout: 0,
                various_info: 1,
            },
            supported_intervals_ms: vec![1, 2],
            contents: OnboardProfileContents::InvalidProfile,
            backups: vec![],
            active_report_interval_ms: Ok(1),
        }
    }

    #[gpui::test]
    fn keyboard_selection_switches_views_and_reverting_an_assignment_clears_the_draft(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let (unused, mut commands) = setup(cx);
        drop(unused);
        let (panel, cx) = cx.add_window_view(|_, cx| OnboardPanel::new(cx));
        let mut view = view();
        view.contents = OnboardProfileContents::Valid(
            openlogi_core::hid::onboard_profile::OnboardProfileSettings {
                report_interval_ms: 1,
                dpi_stages: [400, 800, 1600, 0, 0],
                default_dpi_index: 0,
                shift_dpi_index: 0,
                assignments: vec![
                    StoredOnboardAssignment::Known(OnboardAssignment::MousePrimary);
                    8
                ],
            },
        );
        let Command::ReadOnboard(_, reply) = commands.try_recv().unwrap() else {
            panic!("initial read required")
        };
        reply.send(Ok(view)).unwrap();
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.run_until_parked();
        let focus = panel.read_with(cx, |panel, _| panel.button_focus[4][2].clone());
        cx.update(|window, cx| {
            window.activate_window();
            focus.focus(window, cx);
            window.draw(cx).clear(cx);
        });
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| {
            assert_eq!(panel.selected, 4);
            assert_eq!(panel.image_key, "device_side");
        });
        let selector = panel.read_with(cx, |panel, _| panel.selectors[4].clone());
        selector.update(cx, |_, cx| {
            cx.emit(SelectEvent::Confirm(Some(Some(
                OnboardAssignment::MouseBack,
            ))));
        });
        panel.read_with(cx, |panel, _| assert_eq!(panel.dirty_count(), 1));
        selector.update(cx, |_, cx| {
            cx.emit(SelectEvent::Confirm(Some(Some(
                OnboardAssignment::MousePrimary,
            ))));
        });
        panel.read_with(cx, |panel, _| assert_eq!(panel.dirty_count(), 0));
        assert!(
            commands.try_recv().is_err(),
            "selection must not write to the device"
        );
        panel.update(cx, |panel, cx| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            panel.view.as_mut().unwrap().backups = [60, 7200, 172_800]
                .into_iter()
                .map(
                    |age| openlogi_core::hid::onboard_profile::OnboardBackupSummary {
                        id: ProfileEditId {
                            run: 1,
                            sequence: age,
                        },
                        created_unix_seconds: now - age,
                    },
                )
                .collect();
            panel.backups_open = true;
            panel.restore_confirmation = Some(ProfileEditId {
                run: 1,
                sequence: 60,
            });
            cx.notify();
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(
            commands.try_recv().is_err(),
            "showing a backup confirmation must not restore it"
        );
        cx.update(|window, _| window.remove_window());
    }

    #[gpui::test]
    fn onboard_disconnected_write_is_uncertain_and_cannot_reuse_the_preparation(
        cx: &mut gpui::TestAppContext,
    ) {
        let (panel, mut commands) = setup(cx);
        panel.update(cx, |panel, cx| {
            panel.view = Some(view());
            panel.apply(
                Some(ProfileEditId {
                    run: 1,
                    sequence: 1,
                }),
                cx,
            );
            panel.apply(
                Some(ProfileEditId {
                    run: 1,
                    sequence: 1,
                }),
                cx,
            );
        });
        let Command::RestoreOnboard(_, id, _, reply) = commands.try_recv().unwrap() else {
            panic!("restore command required")
        };
        assert_eq!(id, view().edit_id);
        assert!(
            commands.try_recv().is_err(),
            "a second Apply must not repeat a write"
        );
        drop(reply);
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| {
            assert!(panel.view.is_none());
            assert!(!panel.busy);
            assert!(panel.message.as_ref().unwrap().contains("Uncertain"));
        });
    }

    #[gpui::test]
    fn dirty_refresh_preserves_the_preparation_and_discard_clears_only_the_draft(
        cx: &mut gpui::TestAppContext,
    ) {
        let (panel, mut commands) = setup(cx);
        panel.update(cx, |panel, cx| {
            panel.view = Some(view());
            panel.edit.report_interval_ms = Some(2);
            panel.read(cx);
            assert!(commands.try_recv().is_err());
            assert_eq!(panel.view.as_ref().unwrap().edit_id, view().edit_id);
            panel.discard(cx);
            assert_eq!(panel.edit, OnboardProfileEdit::default());
            assert_eq!(panel.view.as_ref().unwrap().edit_id, view().edit_id);
        });
    }

    #[gpui::test]
    fn automatic_read_runs_once_and_does_not_retry_an_uncertain_write(
        cx: &mut gpui::TestAppContext,
    ) {
        let (panel, mut commands) = setup(cx);
        panel.update(cx, OnboardPanel::activate);
        let Command::ReadOnboard(_, reply) = commands.try_recv().unwrap() else {
            panic!("read required")
        };
        drop(reply);
        cx.run_until_parked();
        panel.update(cx, OnboardPanel::activate);
        assert!(commands.try_recv().is_err());
    }

    #[gpui::test]
    fn onboard_agent_change_discards_an_inflight_read(cx: &mut gpui::TestAppContext) {
        let (panel, mut commands) = setup(cx);
        panel.update(cx, OnboardPanel::read);
        let Command::ReadOnboard(_, reply) = commands.try_recv().unwrap() else {
            panic!("read command required")
        };
        cx.update(|cx| AppState::update(cx, |_, cx| cx.emit(StateEvent::AgentChanged)));
        let _ = reply.send(Ok(view()));
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| assert!(panel.view.is_none()));
    }
}
