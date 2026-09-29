//! The Agents screen: what AI tools cost you, whether the disk is safe, and
//! how to let the agents ask Void themselves.

use gpui_kit::{div, prelude::*, px, Context, Div, FontWeight, Window};

use deepclean_core::guard::DiskState;
use deepclean_core::model::{CleanableItem, Ecosystem};

use super::app_view::AppView;
use super::components::*;
use super::icons::{eco_icon, icon};
use super::theme::{Palette, PAD_X};
use crate::backend::integrations::MCP_COMMAND;
use crate::model::actions::Tone;
use crate::model::agents::{category_slot, preset_hint};
use crate::model::format::{count, format_bytes, percent, plural, relative_time, split_bytes};
use crate::model::selection::total_bytes_of;
use crate::model::ui_state::{matches_preset, preset_for, PresetId, SettingsSection};

fn acard(p: &Palette) -> Div {
    card(p).flex().flex_col().px(px(16.)).py(px(14.)).min_w_0()
}

fn acard_head(p: &Palette) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .mb(px(8.))
        .min_h(px(18.))
        .text_color(p.text_tertiary)
}

fn cli_hint(p: &Palette) -> Div {
    div()
        .mt(px(8.))
        .flex()
        .items_start()
        .gap(px(7.))
        .t_body_sm()
        .text_color(p.caution_text)
        .child(div().mt(px(1.5)).child(icon("warning", px(13.), p.caution_text)))
        .child(div().flex_1().min_w_0().child(
            "Needs the void command-line tool on your PATH. Install it with cargo install --path crates/void-cli \
             or from the release download.",
        ))
}

impl AppView {
    pub(crate) fn render_agents(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let p = self.p;
        // Two columns only while each card keeps a readable width.
        let wide = window.viewport_size().width >= px(760.);
        let grid = |a: Div, b: Div| {
            div()
                .flex()
                .when(!wide, |d| d.flex_col())
                .gap(px(12.))
                .child(a.flex_1().flex_basis(px(0.)))
                .child(b.flex_1().flex_basis(px(0.)))
        };
        let content = div()
            .flex()
            .flex_col()
            .gap(px(20.))
            .px(px(PAD_X))
            .pt(px(16.))
            .pb(px(24.))
            .max_w(px(880.))
            .child(grid(self.guard_card(cx), self.quick_filters(cx)))
            .child(self.usage_section(cx))
            .child(
                div()
                    .child(
                        div()
                            .mb(px(10.))
                            .t_section_title()
                            .text_color(p.text_primary)
                            .child("Integrations"),
                    )
                    .child(grid(self.hooks_card(cx), self.mcp_card(cx))),
            );
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(self.cmdbar(
                "Agents",
                "Void cleans up after your AI agents — and your agents can call Void themselves.".into(),
                false,
                cx,
            ))
            .child(div().id("agents-scroll").flex_1().min_h_0().overflow_y_scroll().child(content))
    }

    fn guard_card(&self, cx: &mut Context<Self>) -> Div {
        let p = self.p;
        let g = self.guard.as_ref();
        let status = g.and_then(|g| g.status.as_ref());
        let cfg = &self.ui.config.guard;
        let state_badge = match (g, status) {
            (Some(g), _) if !g.enabled => Some(badge(Tone::Neutral, "off", None, &p)),
            (_, Some(s)) => Some(match s.state {
                DiskState::Critical => badge(Tone::Danger, "critical", Some("alert-circle"), &p),
                DiskState::Low => badge(Tone::Caution, "low", Some("warning"), &p),
                DiskState::Ok => badge(Tone::Safe, "ok", Some("check"), &p),
            }),
            _ => None,
        };
        let mut body = acard(&p).child(
            acard_head(&p)
                .child(overline("Guard", p.text_tertiary))
                .child(spacer())
                .children(state_badge),
        );
        match status {
            Some(s) => {
                let (n, u) = split_bytes(Some(s.free_bytes));
                let used = if s.total_bytes > 0 {
                    ((s.total_bytes - s.free_bytes) as f32 / s.total_bytes as f32).min(1.)
                } else {
                    0.
                };
                let warn = cfg.warn_free_percent as f32;
                body = body
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap(px(4.))
                            .child(div().t_metric_sm().child(n))
                            .child(
                                div()
                                    .t_body_sm()
                                    .text_color(p.text_tertiary)
                                    .child(format!("{u} free")),
                            )
                            .child(spacer())
                            .child(div().t_caption().text_color(p.text_tertiary).child(format!(
                                "{} of {}",
                                percent(s.free_bytes, s.total_bytes),
                                format_bytes(s.total_bytes)
                            ))),
                    )
                    .child(
                        div()
                            .mt(px(8.))
                            .relative()
                            .h(px(8.))
                            .child(
                                div()
                                    .size_full()
                                    .rounded(px(3.))
                                    .bg(p.chart_track)
                                    .overflow_hidden()
                                    .child(
                                        div()
                                            .h_full()
                                            .w(gpui_kit::relative(used))
                                            .bg(p.text_disabled),
                                    ),
                            )
                            .when(warn > 0., |d| {
                                d.child(
                                    div()
                                        .absolute()
                                        .top(px(-3.))
                                        .bottom(px(-3.))
                                        .w(px(2.))
                                        .left(gpui_kit::relative(
                                            ((100. - warn) / 100.).clamp(0., 1.),
                                        ))
                                        .rounded(px(1.))
                                        .bg(p.caution_border_strong),
                                )
                            }),
                    )
                    .when(!s.message.is_empty(), |d| {
                        d.child(
                            div()
                                .mt(px(8.))
                                .t_body_sm()
                                .text_color(p.text_secondary)
                                .child(s.message.clone()),
                        )
                    });
            }
            None => {
                body = body.child(
                    div()
                        .my(px(6.))
                        .t_body_sm()
                        .text_color(p.text_tertiary)
                        .child(if g.is_some() {
                            "Not checked yet."
                        } else {
                            "Guard status unavailable."
                        }),
                );
            }
        }
        let mut line = match g.and_then(|g| g.checked_at) {
            Some(at) => format!("Checked {}", relative_time(at, chrono::Utc::now())),
            None => "Never checked".into(),
        };
        if let Some(g) = g.filter(|g| g.enabled) {
            line.push_str(&format!(
                " · every {} min",
                count(cfg.check_interval_minutes as usize)
            ));
            line.push_str(if g.auto_clean {
                " · automatic cleanup on"
            } else {
                " · notifications only"
            });
        }
        body = body.child(
            div()
                .mt(px(8.))
                .t_caption()
                .text_color(p.text_tertiary)
                .child(line),
        );
        if let Some(last) = g.and_then(|g| g.last_auto_clean.as_ref()) {
            body = body.child(
                div()
                    .mt(px(4.))
                    .t_caption()
                    .text_color(p.safe_text)
                    .child(format!(
                        "Last automatic cleanup freed {} · {}{}",
                        format_bytes(last.bytes_freed),
                        relative_time(last.at, chrono::Utc::now()),
                        if last.failed > 0 {
                            format!(" · {} failed", count(last.failed))
                        } else {
                            String::new()
                        }
                    )),
            );
        }
        if let Some(note) = g.and_then(|g| g.note.clone()) {
            body = body.child(
                div()
                    .mt(px(4.))
                    .t_caption()
                    .text_color(p.caution_text)
                    .child(note),
            );
        }
        let checking = self.guard_checking;
        body.child(
            div()
                .mt_auto()
                .pt(px(12.))
                .flex()
                .items_center()
                .gap(px(8.))
                .child(
                    btn("guard-check", BtnKind::Secondary, BtnSize::Sm, checking, &p)
                        .map(|b| {
                            if checking {
                                b.child(spinner("guard-spin", 12., p.text_primary))
                                    .child("Checking…")
                            } else {
                                b.child(icon("refresh", px(14.), p.text_primary))
                                    .child("Check now")
                            }
                        })
                        .when(!checking, |b| {
                            b.on_click(cx.listener(|this, _, _, cx| this.guard_check(cx)))
                        }),
                )
                .child(
                    btn("guard-settings", BtnKind::Ghost, BtnSize::Sm, false, &p)
                        .child("Guard settings")
                        .child(icon("chevron-right", px(14.), p.text_secondary))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.ui.route = crate::model::ui_state::Route::Settings;
                            this.settings_section(SettingsSection::Guard, cx);
                        })),
                ),
        )
    }

    fn quick_filters(&self, cx: &mut Context<Self>) -> Div {
        let p = self.p;
        let mut list = div().flex().flex_col().mx(px(-8.));
        for (i, id) in PresetId::ALL.into_iter().enumerate() {
            let preset = preset_for(id, &self.ui.config);
            let eco = match id {
                PresetId::IdleWorktrees => Ecosystem::Worktrees,
                PresetId::OldTranscripts => Ecosystem::AgentData,
                PresetId::DuplicateModels => Ecosystem::Models,
            };
            let matches: Vec<&CleanableItem> = self
                .ui
                .items
                .iter()
                .filter(|i| matches_preset(i, Some(&preset)))
                .collect();
            let stat = if self.ui.has_scanned {
                format!(
                    "{} · {}",
                    count(matches.len()),
                    format_bytes(total_bytes_of(&matches))
                )
            } else {
                "—".into()
            };
            let hover = p.state_hover;
            list = list.child(
                div()
                    .id(("preset", i))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .p(px(8.))
                    .rounded(px(5.))
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(move |this, _, _, cx| this.apply_preset(id, cx)))
                    .child(eco_icon(eco, px(14.), p.eco(eco)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .t_body()
                                    .text_color(p.text_primary)
                                    .child(preset.label),
                            )
                            .child(
                                div()
                                    .t_caption()
                                    .text_color(p.text_tertiary)
                                    .child(preset_hint(id, preset.min_days)),
                            ),
                    )
                    .child(div().t_caption().text_color(p.text_tertiary).child(stat))
                    .child(icon("chevron-right", px(14.), p.text_tertiary)),
            );
        }
        acard(&p)
            .child(acard_head(&p).child(overline("Jump to", p.text_tertiary)))
            .child(list)
    }

    fn usage_section(&self, cx: &mut Context<Self>) -> Div {
        let p = self.p;
        let head = div()
            .flex()
            .items_baseline()
            .gap(px(10.))
            .mb(px(10.))
            .child(
                div()
                    .t_section_title()
                    .text_color(p.text_primary)
                    .child("Usage by agent"),
            )
            .child(
                div()
                    .t_caption()
                    .text_color(p.text_tertiary)
                    .child("Nested items are counted once"),
            );
        let usage = self.agent_usage.clone().unwrap_or_default();
        let body = if !self.ui.has_scanned {
            super::results::state_block(
                &p,
                "sparkle",
                "Scan to see what each agent left behind",
                "Worktrees, transcripts, browsers and models, attributed to the tool that created them.",
            )
            .py(px(28.))
            .child(
                div().mt(px(16.)).child(
                    btn("agents-scan", BtnKind::Primary, BtnSize::Sm, self.ui.scanning, &p)
                        .child(icon("scan", px(14.), p.accent_on_solid))
                        .child("Scan now")
                        .on_click(cx.listener(|this, _, _, cx| this.start_scan(cx))),
                ),
            )
        } else if usage.is_empty() {
            super::results::state_block(
                &p,
                "shield-check",
                "No AI tool data found",
                "The last scan found nothing Void could attribute to an AI agent or model runner.",
            )
            .py(px(28.))
        } else {
            let max = usage
                .iter()
                .map(|a| a.total_bytes)
                .max()
                .unwrap_or(1)
                .max(1);
            let mut rows = div()
                .rounded(px(7.))
                .border_1()
                .border_color(p.border_subtle)
                .bg(p.surface_raised);
            for (i, a) in usage.iter().enumerate() {
                let cats: Vec<_> = a.categories.iter().filter(|c| c.bytes > 0).collect();
                let mut bar =
                    div()
                        .h_full()
                        .min_w(px(4.))
                        .flex()
                        .gap(px(2.))
                        .w(gpui_kit::relative(
                            (a.total_bytes as f32 / max as f32).max(0.01),
                        ));
                for (j, c) in cats.iter().enumerate() {
                    bar = bar.child(
                        div()
                            .id(("seg", i * 100 + j))
                            .h_full()
                            .min_w(px(3.))
                            .flex_grow(c.bytes as f32)
                            .flex_basis(px(0.))
                            .flex_shrink(1.)
                            .rounded(px(2.))
                            .bg(p.series(category_slot(&c.label)))
                            .tooltip_text(format!(
                                "{} · {} · {}",
                                c.label,
                                format_bytes(c.bytes),
                                plural(c.count, "item", "items")
                            )),
                    );
                }
                let mut legend = div()
                    .flex()
                    .flex_wrap()
                    .gap_x(px(14.))
                    .gap_y(px(4.))
                    .mt(px(8.))
                    .t_caption();
                for c in &cats {
                    legend = legend.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(5.))
                            .text_color(p.text_secondary)
                            .child(
                                div()
                                    .size(px(8.))
                                    .rounded(px(2.))
                                    .bg(p.series(category_slot(&c.label))),
                            )
                            .child(c.label.clone())
                            .child(
                                div()
                                    .text_color(p.text_tertiary)
                                    .font_weight(FontWeight(500.))
                                    .child(format_bytes(c.bytes)),
                            ),
                    );
                }
                rows = rows.child(
                    div()
                        .px(px(16.))
                        .py(px(12.))
                        .when(i + 1 < usage.len(), |d| {
                            d.border_b_1().border_color(p.border_subtle)
                        })
                        .child(
                            div()
                                .flex()
                                .items_baseline()
                                .gap(px(8.))
                                .child(div().t_body_strong().text_color(p.text_primary).child(
                                    if a.display_name.is_empty() {
                                        a.agent.clone()
                                    } else {
                                        a.display_name.clone()
                                    },
                                ))
                                .child(div().t_caption().text_color(p.text_tertiary).child(plural(
                                    a.item_count,
                                    "item",
                                    "items",
                                )))
                                .child(spacer())
                                .child(div().t_body_strong().child(format_bytes(a.total_bytes))),
                        )
                        .child(div().mt(px(8.)).h(px(10.)).child(bar))
                        .child(legend),
                );
            }
            rows
        };
        div().child(head).child(body)
    }

    fn hooks_card(&self, cx: &mut Context<Self>) -> Div {
        let p = self.p;
        let h = self.hooks.as_ref();
        let cli_missing = h.is_some_and(|h| h.cli_path.is_none());
        let installed = h.is_some_and(|h| h.status.installed);
        let mut c = acard(&p)
            .child(
                acard_head(&p)
                    .child(div().t_body_strong().text_color(p.text_primary).child("Claude Code hooks"))
                    .child(spacer())
                    .when_some(h, |d, h| {
                        d.child(if h.status.installed {
                            badge(Tone::Safe, "installed", Some("check"), &p)
                        } else {
                            badge(Tone::Neutral, "not installed", None, &p)
                        })
                    }),
            )
            .child(div().t_body_sm().text_color(p.text_secondary).child(
                "Warns your agent at session start when the disk is low, and trims a worktree’s build folders when \
                 the agent removes it.",
            ));
        if let Some(h) = h {
            c = c
                .child(
                    div()
                        .mt(px(8.))
                        .t_caption()
                        .text_color(p.text_tertiary)
                        .child("Settings file"),
                )
                .child(
                    div()
                        .mt(px(2.))
                        .child(cmdline(h.status.settings_path.display().to_string(), &p)),
                );
            if let Some(cmd) = &h.status.command {
                c = c
                    .child(
                        div()
                            .mt(px(6.))
                            .t_caption()
                            .text_color(p.text_tertiary)
                            .child("Runs"),
                    )
                    .child(div().mt(px(2.)).child(cmdline(cmd.clone(), &p)));
            }
        }
        if let Some(err) = &self.hooks_error {
            c = c.child(error_plate(err.clone(), &p).mt(px(8.)));
        } else if cli_missing {
            c = c.child(cli_hint(&p));
        }
        c.child(
            div()
                .mt_auto()
                .pt(px(12.))
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(8.))
                .child(if installed {
                    btn(
                        "hooks-uninstall",
                        BtnKind::Secondary,
                        BtnSize::Sm,
                        false,
                        &p,
                    )
                    .child("Uninstall")
                    .on_click(cx.listener(|this, _, _, cx| this.hooks(false, cx)))
                } else {
                    btn(
                        "hooks-install",
                        BtnKind::Primary,
                        BtnSize::Sm,
                        cli_missing,
                        &p,
                    )
                    .child("Install hooks")
                    .when(!cli_missing, |b| {
                        b.on_click(cx.listener(|this, _, _, cx| this.hooks(true, cx)))
                    })
                })
                .child(
                    div()
                        .t_caption()
                        .text_color(p.text_tertiary)
                        .child("A backup of your settings is written first."),
                ),
        )
    }

    fn mcp_card(&self, cx: &mut Context<Self>) -> Div {
        let p = self.p;
        let command = self
            .mcp
            .as_ref()
            .map(|m| m.command.clone())
            .unwrap_or_else(|| MCP_COMMAND.into());
        let copy_row = |id: &'static str, text: String, pre: bool, cx: &mut Context<Self>| {
            let t = text.clone();
            div()
                .mt(px(3.))
                .flex()
                .items_start()
                .gap(px(6.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(cmdline(text, &p).when(pre, |d| d.max_h(px(160.)))),
                )
                .child(iconbtn(id, "copy", 28., &p).tooltip_text("Copy").on_click(
                    cx.listener(move |this, _, _, cx| this.copy(t.clone(), "Copied", cx)),
                ))
        };
        let mut c = acard(&p)
            .child(
                acard_head(&p)
                    .child(
                        div()
                            .t_body_strong()
                            .text_color(p.text_primary)
                            .child("MCP server"),
                    )
                    .child(spacer())
                    .child(icon("plug", px(14.), p.text_tertiary)),
            )
            .child(div().t_body_sm().text_color(p.text_secondary).child(
                "Let your agents check disk space and propose cleanups — you approve every plan.",
            ))
            .child(
                div()
                    .mt(px(10.))
                    .t_caption()
                    .text_color(p.text_tertiary)
                    .child("Claude Code"),
            )
            .child(copy_row("copy-mcp-command", command, false, cx));
        if let Some(m) = &self.mcp {
            c = c
                .child(
                    div()
                        .mt(px(10.))
                        .t_caption()
                        .text_color(p.text_tertiary)
                        .child("Other agents — add to the MCP config"),
                )
                .child(copy_row("copy-mcp-json", m.json.clone(), true, cx));
        } else if self.mcp_error.is_some() {
            c = c.child(cli_hint(&p));
        }
        c
    }
}
