//! The pre-clean confirmation modal and the scan issues sheet share this
//! frame; the run detail modal in History does too.

use gpui_kit::component::input::Input;
use gpui_kit::{div, prelude::*, px, AnyElement, Context, Div, FontWeight, Stateful, Window};

use deepclean_core::model::RiskLevel;

use super::app_view::AppView;
use super::components::*;
use super::icons::icon;
use super::theme::{Palette, RADIUS_LG};
use crate::model::actions::{describe_method, shows_command_line, MethodClass, Tone};
use crate::model::format::{count, format_bytes};

/// Scrim + centred card. `on_scrim` runs when the backdrop is clicked.
pub(crate) fn modal_frame(
    id: &'static str,
    width: f32,
    p: &Palette,
    on_scrim: impl Fn(&mut AppView, &mut Context<AppView>) + 'static,
    cx: &mut Context<AppView>,
) -> (Stateful<Div>, Stateful<Div>) {
    let scrim = div()
        .id(id)
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(p.surface_scrim)
        .on_click(cx.listener(move |this, _, _, cx| {
            on_scrim(this, cx);
            cx.notify();
        }));
    let card = div()
        .id(format!("{id}-card"))
        .w(px(width))
        .max_w(gpui_kit::relative(0.92))
        .max_h(gpui_kit::relative(0.92))
        .flex()
        .flex_col()
        .bg(p.surface_overlay)
        .border_1()
        .border_color(p.border_default)
        .rounded(px(RADIUS_LG))
        .shadow_lg()
        .overflow_hidden()
        // Clicks inside the card must not reach the scrim behind it.
        .on_click(|_, _, cx| cx.stop_propagation())
        .occlude();
    (scrim, card)
}

pub(crate) fn modal_head(title: String, sub: String, close: Stateful<Div>, p: &Palette) -> Div {
    div()
        .px(px(20.))
        .py(px(16.))
        .border_b_1()
        .border_color(p.border_subtle)
        .flex()
        .items_start()
        .gap(px(12.))
        .child(
            div()
                .flex_1()
                .child(div().t_page_title().child(title))
                .child(div().t_body_sm().text_color(p.text_tertiary).child(sub)),
        )
        .child(close)
}

pub(crate) fn modal_foot(p: &Palette) -> Div {
    div()
        .flex_none()
        .px(px(20.))
        .py(px(14.))
        .border_t_1()
        .border_color(p.border_subtle)
        .flex()
        .items_center()
        .gap(px(10.))
        .bg(p.surface_raised)
}

fn plural_word(n: usize, one: &str, many: &str) -> String {
    format!("{} {}", count(n), if n == 1 { one } else { many })
}

impl AppView {
    pub(crate) fn render_confirm(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = self.p;
        let plan = self.ui.build_plan();
        let n = plan.entries.len();
        let title = format!("Clean {}", plural_word(n, "item", "items"));

        let mut effects = vec![];
        for (class, k, bytes) in &plan.effects {
            let (ico, txt) = match class {
                MethodClass::Directories => (
                    "trash",
                    format!(
                        "{} deleted permanently",
                        plural_word(*k, "directory", "directories")
                    ),
                ),
                MethodClass::Files => (
                    "file",
                    format!("{} deleted permanently", plural_word(*k, "file", "files")),
                ),
                MethodClass::Commands => (
                    "terminal",
                    format!("{} run", plural_word(*k, "command", "commands")),
                ),
                MethodClass::Dedup => (
                    "copy",
                    format!(
                        "{} replaced with clones",
                        plural_word(*k, "duplicate set", "duplicate sets")
                    ),
                ),
                MethodClass::Trash => continue,
            };
            effects.push((ico, txt, *bytes));
        }
        if plan.trashed > 0 {
            effects.push((
                "refresh",
                format!(
                    "{} moved to the Trash",
                    plural_word(plan.trashed, "item", "items")
                ),
                plan.trashed_bytes,
            ));
        }

        let mut body = div()
            .id("confirm-body")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(px(20.))
            .py(px(16.))
            .child(overline("What will happen", p.text_tertiary).mb(px(6.)));
        for (ico, txt, bytes) in effects {
            body = body.child(
                div()
                    .h(px(32.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(icon(ico, px(16.), p.text_tertiary))
                    .child(div().flex_1().t_body().child(txt))
                    .child(
                        div()
                            .t_body_sm()
                            .text_color(p.text_secondary)
                            .child(format_bytes(bytes)),
                    ),
            );
        }
        if !plan.attention.is_empty() {
            let mut list = div()
                .id("attention")
                .rounded(px(5.))
                .border_1()
                .border_color(p.border_default)
                .bg(p.surface_inset)
                .max_h(px(200.))
                .overflow_y_scroll();
            for (item, action) in &plan.attention {
                let m = describe_method(&action.method);
                let tone = if action.risk == RiskLevel::Danger {
                    Tone::Danger
                } else {
                    Tone::Caution
                };
                let glyph = if action.risk == RiskLevel::Danger {
                    "alert-circle"
                } else {
                    "warning"
                };
                list = list.child(
                    div()
                        .px(px(12.))
                        .py(px(10.))
                        .border_b_1()
                        .border_color(p.border_subtle)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .mb(px(3.))
                                .child(badge(
                                    tone,
                                    crate::model::actions::risk_id(action.risk),
                                    Some(glyph),
                                    &p,
                                ))
                                .child(
                                    div()
                                        .flex_1()
                                        .t_body()
                                        .font_weight(FontWeight(540.))
                                        .child(action.label.clone()),
                                )
                                .child(
                                    div().text_size(px(12.)).text_color(p.text_secondary).child(
                                        if item.size_bytes > 0 {
                                            format_bytes(item.size_bytes)
                                        } else {
                                            "unknown".into()
                                        },
                                    ),
                                ),
                        )
                        .child(
                            div()
                                .t_mono_xs()
                                .text_color(p.text_tertiary)
                                .child(item.path.display().to_string()),
                        )
                        .child(if shows_command_line(&action.method) {
                            div()
                                .mt(px(4.))
                                .child(cmdline(format!("{} {}", m.glyph, m.text), &p))
                        } else {
                            div()
                                .mt(px(3.))
                                .t_body_sm()
                                .text_color(p.text_tertiary)
                                .child(action.description.clone())
                        }),
                );
            }
            body = body
                .child(
                    div()
                        .mt(px(20.))
                        .mb(px(8.))
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(icon("warning", px(14.), p.caution_text))
                        .child(overline("Needs your attention", p.caution_text)),
                )
                .child(list);
        }
        body = body.child(
            div()
                .mt(px(16.))
                .t_body_sm()
                .text_color(p.text_tertiary)
                .child(format!(
                    "Deletions and commands bypass the Trash and cannot be undone.{}",
                    if plan.trashed > 0 {
                        " Items moved to the Trash free their space only once the Trash is emptied."
                    } else {
                        ""
                    }
                )),
        );
        let has_danger = plan.has_danger;
        let estimated = plan.estimated;
        drop(plan);
        if has_danger {
            body = body.child(
                div()
                    .mt(px(14.))
                    .child(
                        div()
                            .flex()
                            .gap(px(4.))
                            .t_label()
                            .mb(px(6.))
                            .child("Type")
                            .child(div().t_mono_sm().text_color(p.danger_text).child("delete"))
                            .child("to confirm"),
                    )
                    .child(div().w(px(220.)).child(
                        Input::new(&self.confirm_input).font_family(super::theme::mono_family()),
                    )),
            );
        }
        let locked = has_danger && self.confirm_text != "delete";
        let prefer = self.ui.config.ui.prefer_trash;

        let (scrim, card) = modal_frame(
            "confirm",
            560.,
            &p,
            |this, _| this.ui.show_confirm = false,
            cx,
        );
        let card = card.child(
            div()
                .flex()
                .flex_col()
                .size_full()
                .child(modal_head(
                    title.clone(),
                    format!("{} estimated", format_bytes(estimated)),
                    iconbtn("confirm-close", "close", 28., &p).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.ui.show_confirm = false;
                            cx.notify();
                        },
                    )),
                    &p,
                ))
                .child(body)
                .child(
                    modal_foot(&p)
                        .child(
                            div()
                                .id("toggle-trash")
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .child(checkbox(Some(prefer), false, &p))
                                .child(
                                    div()
                                        .t_body_sm()
                                        .text_color(p.text_secondary)
                                        .child("Prefer Trash where available"),
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.ui.config.ui.prefer_trash =
                                        !this.ui.config.ui.prefer_trash;
                                    this.save_config(false, cx);
                                    this.changed(cx);
                                })),
                        )
                        .child(spacer())
                        .child(
                            btn("confirm-cancel", BtnKind::Secondary, BtnSize::Md, false, &p)
                                .child("Cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.ui.show_confirm = false;
                                    cx.notify();
                                })),
                        )
                        .child(
                            btn("confirm-clean", BtnKind::Danger, BtnSize::Md, locked, &p)
                                .child(title)
                                .when(!locked, |b| {
                                    b.on_click(
                                        cx.listener(|this, _, _, cx| this.clean_selection(cx)),
                                    )
                                }),
                        ),
                ),
        );
        scrim.child(card).into_any_element()
    }
}
