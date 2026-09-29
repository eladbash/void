//! The History screen and the run detail modal.

use gpui_kit::{div, prelude::*, px, AnyElement, Context, FontWeight, Window};

use deepclean_core::history::{CleanRun, RunOutcome};

use super::app_view::AppView;
use super::components::*;
use super::confirm::{modal_foot, modal_frame, modal_head};
use super::icons::icon;
use super::results::{state_block, statusbar_frame};
use super::theme::{COLHEAD_H, PAD_X};
use crate::backend::guard::GUARD_TRIGGER;
use crate::model::format::{count, duration, format_bytes, medium_date_time, split_bytes};
use crate::model::ui_state::Route;

fn succeeded(run: &CleanRun) -> usize {
    run.items
        .iter()
        .filter(|i| i.result == RunOutcome::Succeeded)
        .count()
}

impl AppView {
    pub(crate) fn render_history(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let p = self.p;
        let h = self.history.clone().unwrap_or_default();
        let runs = &h.runs;

        let sub = format!(
            "{} run{} · {} reclaimed",
            count(h.run_count),
            if h.run_count == 1 { "" } else { "s" },
            format_bytes(h.total_bytes)
        );
        let head = div()
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
                    .child(div().t_section_title().text_size(px(15.)).child("History"))
                    .child(
                        div()
                            .t_caption()
                            .mt(px(1.))
                            .text_color(p.text_tertiary)
                            .child(sub),
                    ),
            )
            .when(!runs.is_empty(), |d| {
                d.child(
                    btn("clear-history", BtnKind::Ghost, BtnSize::Sm, false, &p)
                        .child("Clear history")
                        .on_click(cx.listener(|this, _, _, cx| this.clear_history(cx))),
                )
            });

        let mut col = div().size_full().flex().flex_col().child(head);
        if runs.is_empty() {
            col = col.child(
                div().flex_1().child(
                    state_block(
                        &p,
                        "history",
                        "No cleans yet",
                        "Space you reclaim will show up here, with the exact command that freed it.",
                    )
                    .child(
                        div().mt(px(16.)).child(
                            btn("goto-results", BtnKind::Secondary, BtnSize::Md, false, &p)
                                .child("Go to results")
                                .on_click(cx.listener(|this, _, _, cx| this.navigate(Route::Results, cx))),
                        ),
                    ),
                ),
            );
        } else {
            let (tn, tu) = split_bytes(Some(h.total_bytes));
            let (ln, lu) = split_bytes(Some(h.bytes_last_30_days));
            let tile = |k: &str, v: String| {
                div()
                    .flex_1()
                    .h(px(72.))
                    .px(px(12.))
                    .py(px(10.))
                    .rounded(px(7.))
                    .border_1()
                    .border_color(p.border_subtle)
                    .bg(p.surface_raised)
                    .flex()
                    .flex_col()
                    .justify_between()
                    .child(overline(k, p.text_disabled))
                    .child(div().t_metric_sm().child(v))
            };
            // The only chart in the product: reclaimed over time is the one
            // thing here that is genuinely a time series.
            let recent: Vec<&CleanRun> = runs
                .iter()
                .take(24)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            let max = recent
                .iter()
                .map(|r| r.bytes_freed())
                .max()
                .unwrap_or(1)
                .max(1);
            let mut spark = div().h(px(64.)).flex().items_end().gap(px(3.));
            for (i, r) in recent.iter().enumerate() {
                let pct = ((r.bytes_freed() as f32 / max as f32) * 100.).max(2.);
                spark = spark.child(
                    div()
                        .id(("spark", i))
                        .flex_1()
                        .max_w(px(28.))
                        .h(gpui_kit::relative(pct / 100.))
                        .rounded_t(px(1.5))
                        .bg(p.accent_bg)
                        .opacity(if pct > 70. { 1. } else { 0.75 })
                        .tooltip_text(format_bytes(r.bytes_freed())),
                );
            }
            let mut list = div()
                .id("history-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll();
            for (i, r) in runs.iter().enumerate() {
                let ok = succeeded(r);
                let failed = r.items.len() - ok;
                let run_id = r.id;
                let hover = p.state_hover;
                list = list.child(
                    div()
                        .id(("hrow", i))
                        .h(px(44.))
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .px(px(PAD_X))
                        .border_b_1()
                        .border_color(p.border_subtle)
                        .hover(move |s| s.bg(hover))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.ui.open_run = Some(run_id);
                            cx.notify();
                        }))
                        .child(
                            div()
                                .w(px(150.))
                                .t_body()
                                .child(medium_date_time(r.started_at)),
                        )
                        .child(
                            div()
                                .w(px(60.))
                                .flex()
                                .justify_end()
                                .t_body_sm()
                                .text_color(p.text_secondary)
                                .child(count(r.items.len())),
                        )
                        .child(
                            div()
                                .w(px(90.))
                                .flex()
                                .justify_end()
                                .t_body()
                                .font_weight(FontWeight(560.))
                                .child(format!(
                                    "{}{}",
                                    format_bytes(r.bytes_freed()),
                                    if r.is_estimated() { "*" } else { "" }
                                )),
                        )
                        .child(
                            div()
                                .flex_1()
                                .pl(px(16.))
                                .flex()
                                .items_center()
                                .gap(px(4.))
                                .t_body_sm()
                                .text_color(p.text_tertiary)
                                .when(r.trigger.as_deref() == Some(GUARD_TRIGGER), |d| {
                                    d.child(
                                        badge(
                                            crate::model::actions::Tone::Neutral,
                                            "auto",
                                            None,
                                            &p,
                                        )
                                        .mr(px(2.)),
                                    )
                                })
                                .child(
                                    div()
                                        .text_color(p.safe_text)
                                        .child(format!("{} ok", count(ok))),
                                )
                                .when(failed > 0, |d| {
                                    d.child("·").child(
                                        div()
                                            .text_color(p.danger_text)
                                            .child(format!("{} failed", count(failed))),
                                    )
                                }),
                        )
                        .child(div().w(px(24.)).flex().justify_center().child(icon(
                            "chevron-right",
                            px(14.),
                            p.text_tertiary,
                        ))),
                );
            }
            col = col
                .child(
                    div()
                        .flex()
                        .gap(px(10.))
                        .px(px(PAD_X))
                        .pt(px(16.))
                        .child(tile("All time", format!("{tn} {tu}")))
                        .child(tile("Last 30 days", format!("{ln} {lu}")))
                        .child(tile("Runs", count(h.run_count))),
                )
                .child(
                    div()
                        .px(px(PAD_X))
                        .pt(px(20.))
                        .pb(px(14.))
                        .child(overline("Reclaimed per run", p.text_tertiary).mb(px(10.)))
                        .child(spark),
                )
                .child(
                    div()
                        .h(px(COLHEAD_H))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .px(px(PAD_X))
                        .border_b_1()
                        .border_t_1()
                        .border_color(p.border_subtle)
                        .t_caption()
                        .text_color(p.text_tertiary)
                        .child(div().w(px(150.)).child("Date"))
                        .child(div().w(px(60.)).flex().justify_end().child("Items"))
                        .child(div().w(px(90.)).flex().justify_end().child("Freed"))
                        .child(div().flex_1().pl(px(16.)).child("Result"))
                        .child(div().w(px(24.))),
                )
                .child(list);
        }
        let estimated = runs.iter().any(|r| r.is_estimated());
        col.child(statusbar_frame(&p).child(if estimated {
            "* includes estimates — commands cannot report what they removed"
        } else {
            "Kept locally, alongside your settings"
        }))
    }

    pub(crate) fn render_run_detail(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = self.p;
        let id = self.ui.open_run?;
        let run = self
            .history
            .as_ref()?
            .runs
            .iter()
            .find(|r| r.id == id)?
            .clone();
        let sub = format!(
            "{} freed · {} items · {}{}",
            format_bytes(run.bytes_freed()),
            count(run.items.len()),
            duration(run.duration_ms),
            if run.trigger.as_deref() == Some(GUARD_TRIGGER) {
                " · run automatically by Guard"
            } else {
                ""
            }
        );
        let mut body = div()
            .id("run-body")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(px(20.))
            .py(px(16.));
        for item in &run.items {
            let ok = item.result == RunOutcome::Succeeded;
            body = body.child(
                div()
                    .py(px(10.))
                    .border_b_1()
                    .border_color(p.border_subtle)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .mb(px(3.))
                            .child(icon(
                                if ok { "check" } else { "alert-circle" },
                                px(14.),
                                if ok { p.safe_text } else { p.danger_text },
                            ))
                            .child(
                                div()
                                    .flex_1()
                                    .t_body()
                                    .font_weight(FontWeight(540.))
                                    .child(item.action_label.clone()),
                            )
                            .child(div().text_size(px(12.)).text_color(p.text_secondary).child(
                                if ok {
                                    format_bytes(item.effective_bytes())
                                } else {
                                    "—".into()
                                },
                            )),
                    )
                    .child(
                        div()
                            .t_mono_xs()
                            .text_color(p.text_tertiary)
                            .child(item.path.display().to_string()),
                    )
                    .child(
                        div()
                            .mt(px(4.))
                            .child(cmdline(item.method_summary.clone(), &p)),
                    )
                    .when_some(
                        match &item.result {
                            RunOutcome::Failed { error } => Some(error.clone()),
                            RunOutcome::Succeeded => None,
                        },
                        |d, err| d.child(error_plate(err, &p).mt(px(6.))),
                    ),
            );
        }
        let (scrim, card) = modal_frame("run", 640., &p, |this, _| this.ui.open_run = None, cx);
        Some(
            scrim
                .child(
                    card.child(modal_head(
                        medium_date_time(run.started_at),
                        sub,
                        iconbtn("run-close", "close", 28., &p).on_click(cx.listener(
                            |this, _, _, cx| {
                                this.ui.open_run = None;
                                cx.notify();
                            },
                        )),
                        &p,
                    ))
                    .child(body)
                    .child(
                        modal_foot(&p).child(spacer()).child(
                            btn("run-done", BtnKind::Secondary, BtnSize::Md, false, &p)
                                .child("Close")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.ui.open_run = None;
                                    cx.notify();
                                })),
                        ),
                    ),
                )
                .into_any_element(),
        )
    }
}
