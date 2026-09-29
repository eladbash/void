//! The Settings screen: eight sections, saved to disk as they change.

use gpui_kit::component::input::Input;
use gpui_kit::component::slider::Slider;
use gpui_kit::{div, prelude::*, px, Context, Div, ElementId, FontWeight, Window};

use deepclean_core::config::{Density, Grouping, Theme as ThemePref};
use deepclean_core::model::Ecosystem;

use super::app_view::{config_number, AppView};
use super::components::*;
use super::drawer::{facts, safety_row, text};
use super::icons::{eco_icon, icon};
use super::results::statusbar_frame;
use super::theme::{Palette, PAD_X};
use crate::model::format::{count, format_bytes, plural, relative_time};
use crate::model::labels::eco_name;
use crate::model::ui_state::SettingsSection;

/// Paths hard-coded in `safety.rs`. Shown read-only because the user cannot
/// change them and should know they exist.
const PROTECTED: [&str; 3] = [
    "~/Documents · ~/Desktop · ~/Downloads · ~/Pictures",
    "~/Library/Keychains · ~/Library/Preferences",
    ".ssh · .gnupg · .aws · .config · .gitconfig · .zshrc · .bashrc · .env · .Trash",
];

/// What clicking one option of a segmented row does.
type SegApply = Box<dyn Fn(&mut AppView, &Window, &mut Context<AppView>)>;

/// Which list a path row belongs to, for its remove button.
#[derive(Clone, Copy)]
enum PathList {
    ScanRoots,
    Blocked,
    WorktreeRoots,
}

fn field() -> Div {
    div().mb(px(22.))
}

fn help(text: &str, p: &Palette) -> Div {
    div()
        .t_body_sm()
        .text_color(p.text_tertiary)
        .mb(px(8.))
        .child(text.to_string())
}

fn foot(text: &str, p: &Palette) -> Div {
    div()
        .t_caption()
        .text_color(p.text_disabled)
        .mt(px(8.))
        .max_w(px(420.))
        .child(text.to_string())
}

fn pathlist(p: &Palette) -> Div {
    div()
        .rounded(px(5.))
        .border_1()
        .border_color(p.border_subtle)
        .bg(p.surface_inset)
        .overflow_hidden()
}

impl AppView {
    pub(crate) fn render_settings(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let p = self.p;
        let active = self.ui.settings_section;
        let mut seg = div()
            .flex()
            .h(px(30.))
            .p(px(2.))
            .gap(px(2.))
            .rounded(px(5.))
            .border_1()
            .border_color(p.border_subtle)
            .bg(p.surface_sunken);
        for (i, s) in SettingsSection::ALL.into_iter().enumerate() {
            let on = s == active;
            seg = seg.child(
                div()
                    .id(("settings-section", i))
                    .h_full()
                    .px(px(11.))
                    .flex()
                    .items_center()
                    .rounded(px(3.))
                    .text_size(px(12.))
                    .font_weight(FontWeight(520.))
                    .map(|d| {
                        if on {
                            d.bg(p.surface_raised)
                                .text_color(p.text_primary)
                                .shadow_sm()
                        } else {
                            d.text_color(p.text_tertiary)
                        }
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.settings_section(s, cx)))
                    .child(s.label()),
            );
        }

        let body = match active {
            SettingsSection::Scanning => self.scanning_section(cx),
            SettingsSection::Ecosystems => self.ecosystems_section(cx),
            SettingsSection::Ai => self.ai_section(cx),
            SettingsSection::Guard => self.guard_section(cx),
            SettingsSection::Safety => self.safety_section(cx),
            SettingsSection::Performance => self.performance_section(window, cx),
            SettingsSection::General => self.general_section(cx),
            SettingsSection::About => self.about_section(),
        };

        let sub = if self.ui.settings_dirty {
            "Changes apply to your next scan"
        } else {
            "Saved to disk as you change them"
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(52.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .px(px(PAD_X))
                    .border_b_1()
                    .border_color(p.border_subtle)
                    .child(
                        div()
                            .flex_1()
                            .child(div().t_section_title().text_size(px(15.)).child("Settings"))
                            .child(
                                div()
                                    .t_caption()
                                    .mt(px(1.))
                                    .text_color(p.text_tertiary)
                                    .child(sub),
                            ),
                    )
                    .when(self.saved_flash, |d| {
                        d.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(4.))
                                .t_caption()
                                .text_color(p.safe_text)
                                .child(icon("check", px(14.), p.safe_text))
                                .child("Saved"),
                        )
                    }),
            )
            .child(
                div()
                    .h(px(44.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .px(px(PAD_X))
                    .border_b_1()
                    .border_color(p.border_subtle)
                    .child(seg),
            )
            .child(
                div()
                    .id("settings-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(div().px(px(PAD_X)).py(px(20.)).max_w(px(560.)).child(body)),
            )
            .when(self.ui.settings_dirty, |d| {
                d.child(
                    statusbar_frame(&p)
                        .child("Changes apply to your next scan.")
                        .child(
                            btn("rescan-now", BtnKind::Ghost, BtnSize::Sm, false, &p)
                                .h(px(20.))
                                .child("Rescan now")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.navigate(crate::model::ui_state::Route::Results, cx);
                                    this.start_scan(cx);
                                })),
                        ),
                )
            })
    }

    // ── Rows ────────────────────────────────────────────────────────────

    fn switch_row(
        &self,
        id: impl Into<ElementId>,
        label: &str,
        help_text: &str,
        on: bool,
        cx: &mut Context<Self>,
        apply: impl Fn(&mut AppView, &mut Context<AppView>) + 'static,
    ) -> impl IntoElement {
        let p = self.p;
        div()
            .id(id.into())
            .flex()
            .items_center()
            .gap(px(10.))
            .px(px(10.))
            .py(px(12.))
            .border_b_1()
            .border_color(p.border_subtle)
            .on_click(cx.listener(move |this, _, _, cx| apply(this, cx)))
            .child(
                div()
                    .flex_1()
                    .child(
                        div()
                            .t_body()
                            .text_color(p.text_primary)
                            .child(label.to_string()),
                    )
                    .when(!help_text.is_empty(), |d| {
                        d.child(
                            div()
                                .t_caption()
                                .mt(px(2.))
                                .text_color(p.text_tertiary)
                                .child(help_text.to_string()),
                        )
                    }),
            )
            .child(toggle(on, &p))
    }

    fn number_field(&self, label: &str, help_text: &str, path: &'static str, unit: &str) -> Div {
        let p = self.p;
        let input = self.number_inputs.get(path).cloned();
        field()
            .when(!label.is_empty(), |d| {
                d.child(
                    div()
                        .t_label()
                        .text_color(p.text_primary)
                        .mb(px(4.))
                        .child(label.to_string()),
                )
            })
            .when(!help_text.is_empty(), |d| d.child(help(help_text, &p)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().w(px(88.)).children(input.map(|i| Input::new(&i))))
                    .child(
                        div()
                            .t_body()
                            .text_color(p.text_secondary)
                            .child(unit.to_string()),
                    ),
            )
    }

    fn slider_field(
        &self,
        label: &str,
        path: &'static str,
        readout: String,
        help_text: &str,
    ) -> Div {
        let p = self.p;
        let slider = self.sliders.get(path).cloned();
        field()
            .mb(px(18.))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_baseline()
                    .child(div().t_label().child(label.to_string()))
                    .child(div().t_caption().text_color(p.text_tertiary).child(readout)),
            )
            .child(div().my(px(10.)).children(slider.map(|s| Slider::new(&s))))
            .when(!help_text.is_empty(), |d| {
                d.child(foot(help_text, &p).mt_0())
            })
    }

    fn path_rows(
        &self,
        paths: Vec<String>,
        list: PathList,
        empty: &str,
        cx: &mut Context<Self>,
    ) -> Div {
        let p = self.p;
        let mut l = pathlist(&p);
        if paths.is_empty() {
            return l.child(
                div()
                    .h(px(36.))
                    .flex()
                    .items_center()
                    .px(px(10.))
                    .t_mono_sm()
                    .text_color(p.text_tertiary)
                    .child(empty.to_string()),
            );
        }
        let n = paths.len();
        for (i, path) in paths.into_iter().enumerate() {
            let tag = match list {
                PathList::ScanRoots => 0,
                PathList::Blocked => 1000,
                PathList::WorktreeRoots => 2000,
            };
            l = l.child(
                div()
                    .h(px(36.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(10.))
                    .when(i + 1 < n, |d| d.border_b_1().border_color(p.border_subtle))
                    .child(icon("folder", px(14.), p.text_secondary))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .t_mono_sm()
                            .text_color(p.text_secondary)
                            .child(path),
                    )
                    .child(
                        iconbtn(("remove-path", tag + i), "close", 22., &p).on_click(cx.listener(
                            move |this, _, _, cx| {
                                let cfg = &mut this.ui.config;
                                let (v, dirty) = match list {
                                    PathList::ScanRoots => (&mut cfg.scan_roots, true),
                                    PathList::Blocked => (&mut cfg.blocked_paths, false),
                                    PathList::WorktreeRoots => {
                                        (&mut cfg.ai.extra_worktree_roots, true)
                                    }
                                };
                                if i < v.len() {
                                    v.remove(i);
                                }
                                this.save_config(dirty, cx);
                            },
                        )),
                    ),
            );
        }
        l
    }

    fn add_folder_btn(
        &self,
        id: &'static str,
        list: PathList,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let p = self.p;
        let button = btn(id, BtnKind::Secondary, BtnSize::Sm, false, &p)
            .child(icon("plus", px(14.), p.text_primary))
            .child("Add folder…")
            .on_click(cx.listener(move |this, _, _, cx| {
                this.pick_folder(cx, move |this, dir, cx| {
                    let cfg = &mut this.ui.config;
                    let dirty = match list {
                        PathList::ScanRoots => {
                            cfg.scan_roots.push(dir);
                            true
                        }
                        PathList::Blocked => {
                            cfg.blocked_paths.push(dir);
                            false
                        }
                        PathList::WorktreeRoots => {
                            cfg.ai.extra_worktree_roots.push(dir);
                            true
                        }
                    };
                    this.save_config(dirty, cx);
                });
            }));
        // In a block container a button would stretch to the full width.
        div().flex().mt(px(8.)).child(button)
    }

    // ── Sections ────────────────────────────────────────────────────────

    fn scanning_section(&mut self, cx: &mut Context<Self>) -> Div {
        let p = self.p;
        let roots = self
            .ui
            .config
            .scan_roots
            .iter()
            .map(|r| r.display().to_string())
            .collect();
        div()
            .child(
                field()
                    .child(overline("Scan roots", p.text_tertiary).mb(px(8.)))
                    .child(help("Directories Void walks. Defaults to your home folder.", &p))
                    .child(self.path_rows(roots, PathList::ScanRoots, "No scan roots — Void has nothing to walk.", cx))
                    .child(self.add_folder_btn("add-scan-root", PathList::ScanRoots, cx)),
            )
            .child(
                field()
                    .child(overline("Staleness", p.text_tertiary).mb(px(8.)))
                    .child(self.number_field("", "Flag artifacts untouched for longer than", "staleness_threshold_days", "days").mb_0())
                    .child(foot(
                        "Affects the stale marker and the “Stale only” filter. It does not change what Void scans.",
                        &p,
                    )),
            )
    }

    fn ecosystems_section(&mut self, cx: &mut Context<Self>) -> Div {
        let p = self.p;
        let enabled = self.ui.config.enabled_ecosystems.clone();
        let mut stats: Vec<(Ecosystem, u64, usize)> =
            Ecosystem::ALL.iter().map(|e| (*e, 0, 0)).collect();
        for item in &self.ui.items {
            if let Some(s) = stats.iter_mut().find(|s| s.0 == item.ecosystem) {
                s.1 += item.size_bytes;
                s.2 += 1;
            }
        }
        // Sorted by what they actually cost, so the expensive ones carry the
        // numbers that justify the toggle.
        stats.sort_by_key(|s| std::cmp::Reverse(s.1));
        let mut list = pathlist(&p);
        let n = stats.len();
        for (i, (eco, bytes, items)) in stats.into_iter().enumerate() {
            let on = enabled.contains(&eco);
            list = list.child(
                div()
                    .id(("eco", i))
                    .h(px(48.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .px(px(10.))
                    .when(i + 1 < n, |d| d.border_b_1().border_color(p.border_subtle))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let list = &mut this.ui.config.enabled_ecosystems;
                        if let Some(pos) = list.iter().position(|e| *e == eco) {
                            list.remove(pos);
                        } else {
                            list.push(eco);
                        }
                        this.save_config(true, cx);
                    }))
                    .child(eco_icon(eco, px(16.), p.eco(eco)))
                    .child(
                        div()
                            .flex_1()
                            .t_body()
                            .text_color(p.text_primary)
                            .child(eco_name(eco)),
                    )
                    .child(
                        div()
                            .t_caption()
                            .text_color(p.text_tertiary)
                            .child(if items > 0 {
                                format!(
                                    "{} · {}",
                                    format_bytes(bytes),
                                    plural(items, "item", "items")
                                )
                            } else {
                                "no results yet".into()
                            }),
                    )
                    .child(toggle(on, &p)),
            );
        }
        field()
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .mb(px(8.))
                    .child(overline("Ecosystems", p.text_tertiary))
                    .child(spacer())
                    .child(
                        btn("eco-all", BtnKind::Ghost, BtnSize::Sm, false, &p)
                            .child("Enable all")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.ui.config.enabled_ecosystems = Ecosystem::ALL.to_vec();
                                this.save_config(true, cx);
                            })),
                    )
                    .child(
                        btn("eco-none", BtnKind::Ghost, BtnSize::Sm, false, &p)
                            .child("Disable all")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.ui.config.enabled_ecosystems.clear();
                                this.save_config(true, cx);
                            })),
                    ),
            )
            .child(list)
    }

    fn ai_section(&mut self, cx: &mut Context<Self>) -> Div {
        let p = self.p;
        let ai = self.ui.config.ai.clone();
        let roots = ai
            .extra_worktree_roots
            .iter()
            .map(|r| r.display().to_string())
            .collect();
        div()
            .child(overline("AI agents", p.text_tertiary).mb(px(12.)))
            .child(self.number_field(
                "Worktree idle after",
                "An agent worktree untouched this long has its build folders offered for trimming.",
                "ai.worktree_idle_days",
                "days",
            ))
            .child(self.number_field(
                "Keep agent data for",
                "Transcripts, debug logs and file-history snapshots older than this are offered for cleanup.",
                "ai.agent_data_retention_days",
                "days",
            ))
            .child(self.number_field(
                "Large session file",
                "Flag a single agent session file bigger than this.",
                "ai.large_session_file_mb",
                "MB",
            ))
            .child(self.number_field(
                "Stale project after",
                "A git project with no commits and no edits for this long is listed as stale.",
                "ai.stale_project_days",
                "days",
            ))
            .child(field().child(pathlist(&p).child(self.switch_row(
                "toggle-dup-models",
                "Detect duplicate model files",
                "Hashes large files across model stores. The slowest part of a scan.",
                ai.detect_duplicate_models,
                cx,
                |this, cx| {
                    this.ui.config.ai.detect_duplicate_models = !this.ui.config.ai.detect_duplicate_models;
                    this.save_config(true, cx);
                },
            ))))
            .when(ai.detect_duplicate_models, |d| {
                d.child(self.number_field(
                    "Only hash model files over",
                    "Smaller files are skipped when looking for duplicates.",
                    "ai.min_duplicate_model_mb",
                    "MB",
                ))
            })
            .child(
                field()
                    .child(overline("Extra worktree folders", p.text_tertiary).mb(px(8.)))
                    .child(help(
                        "Where your agents keep worktrees, beyond the ones Void already knows (~/.cursor/worktrees, \
                         ~/.codex/worktrees, ~/conductor/workspaces, <repo>/.claude/worktrees…).",
                        &p,
                    ))
                    .child(self.path_rows(roots, PathList::WorktreeRoots, "None — only the built-in locations are checked.", cx))
                    .child(self.add_folder_btn("add-worktree-root", PathList::WorktreeRoots, cx)),
            )
    }

    fn guard_section(&mut self, cx: &mut Context<Self>) -> Div {
        let p = self.p;
        let g = self.ui.config.guard.clone();
        let mut intro =
            "Void watches free space in the background and tells you before the disk fills up."
                .to_string();
        if let Some(status) = self.guard.as_ref().and_then(|v| v.status.as_ref()) {
            intro.push_str(&format!(
                " Right now: {} free",
                format_bytes(status.free_bytes)
            ));
            if let Some(at) = self.guard.as_ref().and_then(|v| v.checked_at) {
                intro.push_str(&format!(
                    ", checked {}",
                    relative_time(at, chrono::Utc::now())
                ));
            }
            intro.push('.');
        }
        let mut policies = pathlist(&p);
        if g.policies.is_empty() {
            policies = policies.child(
                div()
                    .h(px(36.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .t_mono_sm()
                    .text_color(p.text_tertiary)
                    .child("No policies defined."),
            );
        }
        for (i, policy) in g.policies.iter().enumerate() {
            let name = if policy.name.is_empty() {
                policy.id.clone()
            } else {
                policy.name.clone()
            };
            policies = policies.child(self.switch_row(
                ("policy", i),
                &name,
                &policy.id,
                policy.enabled,
                cx,
                move |this, cx| {
                    if let Some(pol) = this.ui.config.guard.policies.get_mut(i) {
                        pol.enabled = !pol.enabled;
                    }
                    this.save_config(false, cx);
                },
            ));
        }
        div()
            .child(
                field()
                    .child(overline("Guard", p.text_tertiary).mb(px(8.)))
                    .child(help(&intro, &p))
                    .child(pathlist(&p).child(self.switch_row(
                        "toggle-guard",
                        "Watch free space",
                        "Notifies you when free space drops below the thresholds.",
                        g.enabled,
                        cx,
                        |this, cx| {
                            this.ui.config.guard.enabled = !this.ui.config.guard.enabled;
                            this.save_config(false, cx);
                        },
                    ))),
            )
            .child(self.number_field("Check every", "", "guard.check_interval_minutes", "minutes"))
            .child(self.slider_field(
                "Warn",
                "guard.warn_free_percent",
                format!("below {}% free", g.warn_free_percent),
                "",
            ))
            .child(self.slider_field(
                "Critical",
                "guard.critical_free_percent",
                format!("below {}% free", g.critical_free_percent),
                "Never above the warning level.",
            ))
            .child(
                field()
                    .child(pathlist(&p).child(self.switch_row(
                        "toggle-auto-clean",
                        "Clean automatically when space is low",
                        "Runs the policies you enable below when free space is low.",
                        g.auto_clean,
                        cx,
                        |this, cx| {
                            this.ui.config.guard.auto_clean = !this.ui.config.guard.auto_clean;
                            this.save_config(false, cx);
                        },
                    )))
                    .when(g.auto_clean, |d| {
                        d.child(div().mt(px(8.)).child(safety_row(
                            false,
                            "Runs only Safe actions, and only while CPU is below the Performance threshold. Every run is recorded in History.",
                            &p,
                        )))
                    }),
            )
            .child(
                field()
                    .child(overline("Policies", p.text_tertiary).mb(px(8.)))
                    .child(help("What automatic cleanup may do. All start off.", &p))
                    .child(policies),
            )
    }

    fn safety_section(&mut self, cx: &mut Context<Self>) -> Div {
        let p = self.p;
        let blocked = self
            .ui
            .config
            .blocked_paths
            .iter()
            .map(|r| r.display().to_string())
            .collect();
        let mut locked = pathlist(&p);
        for (i, line) in PROTECTED.iter().enumerate() {
            locked = locked.child(
                div()
                    .h(px(36.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(10.))
                    .opacity(0.8)
                    .when(i + 1 < PROTECTED.len(), |d| {
                        d.border_b_1().border_color(p.border_subtle)
                    })
                    .child(icon("lock", px(14.), p.text_tertiary))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .t_mono_sm()
                            .text_color(p.text_secondary)
                            .child(*line),
                    ),
            );
        }
        div()
            .child(
                field()
                    .child(overline("Always protected", p.text_tertiary).mb(px(8.)))
                    .child(help("Void refuses to delete these, whatever else you configure.", &p))
                    .child(locked)
                    .child(foot(
                        "Files inside Downloads, Documents, Desktop and Pictures can still be cleaned individually — \
                         it is the folders themselves that are off limits.",
                        &p,
                    )),
            )
            .child(
                field()
                    .child(overline("Never touch these", p.text_tertiary).mb(px(8.)))
                    .child(help("Your own additions. Anything inside them is left alone.", &p))
                    .child(self.path_rows(blocked, PathList::Blocked, "Add a folder Void must leave alone.", cx))
                    .child(self.add_folder_btn("add-blocked", PathList::Blocked, cx)),
            )
    }

    fn performance_section(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> Div {
        let p = self.p;
        let c = &self.ui.config;
        let cores = self.cores;
        div()
            .child(overline("Performance", p.text_tertiary).mb(px(12.)))
            .child(self.slider_field(
                "Walker threads",
                "walker_threads",
                format!("{} of {cores} cores", config_number(c, "walker_threads")),
                "",
            ))
            .child(self.slider_field(
                "Max concurrent analyses",
                "max_concurrent_analyses",
                format!("{}", config_number(c, "max_concurrent_analyses")),
                "",
            ))
            .child(self.slider_field(
                "CPU threshold",
                "cpu_threshold_percent",
                format!("{}%", config_number(c, "cpu_threshold_percent")),
                "",
            ))
            .child(foot("Guard’s automatic cleanup skips a check while overall CPU usage is above this.", &p).mt(px(-10.)).mb(px(18.)))
            .child(div().flex().child(
                btn("restore-performance", BtnKind::Secondary, BtnSize::Sm, false, &p)
                    .child("Restore defaults")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.ui.config.walker_threads = (cores / 2).max(1);
                        this.ui.config.max_concurrent_analyses = 8;
                        this.ui.config.cpu_threshold_percent = 70;
                        this.sync_controls(window, cx);
                        this.save_config(false, cx);
                    })),
            ))
    }

    fn general_section(&mut self, cx: &mut Context<Self>) -> Div {
        let p = self.p;
        let ui = self.ui.config.ui.clone();
        let seg_row = |label: &str,
                       options: Vec<(&'static str, bool, SegApply)>,
                       key: &'static str,
                       cx: &mut Context<Self>| {
            let mut seg = div()
                .flex()
                .h(px(30.))
                .p(px(2.))
                .gap(px(2.))
                .rounded(px(5.))
                .border_1()
                .border_color(p.border_subtle)
                .bg(p.surface_sunken);
            for (i, (text, on, apply)) in options.into_iter().enumerate() {
                let apply = std::rc::Rc::new(apply);
                seg = seg.child(
                    div()
                        .id(ElementId::Name(format!("{key}-{i}").into()))
                        .h_full()
                        .px(px(11.))
                        .flex()
                        .items_center()
                        .rounded(px(3.))
                        .text_size(px(12.))
                        .font_weight(FontWeight(520.))
                        .map(|d| {
                            if on {
                                d.bg(p.surface_raised)
                                    .text_color(p.text_primary)
                                    .shadow_sm()
                            } else {
                                d.text_color(p.text_tertiary)
                            }
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            apply(this, window, cx);
                            this.apply_look(window, cx);
                            this.save_config(false, cx);
                        }))
                        .child(text),
                );
            }
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
                        .t_body()
                        .text_color(p.text_primary)
                        .child(label.to_string()),
                )
                .child(seg)
        };
        let theme =
            |t: ThemePref| -> SegApply { Box::new(move |this, _, _| this.ui.config.ui.theme = t) };
        let density =
            |d: Density| -> SegApply { Box::new(move |this, _, _| this.ui.config.ui.density = d) };
        let grouping = |g: Grouping| -> SegApply {
            Box::new(move |this, _, _| {
                this.ui.config.ui.grouping = g;
                this.ui.collapsed.clear();
            })
        };
        let list = pathlist(&p)
            .child(seg_row(
                "Theme",
                vec![
                    (
                        "System",
                        ui.theme == ThemePref::System,
                        theme(ThemePref::System),
                    ),
                    (
                        "Light",
                        ui.theme == ThemePref::Light,
                        theme(ThemePref::Light),
                    ),
                    ("Dark", ui.theme == ThemePref::Dark, theme(ThemePref::Dark)),
                ],
                "theme",
                cx,
            ))
            .child(seg_row(
                "Density",
                vec![
                    (
                        "Comfortable",
                        ui.density == Density::Comfortable,
                        density(Density::Comfortable),
                    ),
                    (
                        "Compact",
                        ui.density == Density::Compact,
                        density(Density::Compact),
                    ),
                ],
                "density",
                cx,
            ))
            .child(seg_row(
                "Group by",
                vec![
                    (
                        "Ecosystem",
                        ui.grouping == Grouping::Ecosystem,
                        grouping(Grouping::Ecosystem),
                    ),
                    (
                        "Project",
                        ui.grouping == Grouping::Project,
                        grouping(Grouping::Project),
                    ),
                    (
                        "Risk",
                        ui.grouping == Grouping::Risk,
                        grouping(Grouping::Risk),
                    ),
                ],
                "grouping",
                cx,
            ))
            .child(self.switch_row(
                "toggle-confirm",
                "Confirm before cleaning",
                "Turning this off skips the risk review entirely.",
                ui.confirm_before_cleaning,
                cx,
                |this, cx| {
                    this.ui.config.ui.confirm_before_cleaning =
                        !this.ui.config.ui.confirm_before_cleaning;
                    this.save_config(false, cx);
                },
            ))
            .child(self.switch_row(
                "toggle-prefer-trash",
                "Prefer Trash where available",
                "Choose the recoverable action when an item offers one.",
                ui.prefer_trash,
                cx,
                |this, cx| {
                    this.ui.config.ui.prefer_trash = !this.ui.config.ui.prefer_trash;
                    this.save_config(false, cx);
                },
            ))
            .child(self.switch_row(
                "toggle-menu-bar",
                "Show menu bar icon",
                "Free space and quick actions, one click away.",
                ui.show_menu_bar_icon,
                cx,
                |this, cx| {
                    this.ui.config.ui.show_menu_bar_icon = !this.ui.config.ui.show_menu_bar_icon;
                    this.commit_config(cx);
                },
            ))
            .child(self.switch_row(
                "toggle-login",
                "Launch at login",
                "Start Void in the background so Guard can watch your disk.",
                ui.launch_at_login,
                cx,
                |this, cx| {
                    this.ui.config.ui.launch_at_login = !this.ui.config.ui.launch_at_login;
                    this.commit_config(cx);
                },
            ));
        field()
            .child(overline("General", p.text_tertiary).mb(px(8.)))
            .child(list)
    }

    fn about_section(&self) -> Div {
        let p = self.p;
        field()
            .child(overline("About", p.text_tertiary).mb(px(8.)))
            .child(facts(
                vec![
                    ("Version".into(), text(env!("CARGO_PKG_VERSION"))),
                    (
                        "Settings file".into(),
                        div()
                            .t_mono_sm()
                            .child(self.config_path.clone())
                            .into_any_element(),
                    ),
                    ("License".into(), text("MIT")),
                ],
                &p,
            ))
    }
}

#[allow(dead_code)]
fn unused(n: usize) -> String {
    count(n)
}
