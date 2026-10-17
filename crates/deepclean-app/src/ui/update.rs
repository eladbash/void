//! Update checks: the automatic daily check, "Check for updates" in
//! Settings → About, and remembering what the last check found.

use chrono::Utc;
use gpui_kit::{div, prelude::*, px, Context, Div};

use deepclean_core::update::{Check, Feed};

use super::app_view::AppView;
use super::components::*;
use crate::backend::update::CURRENT_VERSION;
use crate::drive::Effect;
use crate::model::update::{check_due, UpdateStatus};

impl AppView {
    /// Run the automatic check if it is on, a day has passed, and this run
    /// can check at all (a sandbox needs a local feed).
    pub(crate) fn maybe_check_for_updates(&mut self, cx: &mut Context<Self>) {
        if check_due(&self.ui.config.updates, Utc::now()) && Feed::from_env().is_some() {
            self.check_for_updates(false, cx);
        }
    }

    /// Ask for the latest release. A manual check reports failures; the
    /// automatic one stays quiet about them.
    pub(crate) fn check_for_updates(&mut self, manual: bool, cx: &mut Context<Self>) {
        if matches!(self.update, UpdateStatus::Checking { .. }) {
            return;
        }
        self.update = UpdateStatus::Checking { manual };
        let check = self.backend.check_for_updates();
        self.update_task = Some(cx.spawn(async move |this, cx| {
            let result = check.await;
            let _ = this.update(cx, |this, cx| this.on_update_checked(result, manual, cx));
        }));
        cx.notify();
    }

    fn on_update_checked(
        &mut self,
        result: Result<Check, String>,
        manual: bool,
        cx: &mut Context<Self>,
    ) {
        self.update_task = None;
        let prefs = &mut self.ui.config.updates;
        match result {
            Ok(Check::UpToDate { latest }) => {
                prefs.last_checked_at = Some(Utc::now());
                prefs.latest_version = Some(latest.to_string());
                prefs.latest_notes_url = None;
                self.update = UpdateStatus::UpToDate {
                    latest: latest.to_string(),
                };
            }
            Ok(Check::Available(update)) => {
                prefs.last_checked_at = Some(Utc::now());
                prefs.latest_version = Some(update.version.to_string());
                prefs.latest_notes_url = Some(update.notes_url.clone());
                self.update = UpdateStatus::Available(Box::new(update));
            }
            Err(err) if manual => self.update = UpdateStatus::Failed(err),
            // Offline at launch is normal; try again next launch.
            Err(_) => self.update = UpdateStatus::Idle,
        }
        self.persist_update_prefs();
        cx.notify();
    }

    /// Write the update fields alone, without the "Saved" flash a settings
    /// change shows: the user didn't change anything.
    pub(crate) fn persist_update_prefs(&mut self) {
        let state = self.backend.state().clone();
        state.config().updates = self.ui.config.updates.clone();
        if let Err(err) = state.persist_config() {
            eprintln!("{err}");
        }
    }

    pub(crate) fn open_url(&mut self, url: String, cx: &mut Context<Self>) {
        if crate::drive::active() {
            crate::drive::capture(Effect::OpenUrl(url));
        } else {
            cx.open_url(&url);
        }
    }

    /// Settings → About: the check's status and the button that runs it.
    pub(crate) fn update_status_row(&mut self, cx: &mut Context<Self>) -> Div {
        let p = self.p;
        let checking = matches!(self.update, UpdateStatus::Checking { .. });
        let tone = match self.update {
            UpdateStatus::Available(_) => p.accent_text,
            UpdateStatus::Failed(_) => p.danger_text,
            _ => p.text_primary,
        };
        let notes = match &self.update {
            UpdateStatus::Available(u) => Some(u.notes_url.clone()),
            _ => None,
        };
        div()
            .flex()
            .items_center()
            .gap(px(10.))
            .px(px(10.))
            .py(px(12.))
            .border_b_1()
            .border_color(p.border_subtle)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .t_body()
                    .text_color(tone)
                    .child(self.update.summary(CURRENT_VERSION)),
            )
            .when_some(notes, |d, url| {
                d.child(
                    btn("update-notes", BtnKind::Ghost, BtnSize::Sm, false, &p)
                        .on_click(cx.listener(move |this, _, _, cx| this.open_url(url.clone(), cx)))
                        .child("What's new"),
                )
            })
            .child(
                btn(
                    "check-updates",
                    BtnKind::Secondary,
                    BtnSize::Sm,
                    checking,
                    &p,
                )
                .when(!checking, |b| {
                    b.on_click(cx.listener(|this, _, _, cx| this.check_for_updates(true, cx)))
                })
                .child("Check for updates"),
            )
    }
}
