//! The item detail drawer.

use gpui_kit::{div, prelude::*, px, AnyElement, Context, FontWeight, Window};

use deepclean_core::model::{CleanAction, CleanableItem, Ecosystem, RiskLevel};

use super::app_view::AppView;
use super::components::*;
use super::icons::{eco_icon, icon};
use super::theme::{Palette, DRAWER_W};
use crate::model::actions::{
    describe_method, is_actionable, outcome_text, risk_id, safety_lines, worktree_info, Tone,
};
use crate::model::format::{count, format_bytes, stale_long};
use crate::model::labels::{agent_name, eco_name, kind_id, kind_label, project_label};

/// A two-column facts list (label, value).
pub(crate) fn facts(rows: Vec<(String, AnyElement)>, p: &Palette) -> gpui_kit::Div {
    let mut list = div().flex().flex_col().gap(px(6.));
    for (label, value) in rows {
        list = list.child(
            div()
                .flex()
                .items_start()
                .gap(px(12.))
                .t_body_sm()
                .child(
                    div()
                        .w(px(92.))
                        .flex_none()
                        .text_color(p.text_tertiary)
                        .child(label),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(p.text_primary)
                        .child(value),
                ),
        );
    }
    list
}

pub(crate) fn text(s: impl Into<String>) -> AnyElement {
    div().child(s.into()).into_any_element()
}

fn sect(p: &Palette) -> gpui_kit::Div {
    div()
        .px(px(16.))
        .py(px(14.))
        .border_b_1()
        .border_color(p.border_subtle)
}

pub(crate) fn safety_row(ok: bool, text: &str, p: &Palette) -> gpui_kit::Div {
    let color = if ok { p.safe_text } else { p.caution_text };
    div()
        .flex()
        .items_start()
        .gap(px(7.))
        .mb(px(6.))
        .t_body_sm()
        .text_color(if ok { p.text_secondary } else { p.caution_text })
        .child(
            div()
                .mt(px(1.5))
                .child(icon(if ok { "check" } else { "warning" }, px(13.), color)),
        )
        .child(div().flex_1().min_w_0().child(text.to_string()))
}

fn risk_badge_full(risk: RiskLevel, p: &Palette) -> gpui_kit::Div {
    match risk {
        RiskLevel::Safe => badge(Tone::Safe, "safe", Some("check"), p),
        RiskLevel::Caution => badge(Tone::Caution, "caution", Some("warning"), p),
        RiskLevel::Danger => badge(Tone::Danger, "danger", Some("alert-circle"), p),
    }
}

impl AppView {
    pub(crate) fn render_drawer(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let p = self.p;
        let item = self.ui.item(self.ui.drawer?)?.clone();
        let chosen = self.ui.action_for(&item).cloned();
        let id = item.id;

        let mut head_eco = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .text_color(p.eco(item.ecosystem))
            .child(eco_icon(item.ecosystem, px(14.), p.eco(item.ecosystem)))
            .child(div().t_body_sm().child(eco_name(item.ecosystem)));
        if let Some(agent) = &item.agent {
            head_eco = head_eco.child(agent_badge_with_icon(&agent_name(agent), "sparkle", &p));
        }

        let mut fact_rows = vec![
            (
                "Size".to_string(),
                text(if item.size_bytes > 0 {
                    format_bytes(item.size_bytes)
                } else {
                    "unknown".into()
                }),
            ),
            (
                "Last modified".into(),
                text(stale_long(item.days_stale, item.last_modified)),
            ),
            ("Kind".into(), text(kind_id(item.kind))),
        ];
        if let Some(root) = &item.project_root {
            fact_rows.push((
                "Project".into(),
                div()
                    .t_mono_sm()
                    .child(root.display().to_string())
                    .into_any_element(),
            ));
        }

        let details = (!item.details.is_empty()).then(|| {
            let wt = (item.ecosystem == Ecosystem::Worktrees).then(|| worktree_info(&item));
            let mut s = sect(&p).child(overline("Details", p.text_disabled).mb(px(10.)));
            if let Some(wt) = wt.filter(|w| w.branch.is_some() || !w.chips.is_empty()) {
                let mut chips = div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(6.))
                    .mb(px(10.));
                if let Some(branch) = wt.branch {
                    chips = chips.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(3.))
                            .t_mono_xs()
                            .text_color(p.text_secondary)
                            .child(icon("git-branch", px(14.), p.text_tertiary))
                            .child(branch),
                    );
                }
                for c in wt.chips {
                    chips = chips.child(badge(c.tone, c.label, None, &p));
                }
                s = s.child(chips);
            }
            s.child(facts(
                item.details
                    .iter()
                    .map(|d| (d.label.clone(), text(d.value.clone())))
                    .collect(),
                &p,
            ))
        });

        let actions = item.available_actions.clone();
        let mut action_list = div().flex().flex_col();
        if actions.is_empty() {
            action_list = action_list.child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(7.))
                    .t_body_sm()
                    .text_color(p.text_secondary)
                    .child(icon("info", px(13.), p.text_tertiary))
                    .child("Informational — nothing to clean automatically."),
            );
        }
        for (i, a) in actions.iter().enumerate() {
            let is_chosen = chosen.as_ref().is_some_and(|c| c.id == a.id);
            let is_default = is_chosen && !self.ui.overrides.contains_key(&id);
            action_list =
                action_list.child(self.action_option(i, &item, a, is_chosen, is_default, cx));
        }

        let safety = chosen.as_ref().map(|a| {
            let mut s = sect(&p).child(overline("Safety", p.text_disabled).mb(px(10.)));
            for l in safety_lines(&a.method) {
                s = s.child(safety_row(l.ok, l.text, &p));
            }
            s
        });

        let actionable = is_actionable(&item);
        let body = div()
            .id("drawer-body")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(
                sect(&p)
                    .pb(px(12.))
                    .child(head_eco)
                    .child(div().t_page_title().mt(px(4.)).child(kind_label(&item)))
                    .when_some(project_label(&item), |d, proj| {
                        d.child(div().t_body_sm().text_color(p.text_tertiary).child(proj))
                    }),
            )
            .child(
                sect(&p)
                    .child(
                        div()
                            .px(px(10.))
                            .py(px(8.))
                            .rounded(px(5.))
                            .border_1()
                            .border_color(p.border_subtle)
                            .bg(p.surface_inset)
                            .text_color(p.text_code)
                            .t_mono_xs()
                            .child(item.path.display().to_string()),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .mt(px(8.))
                            .child(
                                btn("drawer-copy", BtnKind::Secondary, BtnSize::Sm, false, &p)
                                    .child(icon("copy", px(14.), p.text_primary))
                                    .child("Copy")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if let Some(i) = this.ui.item(id) {
                                            let path = i.path.display().to_string();
                                            this.copy(path, "Path copied", cx);
                                        }
                                    })),
                            )
                            .child(
                                btn("drawer-reveal", BtnKind::Secondary, BtnSize::Sm, false, &p)
                                    .child(icon("external-link", px(14.), p.text_primary))
                                    .child("Reveal")
                                    .on_click(
                                        cx.listener(move |this, _, _, cx| this.reveal(id, cx)),
                                    ),
                            ),
                    ),
            )
            .child(sect(&p).child(facts(fact_rows, &p)))
            .children(details)
            .child(
                sect(&p)
                    .child(overline("Choose an action", p.text_disabled).mb(px(10.)))
                    .when(actions.len() > 6, |d| {
                        d.child(
                            div()
                                .t_caption()
                                .text_color(p.text_tertiary)
                                .mb(px(8.))
                                .child(format!("{} actions available", count(actions.len()))),
                        )
                    })
                    .child(action_list),
            )
            .children(safety);

        let drawer = div()
            .id("drawer")
            .absolute()
            .top_0()
            .right_0()
            .bottom_0()
            .w(px(DRAWER_W))
            .flex()
            .flex_col()
            .bg(p.surface_raised)
            .border_l_1()
            .border_color(p.border_subtle)
            .shadow_lg()
            .occlude()
            .child(
                div()
                    .h(px(44.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .pl(px(16.))
                    .pr(px(12.))
                    .border_b_1()
                    .border_color(p.border_subtle)
                    .child(
                        iconbtn("drawer-prev", "chevron-up", 28., &p)
                            .tooltip_text("Previous item (⌥↑)")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.ui.move_drawer(-1);
                                cx.notify();
                            })),
                    )
                    .child(
                        iconbtn("drawer-next", "chevron-down", 28., &p)
                            .tooltip_text("Next item (⌥↓)")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.ui.move_drawer(1);
                                cx.notify();
                            })),
                    )
                    .child(spacer())
                    .child(
                        iconbtn("drawer-close", "close", 28., &p).on_click(cx.listener(
                            |this, _, _, cx| {
                                this.ui.drawer = None;
                                cx.notify();
                            },
                        )),
                    ),
            )
            .child(body)
            .child(
                div()
                    .flex_none()
                    .px(px(16.))
                    .py(px(12.))
                    .border_t_1()
                    .border_color(p.border_subtle)
                    .flex()
                    .gap(px(8.))
                    .child(
                        btn("drawer-exclude", BtnKind::Secondary, BtnSize::Md, false, &p)
                            .flex_1()
                            .child("Exclude this path")
                            .on_click(cx.listener(move |this, _, _, cx| this.exclude(id, cx))),
                    )
                    .child(
                        btn(
                            "drawer-clean",
                            BtnKind::Primary,
                            BtnSize::Md,
                            !actionable,
                            &p,
                        )
                        .flex_1()
                        .child("Clean this item")
                        .when(actionable, |b| {
                            b.on_click(cx.listener(move |this, _, window, cx| {
                                this.clean_one(id, window, cx)
                            }))
                        }),
                    ),
            );

        // Below 1280pt the drawer overlays the list with a scrim, as before.
        Some(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left_0()
                .right_0()
                .child(
                    div()
                        .id("drawer-scrim")
                        .absolute()
                        .size_full()
                        .bg(p.surface_scrim)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.ui.drawer = None;
                            cx.notify();
                        })),
                )
                .child(drawer)
                .into_any_element(),
        )
    }

    fn action_option(
        &self,
        i: usize,
        item: &CleanableItem,
        action: &CleanAction,
        chosen: bool,
        is_default: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let p = self.p;
        let m = describe_method(&action.method);
        let out = outcome_text(action);
        let (item_id, action_id) = (item.id, action.id);
        let mut cmd = format!("{} {}", m.glyph, m.text);
        if let Some(dir) = &m.working_dir {
            cmd.push_str(&format!("\nin {dir}"));
        }
        if !m.paths.is_empty() {
            for path in m.paths.iter().take(6) {
                cmd.push('\n');
                cmd.push_str(path);
            }
            if m.paths.len() > 6 {
                cmd.push_str(&format!("\n…and {} more", count(m.paths.len() - 6)));
            }
        }
        div()
            .id(("action-opt", i))
            .flex()
            .gap(px(10.))
            .py(px(10.))
            .border_b_1()
            .border_color(p.border_subtle)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.ui.choose_action(item_id, action_id);
                this.changed(cx);
            }))
            .child(radio(chosen, &p))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(px(8.))
                            .mb(px(2.))
                            .child(
                                div()
                                    .t_body()
                                    .font_weight(FontWeight(540.))
                                    .child(action.label.clone()),
                            )
                            .child(risk_badge_full(action.risk, &p))
                            .when(is_default, |d| {
                                d.child(badge(Tone::Neutral, "default", None, &p))
                            }),
                    )
                    .child(
                        div()
                            .t_body_sm()
                            .text_color(p.text_tertiary)
                            .mb(px(4.))
                            .child(action.description.clone()),
                    )
                    .child(cmdline(cmd, &p))
                    .child(
                        div()
                            .mt(px(4.))
                            .flex()
                            .gap(px(4.))
                            .t_caption()
                            .text_color(p.text_disabled)
                            .child(format!(
                                "Frees {} ·",
                                out.frees
                                    .map(format_bytes)
                                    .unwrap_or_else(|| "unknown".into())
                            ))
                            .child(
                                div()
                                    .when(out.recoverable, |d| d.text_color(p.safe_text))
                                    .child(out.note),
                            ),
                    ),
            )
            .map(|d| {
                let _ = risk_id(action.risk);
                d
            })
    }
}
