use std::time::{SystemTime, UNIX_EPOCH};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Context, InteractiveElement as _, IntoElement, PathBuilder,
    StatefulInteractiveElement as _, Styled as _, Window, canvas, div, img, point, px,
};
use gpui_base::Button as BaseButton;
use gpui_component::{
    Disableable as _, Selectable as _, h_flex, scroll::ScrollableElement as _, v_flex,
};
use openlogi_assets::metadata::{Origin, Point};
use openlogi_device_registry::hidpp::{OnboardButton, onboard_buttons};

use super::geometry::CanvasLayout;
use super::*;
use crate::{
    services::assets::ResolvedImageView,
    ui::{
        components::{control_button, control_select},
        theme::Typography as _,
    },
};

pub(super) fn model_buttons(cx: &App) -> &'static [OnboardButton] {
    AppState::try_read(cx)
        .and_then(AppState::current_record)
        .and_then(|record| record.registry_model_id.as_deref())
        .map_or(&[], onboard_buttons)
}

fn markers(view: &ResolvedImageView, buttons: &[OnboardButton]) -> Option<Vec<(u8, Point)>> {
    let expected: Vec<_> = buttons
        .iter()
        .filter(|button| button.image_key == view.metadata.key)
        .collect();
    if expected.is_empty() {
        return None;
    }
    expected
        .into_iter()
        .map(|button| {
            let mut matches = view
                .metadata
                .assignments
                .iter()
                .filter(|slot| slot.slot_id == button.slot_id);
            let slot = matches.next()?;
            if matches.next().is_some() {
                return None;
            }
            let point = view.metadata.marker_fraction(
                slot,
                Origin {
                    width: view.png_width,
                    height: view.png_height,
                },
            )?;
            Some((button.index, point))
        })
        .collect()
}

fn assignment_title(stored: &StoredOnboardAssignment) -> SharedString {
    match stored {
        StoredOnboardAssignment::Known(action) => AssignmentOption(Some(*action)).title(),
        StoredOnboardAssignment::Unknown(_) => tr!("onboard.keep_assignment"),
    }
}

impl OnboardPanel {
    fn assignment(&self, index: u8) -> SharedString {
        if let Some(edit) = self
            .edit
            .assignments
            .iter()
            .find(|edit| edit.index == index)
        {
            return AssignmentOption(Some(edit.assignment)).title();
        }
        self.stored_assignment(index)
    }

    fn stored_assignment(&self, index: u8) -> SharedString {
        self.view
            .as_ref()
            .and_then(|view| match &view.contents {
                OnboardProfileContents::Valid(settings) => {
                    settings.assignments.get(usize::from(index))
                }
                OnboardProfileContents::InvalidProfile => None,
            })
            .map_or_else(|| tr!("onboard.keep_assignment"), assignment_title)
    }

    fn button_trigger(&self, index: u8, kind: &'static str, cx: &Context<Self>) -> BaseButton {
        let pal = theme::palette(cx);
        let selected = self.selected == index;
        let highlighted = selected || self.hovered == Some(index);
        BaseButton::new((kind, u64::from(index)))
            .when_some(
                self.button_focus.get(usize::from(index)),
                |button, handles| {
                    button.track_focus(
                        &handles[match kind {
                            "onboard-marker" => 0,
                            "onboard-callout" => 1,
                            _ => 2,
                        }],
                    )
                },
            )
            .accessibility_label(format!("G{} · {}", index + 1, self.assignment(index)))
            .selected(selected)
            .disabled(self.busy)
            .border_1()
            .border_color(if highlighted {
                theme::accent()
            } else {
                pal.border
            })
            .bg(if highlighted {
                theme::accent_tint()
            } else {
                pal.control
            })
            .text_color(if highlighted {
                theme::accent()
            } else {
                pal.text_primary
            })
            .rounded(pal.control_radius)
            .cursor_pointer()
            .focus_visible(|style| style.border_color(theme::accent()).bg(theme::accent_tint()))
            .on_hover(cx.listener(move |panel, hovered: &bool, _, cx| {
                panel.hovered = (*hovered).then_some(index);
                cx.notify();
            }))
            .on_click(cx.listener(move |panel, _, _, cx| panel.select_button(index, cx)))
    }

    fn inspector(&self, cx: &Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let mut inspector = v_flex()
            .w_full()
            .gap_3()
            .p_4()
            .rounded_xl()
            .bg(pal.panel)
            .border_1()
            .border_color(pal.border)
            .child(
                div()
                    .text_heading()
                    .child(format!("G{}", self.selected + 1)),
            )
            .child(
                div()
                    .text_body()
                    .text_color(pal.text_muted)
                    .child(tr!("onboard.assignment")),
            );
        if let Some(selector) = self.selectors.get(usize::from(self.selected)) {
            inspector = inspector.child(control_select(selector).w_full().disabled(self.busy));
        }
        inspector = inspector.child(div().text_caption().text_color(pal.text_muted).child(
            format!(
                "{} {}",
                tr!("onboard.current"),
                self.stored_assignment(self.selected)
            ),
        ));
        if self
            .edit
            .assignments
            .iter()
            .any(|edit| edit.index == self.selected)
        {
            inspector = inspector.child(div().text_caption().text_color(theme::accent()).child(
                format!(
                    "{} {}",
                    tr!("onboard.pending"),
                    self.assignment(self.selected)
                ),
            ));
        }
        inspector
            .child(
                div()
                    .mt_3()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(tr!("onboard.all_buttons")),
            )
            .children(
                (0..self.selectors.len())
                    .filter_map(|index| u8::try_from(index).ok())
                    .map(|index| {
                        self.button_trigger(index, "onboard-list", cx)
                            .w_full()
                            .px_3()
                            .py_2()
                            .flex()
                            .gap_3()
                            .child(
                                div()
                                    .w(px(24.))
                                    .text_body()
                                    .child(format!("G{}", index + 1)),
                            )
                            .child(div().flex_1().text_body().child(self.assignment(index)))
                            .when(
                                self.edit.assignments.iter().any(|edit| edit.index == index),
                                |button| {
                                    button.child(div().size_2().rounded_full().bg(theme::accent()))
                                },
                            )
                    }),
            )
    }

    fn visual_markers<'a>(&self, cx: &'a App) -> Option<(&'a ResolvedImageView, Vec<(u8, Point)>)> {
        let asset = AppState::try_read(cx)?.current_record()?.asset.as_ref()?;
        let buttons = model_buttons(cx);
        if buttons.is_empty() || buttons.len() != self.selectors.len() {
            return None;
        }
        for button in buttons {
            let mut views = asset
                .views
                .iter()
                .filter(|view| view.metadata.key == button.image_key);
            let view = views.next()?;
            if views.next().is_some() || markers(view, buttons).is_none() {
                return None;
            }
        }
        let view = asset
            .views
            .iter()
            .find(|view| view.metadata.key == self.image_key)?;
        Some((view, markers(view, buttons)?))
    }

    fn mouse_canvas(&self, width: f32, height: f32, cx: &Context<Self>) -> AnyElement {
        let pal = theme::palette(cx);
        let Some((view, markers)) = self.visual_markers(cx) else {
            return div()
                .p_5()
                .text_color(pal.text_muted)
                .child(tr!("onboard.image_unavailable"))
                .into_any_element();
        };
        let Some(layout) =
            CanvasLayout::new(width, height, (view.png_width, view.png_height), markers)
        else {
            return div()
                .child(tr!("onboard.image_unavailable"))
                .into_any_element();
        };
        let highlight = self.hovered.unwrap_or(self.selected);
        let mut lines = Vec::new();
        let mut elements = Vec::new();
        for marker in &layout.markers {
            let index = marker.index;
            let Point { x, y } = marker.marker;
            lines.push((
                point(px(x), px(y)),
                point(px(marker.anchor.x), px(marker.anchor.y)),
                index == highlight,
            ));
            elements.push(
                self.button_trigger(index, "onboard-marker", cx)
                    .absolute()
                    .left(px(x - 14.))
                    .top(px(y - 14.))
                    .size(px(28.))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_caption()
                    .child(format!("G{}", index + 1))
                    .into_any_element(),
            );
            elements.push(
                self.button_trigger(index, "onboard-callout", cx)
                    .absolute()
                    .left(px(marker.label.x))
                    .top(px(marker.label.y))
                    .w(px(layout.card_width))
                    .min_h(px(46.))
                    .px_2()
                    .py_1()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_caption().child(format!("G{}", index + 1)))
                    .child(div().text_caption().child(self.assignment(index)))
                    .into_any_element(),
            );
        }
        div()
            .relative()
            .w(px(width))
            .h(px(height))
            .child(
                img(view.image_path.clone())
                    .absolute()
                    .left(px(layout.image_left))
                    .top_0()
                    .w(px(layout.image_width))
                    .h(px(layout.image_height)),
            )
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, (), window, _| {
                        for (start, end, highlighted) in &lines {
                            let mut path =
                                PathBuilder::stroke(px(if *highlighted { 2. } else { 1. }));
                            path.move_to(bounds.origin + *start);
                            path.line_to(bounds.origin + *end);
                            if let Ok(path) = path.build() {
                                window.paint_path(
                                    path,
                                    if *highlighted {
                                        theme::accent()
                                    } else {
                                        pal.border
                                    },
                                );
                            }
                        }
                    },
                )
                .absolute()
                .size_full(),
            )
            .children(elements)
            .into_any_element()
    }

    fn polling_controls(
        &self,
        view: &OnboardProfileView,
        settings: &openlogi_core::hid::onboard_profile::OnboardProfileSettings,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let pal = theme::palette(cx);
        let selected = self
            .edit
            .report_interval_ms
            .unwrap_or(settings.report_interval_ms);
        v_flex()
            .gap_3()
            .p_4()
            .rounded_xl()
            .bg(pal.panel)
            .border_1()
            .border_color(pal.border)
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .child(div().mr_3().text_body().child(tr!("onboard.polling_rate")))
                    .children(
                        view.supported_intervals_ms
                            .iter()
                            .copied()
                            .filter(|rate| *rate != 0)
                            .map(|rate| {
                                control_button(("onboard-rate", u64::from(rate)))
                                    .label(format!("{} Hz", 1000 / u16::from(rate)))
                                    .selected(selected == rate)
                                    .disabled(self.busy)
                                    .on_click(cx.listener(move |panel, _, _, cx| {
                                        let saved =
                                            panel.view.as_ref().and_then(|view| {
                                                match &view.contents {
                                                    OnboardProfileContents::Valid(settings) => {
                                                        Some(settings.report_interval_ms)
                                                    }
                                                    OnboardProfileContents::InvalidProfile => None,
                                                }
                                            });
                                        panel.edit.report_interval_ms =
                                            (saved != Some(rate)).then_some(rate);
                                        cx.notify();
                                    }))
                            }),
                    ),
            )
            .child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(format!(
                        "{} {}",
                        tr!("onboard.active_rate"),
                        view.active_report_interval_ms
                            .as_ref()
                            .map_or_else(Clone::clone, |rate| if *rate == 0 {
                                "—".into()
                            } else {
                                format!("{} Hz", 1000 / u16::from(*rate))
                            })
                    )),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .child(div().mr_3().text_body().child(tr!("onboard.dpi_stages")))
                    .children(
                        settings
                            .dpi_stages
                            .iter()
                            .filter(|dpi| **dpi > 0)
                            .map(|dpi| {
                                div()
                                    .px_3()
                                    .py_1()
                                    .rounded_full()
                                    .bg(pal.control)
                                    .text_caption()
                                    .child(dpi.to_string())
                            }),
                    ),
            )
    }

    fn backup_controls(&self, view: &OnboardProfileView, cx: &Context<Self>) -> impl IntoElement {
        let mut content = v_flex().gap_3();
        content = content.child(div().text_body().child(tr!("onboard.restore_description")));
        for backup in &view.backups {
            let id = backup.id;
            let seconds = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
                .saturating_sub(backup.created_unix_seconds);
            let age = if seconds < 3600 {
                tr!("onboard.minutes_ago", count => seconds / 60)
            } else if seconds < 86400 {
                tr!("onboard.hours_ago", count => seconds / 3600)
            } else {
                tr!("onboard.days_ago", count => seconds / 86400)
            };
            content = content.child(
                control_button(SharedString::from(format!("backup-{id}")))
                    .label(format!("{} · {age} · {id}", tr!("onboard.restore")))
                    .disabled(self.busy || self.dirty_count() > 0)
                    .on_click(cx.listener(move |panel, _, _, cx| {
                        panel.restore_confirmation = Some(id);
                        cx.notify();
                    })),
            );
        }
        if let Some(id) = self.restore_confirmation {
            content = content.child(
                v_flex()
                    .gap_2()
                    .p_3()
                    .border_1()
                    .border_color(theme::accent())
                    .rounded_lg()
                    .child(div().text_body().child(tr!("onboard.confirm_restore")))
                    .child(div().text_caption().child(id.to_string()))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                control_button("confirm-restore")
                                    .label(tr!("onboard.restore"))
                                    .disabled(self.busy || self.dirty_count() > 0)
                                    .on_click(cx.listener(move |panel, _, _, cx| {
                                        panel.restore_confirmation = None;
                                        panel.apply(Some(id), cx);
                                    })),
                            )
                            .child(
                                control_button("cancel-restore")
                                    .label(tr!("onboard.cancel"))
                                    .on_click(cx.listener(|panel, _, _, cx| {
                                        panel.restore_confirmation = None;
                                        cx.notify();
                                    })),
                            ),
                    ),
            );
        }
        content
    }

    fn button_workspace(
        &self,
        width: f32,
        canvas_w: f32,
        canvas_h: f32,
        narrow: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let visual = v_flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        control_button("onboard-front")
                            .label(tr!("onboard.front_view"))
                            .selected(self.image_key == "device_image")
                            .on_click(cx.listener(|panel, _, _, cx| {
                                panel.image_key = "device_image";
                                cx.notify();
                            })),
                    )
                    .child(
                        control_button("onboard-side")
                            .label(tr!("onboard.side_view"))
                            .selected(self.image_key == "device_side")
                            .on_click(cx.listener(|panel, _, _, cx| {
                                panel.image_key = "device_side";
                                cx.notify();
                            })),
                    ),
            )
            .child(self.mouse_canvas(canvas_w, canvas_h, cx));

        div()
            .flex()
            .gap_5()
            .items_start()
            .when(narrow, gpui::Styled::flex_col)
            .child(visual)
            .child(
                div()
                    .w(if narrow { px(width - 32.) } else { px(280.) })
                    .flex_shrink_0()
                    .child(self.inspector(cx)),
            )
    }

    fn action_bar(&self, cx: &Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        h_flex()
            .flex_shrink_0()
            .px_5()
            .py_3()
            .gap_2()
            .border_t_1()
            .border_color(pal.border)
            .bg(pal.panel)
            .child(
                div()
                    .flex_1()
                    .text_caption()
                    .child(if self.dirty_count() == 0 {
                        tr!("onboard.no_changes")
                    } else {
                        tr!("onboard.unsaved", count => self.dirty_count())
                    }),
            )
            .child(
                control_button("onboard-backups")
                    .label(tr!("onboard.backups"))
                    .selected(self.backups_open)
                    .disabled(self.view.is_none())
                    .on_click(cx.listener(|panel, _, _, cx| {
                        panel.backups_open = !panel.backups_open;
                        cx.notify();
                    })),
            )
            .child(
                control_button("onboard-discard")
                    .label(tr!("onboard.discard"))
                    .disabled(self.busy || self.dirty_count() == 0)
                    .on_click(cx.listener(|panel, _, _, cx| panel.discard(cx))),
            )
            .child(
                control_button("apply-onboard")
                    .label(tr!("onboard.apply"))
                    .disabled(self.busy || self.dirty_count() == 0 || self.view.is_none())
                    .on_click(cx.listener(|panel, _, _, cx| panel.apply(None, cx))),
            )
    }

    pub(super) fn render_workspace(&self, window: &Window, cx: &Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let width =
            (f32::from(window.viewport_size().width) - theme::DETAIL_RAIL_W - 64.).max(320.);
        let narrow = width < 780.;
        let canvas_w = if narrow { width - 32. } else { width - 320. }.min(680.);
        let canvas_h = (f32::from(window.viewport_size().height) - 270.).clamp(350., 550.);
        let mut content = v_flex().gap_4().p_5().w_full();
        if let Some(message) = &self.message {
            content = content.child(
                div()
                    .p_3()
                    .rounded_lg()
                    .bg(pal.panel)
                    .text_body()
                    .child(message.clone()),
            );
        }
        if let Some(view) = &self.view {
            match &view.contents {
                OnboardProfileContents::InvalidProfile => {
                    content = content.child(div().child(tr!("onboard.invalid")));
                }
                OnboardProfileContents::Valid(settings) => {
                    content =
                        content.child(self.button_workspace(width, canvas_w, canvas_h, narrow, cx));
                    content = content.child(self.polling_controls(view, settings, cx));
                }
            }
            if self.backups_open {
                content = content.child(self.backup_controls(view, cx));
            }
        }
        v_flex()
            .size_full()
            .min_h_0()
            .text_color(pal.text_primary)
            .child(
                h_flex()
                    .flex_shrink_0()
                    .px_5()
                    .py_3()
                    .gap_3()
                    .border_b_1()
                    .border_color(pal.border)
                    .child(div().text_heading().child(tr!("onboard.title")))
                    .child(
                        div()
                            .flex_1()
                            .text_caption()
                            .text_color(pal.text_muted)
                            .child(tr!("onboard.saved_on_mouse")),
                    )
                    .child(
                        control_button("read-onboard")
                            .label(tr!("onboard.refresh"))
                            .disabled(
                                self.busy || self.dirty_count() > 0 || current_target(cx).is_none(),
                            )
                            .on_click(cx.listener(|panel, _, _, cx| panel.read(cx))),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(content),
            )
            .child(self.action_bar(cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openlogi_assets::metadata::{Assignment, ImageEntry, MarkerCoordinates};

    #[test]
    fn visual_markers_match_exact_slots_and_reject_duplicates_missing_slots_and_bounds() {
        let buttons = [
            OnboardButton {
                slot_id: "first",
                index: 0,
                image_key: "top",
            },
            OnboardButton {
                slot_id: "second",
                index: 1,
                image_key: "edge",
            },
        ];
        let mut view = ResolvedImageView {
            image_path: "top.png".into(),
            png_width: 800,
            png_height: 1600,
            metadata: ImageEntry {
                key: "top".into(),
                coordinates: MarkerCoordinates::Pixels,
                origin: Origin {
                    width: 800,
                    height: 1600,
                },
                assignments: vec![Assignment {
                    slot_id: "first".into(),
                    slot_name: String::new(),
                    marker: Some(Point { x: 200., y: 400. }),
                    label: openlogi_assets::metadata::Direction::default(),
                }],
            },
        };
        assert_eq!(
            markers(&view, &buttons),
            Some(vec![(0, Point { x: 0.25, y: 0.25 })])
        );
        view.metadata
            .assignments
            .push(view.metadata.assignments[0].clone());
        assert!(markers(&view, &buttons).is_none());
        view.metadata.assignments.pop();
        view.metadata.assignments[0].marker.as_mut().unwrap().x = 801.;
        assert!(markers(&view, &buttons).is_none());
        view.metadata.assignments[0].slot_id = "unknown".into();
        assert!(markers(&view, &buttons).is_none());
        view.metadata.key = "edge".into();
        assert!(markers(&view, &buttons).is_none());
        assert!(markers(&view, &[]).is_none());
    }
}
