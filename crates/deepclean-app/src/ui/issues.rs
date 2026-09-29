//! The scan issues sheet.

use gpui_kit::{div, prelude::*, px, AnyElement, Context};

use super::app_view::AppView;
use super::components::*;
use super::confirm::{modal_foot, modal_frame, modal_head};
use crate::model::format::count;

impl AppView {
    pub(crate) fn render_issues(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.p;
        let denied: Vec<String> = self.ui.denied_paths.iter().cloned().collect();
        let issues = self.ui.issues.clone();

        let mut body = div()
            .id("issues-body")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(px(20.))
            .py(px(16.));
        if !denied.is_empty() {
            body = body
                .child(overline(&format!("Could not be read ({})", count(denied.len())), p.text_tertiary).mb(px(8.)))
                .child(div().t_body_sm().text_color(p.text_secondary).mb(px(10.)).child(
                    "Whatever these hold is missing from your results. On macOS, granting Full Disk Access \
                     in System Settings → Privacy & Security usually fixes it.",
                ))
                .child(error_plate(denied.join("\n"), &p).mb(px(16.)));
        }
        if !issues.is_empty() {
            body = body.child(overline("Errors", p.text_tertiary).mb(px(8.)));
            for (msg, n) in &issues {
                body = body.child(
                    div()
                        .flex()
                        .items_start()
                        .gap(px(8.))
                        .mb(px(8.))
                        .child(
                            div()
                                .flex_1()
                                .t_mono_xs()
                                .text_color(p.text_secondary)
                                .child(msg.clone()),
                        )
                        .when(*n > 1, |d| {
                            d.child(countpill(format!("×{}", count(*n)), &p))
                        }),
                );
            }
        }
        if denied.is_empty() && issues.is_empty() {
            body = body.child(
                div()
                    .t_body_sm()
                    .text_color(p.text_tertiary)
                    .child("No issues recorded."),
            );
        }

        let (scrim, card) = modal_frame(
            "issues",
            520.,
            &p,
            |this, _| this.ui.show_issues = false,
            cx,
        );
        scrim
            .child(
                card.child(modal_head(
                    "Scan issues".into(),
                    "Problems Void hit while walking your disk.".into(),
                    iconbtn("issues-close", "close", 28., &p).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.ui.show_issues = false;
                            cx.notify();
                        },
                    )),
                    &p,
                ))
                .child(body)
                .child(
                    modal_foot(&p)
                        .child(
                            btn("copy-issues", BtnKind::Secondary, BtnSize::Sm, false, &p)
                                .child("Copy all")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let text = this
                                        .ui
                                        .denied_paths
                                        .iter()
                                        .cloned()
                                        .chain(this.ui.issues.iter().map(|(m, _)| m.clone()))
                                        .collect::<Vec<_>>()
                                        .join("\n");
                                    this.copy(text, "Copied", cx);
                                })),
                        )
                        .child(spacer())
                        .child(
                            btn("issues-done", BtnKind::Secondary, BtnSize::Md, false, &p)
                                .child("Close")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.ui.show_issues = false;
                                    cx.notify();
                                })),
                        ),
                ),
            )
            .into_any_element()
    }
}
