//! The Results screen.

use gpui_kit::component::input::Input;
use gpui_kit::component::Sizable;
use gpui_kit::{div, list, prelude::*, px, AnyElement, Context, FontWeight, Window};
use uuid::Uuid;

use deepclean_core::model::{CleanableItem, Ecosystem, RiskLevel};

use super::app_view::{AppView, Row};
use super::components::*;
use super::icons::{eco_icon, icon};
use super::theme::{metrics, COLHEAD_H, HEADER_H, PAD_X, STATUSBAR_H, SUMMARY_H, TOOLBAR_H};
use crate::model::actions::{is_actionable, worktree_info};
use crate::model::format::{
    count, duration, format_bytes, percent, plural, relative_time, split_bytes, stale_short,
};
use crate::model::labels::{agent_name, eco_name, kind_label, project_label};
use crate::model::ui_state::Route;
use crate::model::ui_state::{grouping_label, next_grouping, RowState, ScannerStatus};

impl AppView {
    pub(crate) fn render_results(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let p = self.p;
        let ui = &self.ui;
        let visible = ui.visible_items().len();
        let show_empty = !ui.has_scanned || visible == 0;

        let sub = if ui.scanning {
            format!(
                "Scanning · {} paths · {} found",
                count(ui.paths_scanned as usize),
                count(ui.items.len())
            )
        } else if ui.has_scanned {
            let when = ui
                .last_scan_at
                .map(|t| relative_time(t, chrono::Utc::now()))
                .unwrap_or_else(|| "just now".into());
            format!("{} items · scanned {when}", count(ui.items.len()))
        } else {
            "Ready to scan".into()
        };

        let mut col = div().size_full().flex().flex_col().min_h_0();
        col = col.child(self.cmdbar("Results", sub, true, cx));
        if ui.scanning {
            col = col.child(indeterminate(&p));
        }
        if ui.has_scanned || ui.scanning {
            let narrow = window.viewport_size().width < px(860.);
            col = col.child(self.summary_strip(narrow, cx));
        }
        if ui.has_scanned && !ui.denied_paths.is_empty() && !ui.scanning {
            col = col.child(self.banner(cx));
        }
        if ui.scanning && !ui.scanner_status.is_empty() {
            col = col.child(self.ticker());
        }
        // Kept while a filter matches nothing: the filter field lives here,
        // and hiding it would drop focus mid-word.
        if ui.has_scanned && !ui.items.is_empty() {
            col = col.child(self.toolbar(window, cx)).child(self.table_head());
        }
        let body = if show_empty {
            self.empty_state(cx).into_any_element()
        } else {
            let render_row =
                cx.processor(|this: &mut AppView, ix: usize, _window, cx| this.render_row(ix, cx));
            list(self.list.clone(), render_row)
                .size_full()
                .into_any_element()
        };
        col.child(
            div()
                .id("results-list")
                .flex_1()
                .min_h_0()
                .bg(p.surface_base)
                .child(body),
        )
        .child(self.statusbar())
    }

    pub(crate) fn cmdbar(
        &self,
        title: &str,
        sub: String,
        primary_scan: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let p = self.p;
        let scanning = self.ui.scanning;
        let kind = if primary_scan {
            BtnKind::Primary
        } else {
            BtnKind::Secondary
        };
        let scan_btn = btn("scan-now", kind, BtnSize::Md, scanning, &p)
            .map(|b| {
                if scanning {
                    let fg = if primary_scan {
                        p.accent_on_solid
                    } else {
                        p.text_primary
                    };
                    b.child(spinner("scan-spin", 12., fg)).child("Scanning…")
                } else {
                    let fg = if primary_scan {
                        p.accent_on_solid
                    } else {
                        p.text_primary
                    };
                    b.child(icon("scan", px(14.), fg)).child("Scan now")
                }
            })
            .on_click(cx.listener(|this, _, _, cx| this.start_scan(cx)));
        div()
            .h(px(HEADER_H))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(12.))
            .px(px(PAD_X))
            .border_b_1()
            .border_color(p.border_subtle)
            .bg(p.surface_base)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .t_section_title()
                            .text_size(px(15.))
                            .child(title.to_string()),
                    )
                    .child(
                        div()
                            .t_caption()
                            .mt(px(1.))
                            .text_color(p.text_tertiary)
                            .truncate()
                            .child(sub),
                    ),
            )
            .when(title != "Settings" && title != "History", |d| {
                d.child(scan_btn).when(title == "Results", |d| {
                    d.child(
                        iconbtn("overflow", "more-horizontal", 28., &p).on_click(cx.listener(
                            |this, _, _, cx| {
                                let k = mod_key();
                                this.show_toast(
                                    format!("Rescan with {k}R · settings with {k},"),
                                    None,
                                    cx,
                                );
                            },
                        )),
                    )
                })
            })
    }

    /// `narrow`: below the default window width the headline and the action
    /// leave the disk meter too little room, so it is dropped — the rail keeps
    /// showing disk usage.
    fn summary_strip(&self, narrow: bool, cx: &mut Context<Self>) -> AnyElement {
        if self.ui.clean.is_some() {
            return self.progress_strip(cx).into_any_element();
        }
        if self.ui.last_result.is_some() {
            return self.result_card(cx).into_any_element();
        }
        let p = self.p;
        let ui = &self.ui;
        let total = ui.total_bytes();
        let (n, u) = split_bytes(Some(total));
        let sel = ui.selection_summary();
        let safe = ui.safe_count();
        let disk = match &ui.disk {
            Some(disk) => {
                let t = disk.total_bytes.max(1) as f32;
                let recl = (total as f32 / t).min(1.);
                let used = (disk.used_bytes as f32 / t).min(1.);
                div()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .h(px(8.))
                            .w_full()
                            .flex()
                            .rounded(px(3.))
                            .overflow_hidden()
                            .bg(p.chart_track)
                            .child(
                                div()
                                    .h_full()
                                    .w(gpui_kit::relative((used - recl).max(0.)))
                                    .bg(p.text_disabled),
                            )
                            .child(div().h_full().w(gpui_kit::relative(recl)).bg(p.accent_bg)),
                    )
                    .child(
                        div()
                            .mt(px(6.))
                            .flex()
                            .gap(px(12.))
                            .t_caption()
                            .text_color(p.text_tertiary)
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .child(
                                div()
                                    .flex()
                                    .gap(px(4.))
                                    .child(
                                        div()
                                            .text_color(p.text_secondary)
                                            .font_weight(FontWeight(560.))
                                            .child(percent(total, disk.total_bytes)),
                                    )
                                    .child(format!(
                                        "of your {} disk",
                                        format_bytes(disk.total_bytes)
                                    )),
                            )
                            .child(format!("{} free", format_bytes(disk.available_bytes))),
                    )
            }
            None => div()
                .flex_1()
                .t_caption()
                .text_color(p.text_tertiary)
                .child("Disk capacity unavailable"),
        };

        let action = if sel.count > 0 {
            div()
                .flex()
                .flex_col()
                .items_end()
                .gap(px(4.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .t_caption()
                        .text_color(p.text_secondary)
                        .child(
                            div()
                                .text_color(p.text_primary)
                                .font_weight(FontWeight(600.))
                                .child(format!("{} selected", count(sel.count))),
                        )
                        .child(format!("· {}", format_bytes(sel.bytes)))
                        .when(sel.nested > 0, |d| {
                            d.child(
                                div()
                                    .text_color(p.text_tertiary)
                                    .child(format!("({} nested)", count(sel.nested))),
                            )
                        })
                        .when(sel.hidden > 0, |d| {
                            d.child(
                                chip_quiet("sel-hidden", &p)
                                    .child(format!("{} hidden", sel.hidden))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.ui.reset_filters();
                                        this.filter_input
                                            .update(cx, |s, cx| s.set_value("", window, cx));
                                        this.changed(cx);
                                    })),
                            )
                        }),
                )
                .child(
                    btn("review", BtnKind::Primary, BtnSize::Md, false, &p)
                        .child("Review & Clean")
                        .child(kbd(&format!("{}↵", mod_key()), &p))
                        .on_click(cx.listener(|this, _, window, cx| this.open_review(window, cx))),
                )
        } else {
            div().flex().justify_end().child(
                btn("select-safe", BtnKind::Ghost, BtnSize::Sm, false, &p)
                    .child(format!("Select all safe ({})", count(safe)))
                    .child(icon("arrow-right", px(14.), p.text_secondary))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.ui.select_safe();
                        this.changed(cx);
                    })),
            )
        };

        div()
            .h(px(SUMMARY_H))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(20.))
            .px(px(PAD_X))
            .border_b_1()
            .border_color(p.border_subtle)
            .bg(p.surface_base)
            .child(
                div()
                    .w(px(250.))
                    .flex_none()
                    .when(narrow, |d| d.flex_1())
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
                                    .child(format!("{u} reclaimable")),
                            ),
                    )
                    .child(
                        div()
                            .t_caption()
                            .mt(px(2.))
                            .text_color(p.text_tertiary)
                            .child(format!(
                                "{} · {}",
                                plural(ui.items.len(), "item", "items"),
                                plural(ui.ecosystems_present(), "ecosystem", "ecosystems")
                            )),
                    ),
            )
            .when(!narrow, |d| d.child(disk))
            .child(div().w(px(240.)).flex_none().child(action))
            .into_any_element()
    }

    fn progress_strip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.p;
        let c = self
            .ui
            .clean
            .as_ref()
            .expect("progress strip without a clean");
        let pct = c.percent();
        let shown = c.done() + usize::from(c.current.is_some() && c.done() < c.total);
        div()
            .h(px(78.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(20.))
            .px(px(PAD_X))
            .py(px(10.))
            .border_b_1()
            .border_color(p.border_subtle)
            .bg(p.surface_base)
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .gap(px(7.))
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .child(div().t_body_strong().child(format!(
                                "Cleaning {} of {}",
                                count(shown.min(c.total)),
                                count(c.total)
                            )))
                            .child(spacer())
                            .child(
                                div()
                                    .t_caption()
                                    .text_color(p.text_tertiary)
                                    .child(format!("{pct}%")),
                            ),
                    )
                    .child(bar(pct as f32 / 100., 8., p.accent_bg, &p))
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .child(
                                div()
                                    .t_caption()
                                    .text_color(p.text_tertiary)
                                    .truncate()
                                    .child(c.current.clone().unwrap_or_else(|| "Starting…".into())),
                            )
                            .child(spacer())
                            .child(
                                div()
                                    .t_caption()
                                    .text_color(p.text_secondary)
                                    .child(format!("{} freed so far", format_bytes(c.bytes))),
                            ),
                    ),
            )
            .child(
                btn("cancel-clean", BtnKind::Secondary, BtnSize::Md, false, &p)
                    .child(icon("stop", px(14.), p.text_primary))
                    .child("Stop")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.backend.cancel_clean();
                        this.show_toast("Stopping after the current item…".into(), None, cx);
                    })),
            )
    }

    fn result_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.p;
        let r = self
            .ui
            .last_result
            .as_ref()
            .expect("result card without a result");
        let (n, u) = split_bytes(Some(r.bytes_freed));
        let failed = r.failed > 0;
        let (bg, glyph_color) = if failed {
            (p.caution_bg, p.caution_text)
        } else {
            (p.safe_bg, p.safe_text)
        };
        let mut detail = format!("{} of {} cleaned", count(r.succeeded), count(r.total));
        let failed_text = failed.then(|| format!("{} failed", count(r.failed)));
        let tail = format!(
            " · {}{}",
            duration(r.duration_ms),
            if r.cancelled { " · stopped early" } else { "" }
        );
        if failed_text.is_none() {
            detail.push_str(&tail);
        }
        div()
            .h(px(96.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(14.))
            .px(px(PAD_X))
            .border_b_1()
            .border_color(p.border_subtle)
            .bg(bg)
            .child(icon(
                if failed { "warning" } else { "shield-check" },
                px(24.),
                glyph_color,
            ))
            .child(
                div()
                    .flex_1()
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap(px(8.))
                            .child(div().t_metric_sm().child(format!("Reclaimed {n} {u}")))
                            .when(r.estimated, |d| {
                                d.child(badge(
                                    crate::model::actions::Tone::Neutral,
                                    "estimated",
                                    None,
                                    &p,
                                ))
                            }),
                    )
                    .child(
                        div()
                            .mt(px(3.))
                            .flex()
                            .t_body_sm()
                            .text_color(p.text_secondary)
                            .child(detail)
                            .when_some(failed_text, |d, f| {
                                d.child(" · ")
                                    .child(div().text_color(p.danger_text).underline().child(f))
                                    .child(tail.clone())
                            }),
                    ),
            )
            .child(
                btn("view-report", BtnKind::Secondary, BtnSize::Md, false, &p)
                    .child("View report")
                    .on_click(cx.listener(|this, _, _, cx| this.navigate(Route::History, cx))),
            )
            .child(
                btn("scan-again", BtnKind::Ghost, BtnSize::Md, false, &p)
                    .child("Scan again")
                    .on_click(cx.listener(|this, _, _, cx| this.start_scan(cx))),
            )
            .child(
                iconbtn("dismiss-result", "close", 28., &p).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.ui.last_result = None;
                        cx.notify();
                    },
                )),
            )
    }

    fn banner(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.p;
        div()
            .h(px(44.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(10.))
            .px(px(PAD_X))
            .border_b_1()
            .border_color(p.border_subtle)
            .bg(p.caution_bg)
            .child(icon("lock", px(16.), p.caution_text))
            .child(
                div()
                    .flex_1()
                    .t_body()
                    .text_color(p.text_primary)
                    .child("Some folders couldn't be read, so what they hold is not listed here."),
            )
            .child(
                btn("see-issues", BtnKind::Secondary, BtnSize::Sm, false, &p)
                    .child("See which")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.ui.show_issues = true;
                        cx.notify();
                    })),
            )
            .child(
                iconbtn("dismiss-banner", "close", 24., &p).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.ui.denied_paths.clear();
                        cx.notify();
                    },
                )),
            )
    }

    fn ticker(&self) -> impl IntoElement {
        let p = self.p;
        let mut row = div()
            .h(px(28.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(14.))
            .px(px(PAD_X))
            .overflow_hidden()
            .border_b_1()
            .border_color(p.border_subtle)
            .bg(p.surface_sunken);
        for (i, (eco, status)) in self.ui.scanner_status.iter().enumerate() {
            let name = eco_name(*eco);
            let item = div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(5.))
                .whitespace_nowrap()
                .text_size(px(11.))
                .font_weight(FontWeight(500.));
            row = row.child(match status {
                ScannerStatus::Running => item
                    .text_color(p.accent_text)
                    .child(spinner(("tick", i), 12., p.accent_text))
                    .child(format!("{name}…")),
                ScannerStatus::Done(n) => item
                    .text_color(p.text_secondary)
                    .child(icon("check", px(12.), p.text_secondary))
                    .child(format!("{name} {n}")),
                ScannerStatus::Pending => {
                    item.text_color(p.text_disabled).child(format!("○ {name}"))
                }
            });
        }
        row
    }

    fn toolbar(&self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.p;
        let ui = &self.ui;
        let n = ui.active_filter_count();
        div()
            .h(px(TOOLBAR_H))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(PAD_X))
            .border_b_1()
            .border_color(p.border_subtle)
            .bg(p.surface_base)
            .child(
                div().flex_1().max_w(px(320.)).min_w(px(120.)).child(
                    Input::new(&self.filter_input)
                        // 12px text in a 24px box, as the original search field.
                        .with_size(gpui_kit::component::Size::Size(px(13.72)))
                        .prefix(icon("search", px(14.), p.text_tertiary))
                        .cleanable(true),
                ),
            )
            .child(
                chip("filters", n > 0, &p)
                    .child(icon(
                        "filter",
                        px(13.),
                        if n > 0 {
                            p.accent_text
                        } else {
                            p.text_secondary
                        },
                    ))
                    .child("Filters")
                    .when(n > 0, |d| d.child(countpill(n.to_string(), &p)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        // Cycles the one filter that matters most without a
                        // popover: large items.
                        this.ui.large_only = !this.ui.large_only;
                        this.changed(cx);
                    })),
            )
            .when_some(ui.preset.as_ref(), |d, preset| {
                let label = match preset.min_days {
                    Some(days) => format!("{} · {}+ days", preset.label, count(days as usize)),
                    None => preset.label.to_string(),
                };
                d.child(
                    chip("clear-preset", true, &p)
                        .child(label)
                        .child(icon("close", px(13.), p.accent_text))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.ui.preset = None;
                            this.list.scroll_to(Default::default());
                            this.changed(cx);
                        })),
                )
            })
            .child(div().w(px(1.)).h(px(16.)).mx(px(8.)).bg(p.border_subtle))
            .child(
                chip("group", false, &p)
                    .child(format!("Group: {}", grouping_label(ui.config.ui.grouping)))
                    .child(icon("chevron-down", px(13.), p.text_secondary))
                    .on_click(cx.listener(|this, _, _, cx| {
                        let g = &mut this.ui.config.ui.grouping;
                        *g = next_grouping(*g);
                        this.ui.collapsed.clear();
                        this.list.scroll_to(Default::default());
                        this.save_config(false, cx);
                        this.changed(cx);
                    })),
            )
            .child(
                chip("sort", false, &p)
                    .child(ui.sort.label())
                    .child(icon("chevron-down", px(13.), p.text_secondary))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.ui.sort = this.ui.sort.next();
                        this.list.scroll_to(Default::default());
                        this.changed(cx);
                    })),
            )
            .child(spacer())
            .child(
                iconbtn("density", "sidebar-toggle", 28., &p)
                    .tooltip_text(format!("Density ({}D)", mod_key()))
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_density(window, cx))),
            )
            .child(
                iconbtn("expand-all", "chevron-down", 28., &p)
                    .tooltip_text(format!("Expand or collapse all ({}⇧E)", mod_key()))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.ui.toggle_expand_all();
                        this.changed(cx);
                    })),
            )
    }

    fn table_head(&self) -> impl IntoElement {
        let p = self.p;
        div()
            .h(px(COLHEAD_H))
            .flex_none()
            .flex()
            .items_center()
            .px(px(PAD_X))
            .border_b_1()
            .border_color(p.border_subtle)
            .bg(p.surface_base)
            .t_caption()
            .text_color(p.text_tertiary)
            .child(div().w(px(22.)).flex_none())
            .child(div().w(px(12.)).flex_none())
            .child(div().flex_1().child("Name"))
            .child(
                div()
                    .w(px(56.))
                    .flex_none()
                    .pr(px(12.))
                    .flex()
                    .justify_end()
                    .child("Stale"),
            )
            .child(div().w(px(56.)).flex_none())
            .child(
                div()
                    .w(px(84.))
                    .flex_none()
                    .pr(px(8.))
                    .flex()
                    .justify_end()
                    .child("Size"),
            )
            .child(div().w(px(24.)).flex_none())
    }

    pub(crate) fn render_row(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        match self.rows.get(ix).cloned() {
            Some(Row::Group {
                key,
                label,
                eco,
                count: n,
                bytes,
                share,
                check,
                selectable,
            }) => self.group_header(ix, key, label, eco, n, bytes, share, check, selectable, cx),
            Some(Row::Item { id, group_max }) => {
                match self.row_index.get(&id).and_then(|&i| self.ui.items.get(i)) {
                    Some(item) => self.item_row(item.clone(), group_max, cx),
                    None => div().into_any_element(),
                }
            }
            None => div().into_any_element(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn group_header(
        &self,
        ix: usize,
        key: String,
        label: String,
        eco: Ecosystem,
        n: usize,
        bytes: u64,
        share: f32,
        check: Option<bool>,
        selectable: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = self.p;
        let collapsed = self.ui.collapsed.contains(&key);
        let k1 = key.clone();
        let k2 = key.clone();
        div()
            .id(("group", ix))
            .w_full()
            .h(px(36.))
            .flex()
            .items_center()
            .gap(px(10.))
            .px(px(PAD_X))
            .bg(p.surface_sunken)
            .border_t_1()
            .border_b_1()
            .border_color(p.border_subtle)
            .child(
                div()
                    .id(("group-check", ix))
                    .child(checkbox(check, !selectable, &p))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.ui.toggle_group(&k1);
                        this.changed(cx);
                    })),
            )
            .child(
                div()
                    .id(("group-toggle", ix))
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.ui.toggle_collapsed(&k2);
                        this.changed(cx);
                    }))
                    .child(icon(
                        if collapsed {
                            "chevron-right"
                        } else {
                            "chevron-down"
                        },
                        px(14.),
                        p.text_tertiary,
                    ))
                    .child(eco_icon(eco, px(14.), p.eco(eco)))
                    .child(
                        div()
                            .t_body_strong()
                            .text_color(p.text_primary)
                            .child(label),
                    )
                    .child(countpill(plural(n, "item", "items"), &p)),
            )
            .child(
                div()
                    .w(px(100.))
                    .flex_none()
                    .h(px(6.))
                    .rounded(px(3.))
                    .bg(p.chart_track)
                    .overflow_hidden()
                    .child(
                        div()
                            .h_full()
                            .rounded(px(3.))
                            .bg(p.eco(eco))
                            .w(gpui_kit::relative(share)),
                    ),
            )
            .child(div().w(px(84.)).flex_none().child(size_cell(bytes, &p)))
            .into_any_element()
    }

    fn item_row(&self, item: CleanableItem, group_max: u64, cx: &mut Context<Self>) -> AnyElement {
        let p = self.p;
        let ui = &self.ui;
        let m = metrics(ui.config.ui.density);
        let risk = ui.risk_of(&item);
        let action = ui.action_for(&item).cloned();
        let actions = item.available_actions.len();
        let selected = ui.selected.contains(&item.id);
        let focused = ui.focused == Some(item.id);
        let stale = ui.is_stale(&item);
        let ratio = if item.size_bytes > 0 && group_max > 0 {
            (item.size_bytes as f64 / group_max as f64).sqrt() as f32
        } else {
            0.02
        };
        let state = ui.row_state(item.id).cloned();
        let accent = p.eco(item.ecosystem);
        let id = item.id;

        let lead = match &state {
            Some(RowState::Active) => spinner(
                ("row-spin", id.as_u128() as u64 as usize),
                12.,
                p.accent_text,
            )
            .into_any_element(),
            Some(RowState::Queued) => div()
                .text_color(p.text_tertiary)
                .child("◦")
                .into_any_element(),
            Some(RowState::Failed { .. }) => {
                icon("alert-circle", px(14.), p.danger_text).into_any_element()
            }
            Some(RowState::Done) => icon("check", px(14.), p.safe_text).into_any_element(),
            None => checkbox(Some(selected), !is_actionable(&item), &p).into_any_element(),
        };

        let wt = (item.ecosystem == Ecosystem::Worktrees).then(|| worktree_info(&item));
        let mut line1 = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .min_w_0()
            .overflow_hidden()
            .child(
                div()
                    .flex_shrink(1.)
                    .min_w_0()
                    .truncate()
                    .text_size(m.kind_size)
                    .font_weight(FontWeight(560.))
                    .text_color(p.text_primary)
                    .child(kind_label(&item)),
            );
        if let Some(proj) = project_label(&item) {
            line1 = line1.child(
                div()
                    .flex_shrink(1.)
                    .min_w_0()
                    .truncate()
                    .t_body()
                    .text_color(p.text_tertiary)
                    .child(format!("· {proj}")),
            );
        }
        if let Some(wt) = &wt {
            if let Some(branch) = &wt.branch {
                line1 = line1.child(
                    div()
                        .flex()
                        .flex_shrink(1.)
                        .min_w_0()
                        .items_center()
                        .gap(px(3.))
                        .overflow_hidden()
                        .t_mono_xs()
                        .text_color(p.text_secondary)
                        .child(icon("git-branch", px(14.), p.text_tertiary))
                        .child(div().truncate().child(branch.clone())),
                );
            }
            for chip in &wt.chips {
                line1 = line1.child(badge(chip.tone, chip.label, None, &p));
            }
        }
        if let Some(agent) = item
            .agent
            .as_deref()
            .filter(|_| item.ecosystem != Ecosystem::Worktrees)
        {
            line1 = line1.child(agent_badge(&agent_name(agent), &p));
        }
        if let Some(b) = risk_badge(risk, &p) {
            line1 = line1.child(b);
        }

        let line2 = match &state {
            Some(RowState::Failed {
                error,
                can_fallback,
            }) => div()
                .flex()
                .items_center()
                .gap(px(8.))
                .min_w_0()
                .overflow_hidden()
                .child(
                    div()
                        .truncate()
                        .t_mono_xs()
                        .text_color(p.danger_text)
                        .child(error.clone()),
                )
                .when(*can_fallback, |d| {
                    d.child(
                        chip_quiet(("fallback", id.as_u128() as u64 as usize), &p)
                            .child("Use “Remove directory” instead")
                            .child(icon("arrow-right", px(12.), p.text_tertiary))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.fallback(id, window, cx)
                            })),
                    )
                }),
            _ => {
                let path = item.path.display().to_string();
                let tail = if actions > 1 {
                    chip_quiet(("actions", id.as_u128() as u64 as usize), &p)
                        .child(format!("{actions} actions"))
                        .child(icon("chevron-down", px(12.), p.text_tertiary))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.ui.drawer = Some(id);
                            cx.notify();
                        }))
                        .into_any_element()
                } else {
                    let text = match &action {
                        Some(a) => format!("via {}", a.label),
                        None => "informational".into(),
                    };
                    div()
                        .flex_shrink(1.)
                        .min_w_0()
                        .max_w(gpui_kit::relative(0.45))
                        .truncate()
                        .t_caption()
                        .text_color(p.text_tertiary)
                        .child(text)
                        .into_any_element()
                };
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .min_w_0()
                    .overflow_hidden()
                    .child(div().flex_1().min_w_0().overflow_hidden().child(path_line(
                        &path,
                        item.project_name.as_deref(),
                        &self.home,
                        &p,
                    )))
                    .child(tail)
            }
        };

        let (bg, edge) = match (&state, selected) {
            (Some(RowState::Failed { .. }), _) => (p.danger_bg, None),
            (Some(RowState::Active), _) => (p.state_hover, Some(p.accent_bg)),
            (_, true) => (p.state_selected, Some(p.state_selected_border)),
            _ => (p.surface_base, None),
        };
        let risk_edge = match risk {
            RiskLevel::Safe => gpui_kit::transparent_black(),
            RiskLevel::Caution => p.caution_solid,
            RiskLevel::Danger => p.danger_solid,
        };
        let hover = p.state_hover;
        let compact = m.row_h < px(40.);

        div()
            .id(("row", id.as_u128() as u64 as usize))
            .w_full()
            .relative()
            .h(m.row_h)
            .flex()
            .items_center()
            .px(px(PAD_X))
            .overflow_hidden()
            .bg(bg)
            .border_b_1()
            .border_color(p.border_subtle)
            .when(state.is_none() && !selected, |d| {
                d.hover(move |s| s.bg(hover))
            })
            .when(matches!(state, Some(RowState::Queued)), |d| d.opacity(0.55))
            .when(focused, |d| d.border_2().border_color(p.focus_ring))
            .when_some(edge, |d, c| {
                d.child(div().absolute().left_0().top_0().bottom_0().w(px(2.)).bg(c))
            })
            // Per-row magnitude, drawn under the row rather than in a column.
            .child(
                div()
                    .absolute()
                    .left_0()
                    .bottom_0()
                    .h(px(2.))
                    .w(gpui_kit::relative(ratio.min(1.)))
                    .bg(accent)
                    .opacity(0.22),
            )
            .child(
                div()
                    .id(("row-check", id.as_u128() as u64 as usize))
                    .w(px(22.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .child(lead)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.ui.toggle_select(id);
                        this.changed(cx);
                    })),
            )
            .child(
                div()
                    .w(px(4.))
                    .mr(px(8.))
                    .self_stretch()
                    .my(px(4.))
                    .rounded(px(1.))
                    .bg(risk_edge),
            )
            .child(
                div()
                    .id(("row-main", id.as_u128() as u64 as usize))
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .gap(px(if compact { 0. } else { 1. }))
                    .pr(px(12.))
                    .child(line1)
                    .child(line2)
                    .on_click(cx.listener(move |this, ev: &gpui_kit::ClickEvent, _, cx| {
                        this.ui.focused = Some(id);
                        if ev.click_count() >= 2 {
                            this.ui.drawer = Some(id);
                        }
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .w(px(56.))
                    .flex_none()
                    .pr(px(12.))
                    .flex()
                    .justify_end()
                    .t_caption()
                    .text_color(if stale {
                        p.caution_text
                    } else {
                        p.text_tertiary
                    })
                    .child(stale_short(item.days_stale)),
            )
            .child(
                div().w(px(56.)).flex_none().pr(px(12.)).child(
                    div()
                        .h(px(4.))
                        .rounded(px(2.))
                        .bg(p.chart_track)
                        .overflow_hidden()
                        .child(
                            div()
                                .h_full()
                                .rounded(px(2.))
                                .bg(accent)
                                .opacity(0.85)
                                .w(gpui_kit::relative(ratio.clamp(0.03, 1.))),
                        ),
                ),
            )
            .child(
                div()
                    .w(px(84.))
                    .flex_none()
                    .pr(px(8.))
                    .child(size_cell(item.size_bytes, &p)),
            )
            .child(
                div()
                    .id(("row-open", id.as_u128() as u64 as usize))
                    .w(px(24.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon("chevron-right", px(14.), p.text_disabled))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.ui.drawer = Some(id);
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    fn empty_state(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.p;
        let ui = &self.ui;
        if !ui.has_scanned {
            let roots = ui
                .config
                .scan_roots
                .iter()
                .map(|r| r.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            let roots = if roots.is_empty() {
                "~/".to_string()
            } else {
                roots
            };
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p(px(24.))
                .child(
                    div()
                        .w(px(440.))
                        .flex()
                        .flex_col()
                        .items_center()
                        .child(div().mb(px(24.)).child(icon("mark", px(64.), p.accent_bg)))
                        .child(div().t_page_title().child("Find what your builds left behind"))
                        .child(
                            div()
                                .mt(px(8.))
                                .max_w(px(380.))
                                .t_body()
                                .text_center()
                                .text_color(p.text_secondary)
                                .child(
                                    "Void walks your scan roots and reports every build artifact it finds, grouped by \
                                     ecosystem. Nothing is deleted until you pick it and confirm.",
                                ),
                        )
                        .child(
                            card(&p)
                                .w_full()
                                .mt(px(24.))
                                .flex()
                                .items_center()
                                .gap(px(12.))
                                .px(px(16.))
                                .py(px(14.))
                                .child(icon("folder", px(20.), p.text_secondary))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .child(div().t_label().text_color(p.text_tertiary).child("Scan roots"))
                                        .child(div().t_mono_sm().mt(px(2.)).truncate().child(roots))
                                        .child(div().t_caption().mt(px(2.)).text_color(p.text_tertiary).child(format!(
                                            "{} ecosystems enabled",
                                            ui.config.enabled_ecosystems.len()
                                        ))),
                                )
                                .child(
                                    btn("change-roots", BtnKind::Ghost, BtnSize::Sm, false, &p)
                                        .child("Change")
                                        .child(icon("chevron-right", px(14.), p.text_secondary))
                                        .on_click(cx.listener(|this, _, _, cx| this.navigate(Route::Settings, cx))),
                                ),
                        )
                        .child(
                            btn("first-scan", BtnKind::Primary, BtnSize::Lg, false, &p)
                                .w(px(240.))
                                .mt(px(16.))
                                .child("Start first scan")
                                .on_click(cx.listener(|this, _, _, cx| this.start_scan(cx))),
                        )
                        .child(div().mt(px(12.)).t_caption().text_color(p.text_tertiary).child("Typically takes 30–90 seconds")),
                )
                .into_any_element();
        }
        if ui.items.is_empty() {
            return state_block(
                &p,
                "shield-check",
                "Nothing to reclaim",
                &format!(
                    "Void scanned {} paths and found no build artifacts. On macOS this is more often a permissions \
                     problem than a clean machine.",
                    count(ui.paths_scanned as usize)
                ),
            )
            .child(
                div()
                    .mt(px(16.))
                    .flex()
                    .gap(px(8.))
                    .child(
                        btn("check-roots", BtnKind::Secondary, BtnSize::Md, false, &p)
                            .child("Check scan roots")
                            .on_click(cx.listener(|this, _, _, cx| this.navigate(Route::Settings, cx))),
                    )
                    .child(
                        btn("scan-again-empty", BtnKind::Ghost, BtnSize::Md, false, &p)
                            .child("Scan again")
                            .on_click(cx.listener(|this, _, _, cx| this.start_scan(cx))),
                    ),
            )
            .child(div().mt(px(14.)).t_caption().text_color(p.text_disabled).child(format!("{}R to rescan", mod_key())))
            .into_any_element();
        }
        state_block(
            &p,
            "filter",
            "No items match",
            &format!(
                "The scan found {} items, none matching the current filters.",
                count(ui.items.len())
            ),
        )
        .py(px(28.))
        .child(
            div().mt(px(16.)).child(
                btn("clear-filters", BtnKind::Ghost, BtnSize::Sm, false, &p)
                    .child("Clear filters")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.ui.reset_filters();
                        this.filter_input
                            .update(cx, |s, cx| s.set_value("", window, cx));
                        this.changed(cx);
                    })),
            ),
        )
        .into_any_element()
    }

    fn statusbar(&self) -> impl IntoElement {
        let p = self.p;
        let ui = &self.ui;
        let issues = ui.issue_count();
        let left = if ui.scanning {
            format!("Walking · {} paths", count(ui.paths_scanned as usize))
        } else if ui.has_scanned {
            format!(
                "{} paths · {}",
                count(ui.paths_scanned as usize),
                duration(ui.scan_duration_ms)
            )
        } else {
            "Idle".into()
        };
        statusbar_frame(&p)
            .child(left)
            .child(spacer())
            .when(issues > 0, |d| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(5.))
                        .text_color(p.caution_text)
                        .child(icon("warning", px(14.), p.caution_text))
                        .child(plural(issues, "issue", "issues")),
                )
            })
            .child(
                div()
                    .text_color(p.text_tertiary)
                    .child(match ui.config.ui.density {
                        deepclean_core::config::Density::Compact => "Compact",
                        deepclean_core::config::Density::Comfortable => "Comfortable",
                    }),
            )
    }
}

pub(crate) fn statusbar_frame(p: &super::theme::Palette) -> gpui_kit::Div {
    div()
        .h(px(STATUSBAR_H))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(12.))
        .px(px(PAD_X))
        .whitespace_nowrap()
        .overflow_hidden()
        .border_t_1()
        .border_color(p.border_subtle)
        .bg(p.surface_sunken)
        .t_caption()
        .text_color(p.text_tertiary)
}

/// The shared "empty / error" block: glyph, title, body.
pub(crate) fn state_block(
    p: &super::theme::Palette,
    glyph: &str,
    title: &str,
    body: &str,
) -> gpui_kit::Div {
    div()
        .flex()
        .flex_col()
        .items_center()
        .px(px(24.))
        .py(px(56.))
        .child(icon(glyph, px(24.), p.text_disabled))
        .child(
            div()
                .mt(px(12.))
                .text_size(px(14.))
                .line_height(px(20.))
                .font_weight(FontWeight(600.))
                .text_color(p.text_primary)
                .child(title.to_string()),
        )
        .child(
            div()
                .mt(px(4.))
                .max_w(px(420.))
                .text_center()
                .text_size(px(13.))
                .line_height(px(20.))
                .text_color(p.text_secondary)
                .child(body.to_string()),
        )
}

/// Stable per-item element ids for the row children.
#[allow(dead_code)]
pub(crate) fn row_key(id: Uuid) -> usize {
    id.as_u128() as u64 as usize
}
