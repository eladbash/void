//! Update checks: the automatic daily check, "Check for updates" in
//! Settings → About, and remembering what the last check found.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use chrono::Utc;
use gpui_kit::{div, prelude::*, px, Context, Div, Stateful};

use deepclean_core::update::install;
use deepclean_core::update::{Check, Feed, Installed, Relaunch};

use super::app_view::AppView;
use super::components::*;
use super::theme::PAD_X;
use crate::backend::update::CURRENT_VERSION;
use crate::drive::Effect;
use crate::model::format::format_bytes;
use crate::model::ui_state::{Route, SettingsSection};
use crate::model::update::{check_due, notice, InstallState, Notice, UpdateStatus};

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
        self.refresh_tray();
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

impl AppView {
    /// The release to announce, unless the user dismissed it this session.
    pub(crate) fn update_notice(&self) -> Option<Notice> {
        if self.notice_dismissed {
            return None;
        }
        self.pending_update()
    }

    /// The pending update regardless of this session's dismissal: what the
    /// tray offers.
    pub(crate) fn pending_update(&self) -> Option<Notice> {
        notice(&self.update, &self.ui.config.updates, CURRENT_VERSION)
    }

    /// Hide the banner until the next launch.
    pub(crate) fn dismiss_update_notice(&mut self, cx: &mut Context<Self>) {
        self.notice_dismissed = true;
        cx.notify();
    }

    /// Hide this release's banner and tray item until a newer one is out.
    pub(crate) fn skip_update(&mut self, cx: &mut Context<Self>) {
        let Some(n) = self.pending_update() else {
            return;
        };
        self.ui.config.updates.skipped_version = Some(n.version.to_string());
        self.persist_update_prefs();
        self.refresh_tray();
        cx.notify();
    }

    /// Settings → About, where the update is shown in full.
    pub(crate) fn show_update(&mut self, cx: &mut Context<Self>) {
        self.navigate(Route::Settings, cx);
        self.settings_section(SettingsSection::About, cx);
    }

    /// The banner above every screen while an update is pending.
    pub(crate) fn render_update_banner(&mut self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let n = self.update_notice()?;
        let p = self.p;
        Some(
            div()
                .id("update-banner")
                .flex_none()
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(PAD_X))
                .py(px(7.))
                .bg(p.accent_subtle)
                .border_b_1()
                .border_color(p.accent_border)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .t_body_sm()
                        .text_color(p.text_primary)
                        .child(format!("Void {} is available", n.version)),
                )
                .when_some(n.notes_url, |d, url| {
                    d.child(
                        btn("banner-notes", BtnKind::Ghost, BtnSize::Sm, false, &p)
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.open_url(url.clone(), cx)),
                            )
                            .child("What's new"),
                    )
                })
                .child(
                    btn("banner-skip", BtnKind::Ghost, BtnSize::Sm, false, &p)
                        .on_click(cx.listener(|this, _, _, cx| this.skip_update(cx)))
                        .child("Skip this version"),
                )
                .child(
                    btn("banner-show", BtnKind::Primary, BtnSize::Sm, false, &p)
                        .on_click(cx.listener(|this, _, _, cx| this.show_update(cx)))
                        .child("Update…"),
                )
                .child(
                    iconbtn("banner-close", "close", 22., &p)
                        .on_click(cx.listener(|this, _, _, cx| this.dismiss_update_notice(cx))),
                ),
        )
    }
}

impl AppView {
    /// "Update now": download, verify, install, relaunch.
    pub(crate) fn start_update(&mut self, cx: &mut Context<Self>) {
        let UpdateStatus::Available(update) = &self.update else {
            return;
        };
        if self.install.busy() {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.install_cancel = Some(cancel.clone());
        self.install = InstallState::Downloading { done: 0, total: 0 };
        let install = self.backend.install_update((**update).clone(), cancel);
        cx.spawn(async move |this, cx| {
            let result = install.await;
            let _ = this.update(cx, |this, cx| this.on_update_installed(result, cx));
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn cancel_update(&mut self, cx: &mut Context<Self>) {
        if let Some(cancel) = &self.install_cancel {
            cancel.store(true, Ordering::Relaxed);
        }
        cx.notify();
    }

    fn on_update_installed(&mut self, result: Result<Installed, String>, cx: &mut Context<Self>) {
        self.install_cancel = None;
        match result {
            Ok(Installed::Replaced { relaunch }) => {
                self.install = InstallState::Restarting;
                let path = match &relaunch {
                    Relaunch::App(p) | Relaunch::Exe(p) => p.display().to_string(),
                };
                if self.sandboxed {
                    // A sandbox updated a stand-in app; restarting would
                    // start something that isn't this build.
                    if crate::drive::active() {
                        crate::drive::capture(Effect::Relaunch(path.clone()));
                    }
                    self.install = InstallState::Installed { path };
                } else if let Err(err) = install::relaunch_after_exit(&relaunch, std::process::id())
                {
                    // Installed, but we can't start it for the user.
                    self.install = InstallState::Failed(format!(
                        "Void {} is installed; open it again to finish ({err})",
                        self.pending_update()
                            .map(|n| n.version.to_string())
                            .unwrap_or_default()
                    ));
                } else {
                    cx.quit();
                }
            }
            Ok(Installed::InstallerStarted) => {
                if self.sandboxed {
                    self.install = InstallState::Installed {
                        path: "installer".into(),
                    };
                } else {
                    self.install = InstallState::Restarting;
                    cx.quit();
                }
            }
            Ok(Installed::Manual { package, command }) => {
                self.install = InstallState::Manual {
                    package: package.display().to_string(),
                    command,
                };
            }
            Err(err) if err == "cancelled" => {
                self.install = InstallState::Idle;
                self.show_toast("Update cancelled".into(), None, cx);
            }
            Err(err) => self.install = InstallState::Failed(err),
        }
        cx.notify();
    }

    /// Settings → About: install controls while an update is available.
    pub(crate) fn update_install_row(&mut self, cx: &mut Context<Self>) -> Option<Div> {
        let UpdateStatus::Available(update) = &self.update else {
            return None;
        };
        let p = self.p;
        let has_download = update.asset.is_some();
        let notes = update.notes_url.clone();
        let row = div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .px(px(10.))
            .py(px(12.))
            .border_b_1()
            .border_color(p.border_subtle);
        let controls = |label: String| {
            div().flex().items_center().gap(px(10.)).child(
                div()
                    .flex_1()
                    .min_w_0()
                    .t_body_sm()
                    .text_color(p.text_secondary)
                    .child(label),
            )
        };
        Some(match self.install.clone() {
            InstallState::Idle | InstallState::Failed(_) => {
                let failed = match &self.install {
                    InstallState::Failed(e) => Some(e.clone()),
                    _ => None,
                };
                let action = if has_download {
                    btn("update-now", BtnKind::Primary, BtnSize::Sm, false, &p)
                        .on_click(cx.listener(|this, _, _, cx| this.start_update(cx)))
                        .child("Update now")
                } else {
                    btn(
                        "update-download",
                        BtnKind::Secondary,
                        BtnSize::Sm,
                        false,
                        &p,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.open_url(notes.clone(), cx)))
                    .child("Download from GitHub")
                };
                let label = if has_download {
                    "Downloads, verifies the release signature, installs and restarts Void."
                } else {
                    "There's no automatic update for your system; download it from the release page."
                };
                row.child(controls(label.into()).child(action))
                    .when_some(failed, |d, e| {
                        d.child(div().t_body_sm().text_color(p.danger_text).child(format!(
                            "Update failed: {e}. Your current version is unchanged."
                        )))
                    })
            }
            InstallState::Downloading { done, total } => {
                let share = if total > 0 {
                    done as f32 / total as f32
                } else {
                    0.
                };
                let label = if total > 0 {
                    format!(
                        "Downloading… {} of {}",
                        format_bytes(done),
                        format_bytes(total)
                    )
                } else {
                    format!("Downloading… {}", format_bytes(done))
                };
                row.child(
                    controls(label).child(
                        btn("update-cancel", BtnKind::Secondary, BtnSize::Sm, false, &p)
                            .on_click(cx.listener(|this, _, _, cx| this.cancel_update(cx)))
                            .child("Cancel"),
                    ),
                )
                .child(bar(share, 4., p.accent_bg, &p))
            }
            InstallState::Installing => row.child(controls("Verified. Installing…".into())),
            InstallState::Restarting => row.child(controls("Installed. Restarting Void…".into())),
            InstallState::Installed { path } => row.child(controls(format!(
                "Installed into {path}. This is a sandbox, so Void isn't restarted."
            ))),
            InstallState::Manual { package, command } => {
                let copy = command.clone();
                row.child(controls(
                    "Downloaded and verified. Install it with your package manager:".into(),
                ))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .t_mono_sm()
                                .px(px(8.))
                                .py(px(6.))
                                .rounded(px(4.))
                                .bg(p.surface_inset)
                                .child(command),
                        )
                        .child(
                            btn("update-copy", BtnKind::Secondary, BtnSize::Sm, false, &p)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.copy(copy.clone(), "Command copied", cx)
                                }))
                                .child("Copy"),
                        ),
                )
                .child(div().t_caption().text_color(p.text_tertiary).child(package))
            }
        })
    }
}
