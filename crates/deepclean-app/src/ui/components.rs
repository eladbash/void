//! Small building blocks shared by the screens, matching `components.css`.

use std::time::Duration;

use gpui_kit::{
    div, percentage, prelude::*, px, svg, Animation, AnimationExt, Div, ElementId, FontWeight,
    Hsla, SharedString, Stateful, Transformation,
};

use super::icons::icon;
use super::theme::{mono_family, Palette, RADIUS_SM, RADIUS_XS};
use crate::model::actions::Tone;
use crate::model::format::{magnitude, path_segments, split_bytes, Magnitude, SegRole};
use deepclean_core::model::RiskLevel;

/// Type scale from `tokens.css`: size, line height, weight.
pub trait TypeExt: Styled + Sized {
    fn t_metric_sm(self) -> Self {
        self.text_size(px(20.))
            .line_height(px(24.))
            .font_weight(FontWeight(600.))
    }
    fn t_page_title(self) -> Self {
        self.text_size(px(18.))
            .line_height(px(24.))
            .font_weight(FontWeight(600.))
    }
    fn t_section_title(self) -> Self {
        self.text_size(px(13.))
            .line_height(px(16.))
            .font_weight(FontWeight(600.))
    }
    fn t_body(self) -> Self {
        self.text_size(px(13.))
            .line_height(px(18.))
            .font_weight(FontWeight(450.))
    }
    fn t_body_strong(self) -> Self {
        self.text_size(px(13.))
            .line_height(px(18.))
            .font_weight(FontWeight(560.))
    }
    fn t_body_sm(self) -> Self {
        self.text_size(px(12.))
            .line_height(px(16.))
            .font_weight(FontWeight(450.))
    }
    fn t_label(self) -> Self {
        self.text_size(px(12.))
            .line_height(px(16.))
            .font_weight(FontWeight(520.))
    }
    fn t_caption(self) -> Self {
        self.text_size(px(11.))
            .line_height(px(14.))
            .font_weight(FontWeight(450.))
    }
    fn t_overline(self) -> Self {
        self.text_size(px(10.))
            .line_height(px(12.))
            .font_weight(FontWeight(620.))
    }
    fn t_mono_sm(self) -> Self {
        self.font_family(mono_family())
            .text_size(px(12.))
            .line_height(px(17.))
    }
    fn t_mono_xs(self) -> Self {
        self.font_family(mono_family())
            .text_size(px(11.))
            .line_height(px(15.))
    }
}

impl<T: Styled> TypeExt for T {}

/// An uppercase overline label ("WHAT WILL HAPPEN").
pub fn overline(text: &str, color: Hsla) -> Div {
    div()
        .t_overline()
        .text_color(color)
        .child(text.to_uppercase())
}

pub fn spacer() -> Div {
    div().flex_1()
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BtnKind {
    Primary,
    Secondary,
    Ghost,
    Danger,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BtnSize {
    Sm,
    Md,
    Lg,
}

/// A button. The caller adds `.on_click`.
pub fn btn(
    id: impl Into<ElementId>,
    kind: BtnKind,
    size: BtnSize,
    disabled: bool,
    p: &Palette,
) -> Stateful<Div> {
    let (h, pad, fs) = match size {
        BtnSize::Sm => (24., 8., 12.),
        BtnSize::Md => (30., 11., 13.),
        BtnSize::Lg => (36., 14., 13.),
    };
    let base = div()
        .id(id.into())
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .justify_center()
        .gap(px(6.))
        .h(px(h))
        .px(px(pad))
        .rounded(px(RADIUS_SM))
        .border_1()
        .border_color(gpui_kit::transparent_black())
        .text_size(px(fs))
        .font_weight(FontWeight(540.))
        .whitespace_nowrap();
    let styled = match kind {
        BtnKind::Primary => {
            let hover = p.accent_bg_hover;
            base.bg(p.accent_bg)
                .text_color(p.accent_on_solid)
                .when(!disabled, |b| b.hover(move |s| s.bg(hover)))
        }
        BtnKind::Secondary => {
            let hover = p.state_hover;
            base.bg(p.surface_raised)
                .text_color(p.text_primary)
                .border_color(p.border_strong)
                .when(!disabled, |b| b.hover(move |s| s.bg(hover)))
        }
        BtnKind::Ghost => {
            let (hover, fg) = (p.state_hover, p.text_primary);
            base.text_color(p.text_secondary)
                .when(!disabled, |b| b.hover(move |s| s.bg(hover).text_color(fg)))
        }
        BtnKind::Danger => {
            let hover = p.danger_solid_hover;
            base.bg(p.danger_solid)
                .text_color(p.danger_on_solid)
                .when(!disabled, |b| b.hover(move |s| s.bg(hover)))
        }
    };
    styled.when(disabled, |b| b.opacity(0.45))
}

/// A square icon-only button.
pub fn iconbtn(id: impl Into<ElementId>, name: &str, size: f32, p: &Palette) -> Stateful<Div> {
    let (hover, fg) = (p.state_hover, p.text_primary);
    div()
        .id(id.into())
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(size))
        .rounded(px(RADIUS_SM))
        .text_color(p.text_tertiary)
        .hover(move |s| s.bg(hover).text_color(fg))
        .child(icon(
            name,
            px(if size < 24. { 14. } else { 16. }),
            p.text_tertiary,
        ))
}

/// A toolbar chip ("Filters", "Group: Ecosystem").
pub fn chip(id: impl Into<ElementId>, active: bool, p: &Palette) -> Stateful<Div> {
    let (hover_border, hover_fg) = (p.border_strong, p.text_primary);
    div()
        .id(id.into())
        .flex()
        .flex_none()
        .items_center()
        .gap(px(6.))
        .h(px(24.))
        .px(px(9.))
        .rounded(px(RADIUS_SM))
        .border_1()
        .text_size(px(12.))
        .font_weight(FontWeight(500.))
        .whitespace_nowrap()
        .map(|d| {
            if active {
                d.bg(p.accent_subtle)
                    .border_color(p.accent_border_strong)
                    .text_color(p.accent_text)
            } else {
                d.bg(p.surface_raised)
                    .border_color(p.border_default)
                    .text_color(p.text_secondary)
                    .hover(move |s| s.border_color(hover_border).text_color(hover_fg))
            }
        })
}

/// The small in-row chip ("2 actions ⌄", "Use Remove directory instead").
pub fn chip_quiet(id: impl Into<ElementId>, p: &Palette) -> Stateful<Div> {
    let fg = p.text_primary;
    div()
        .id(id.into())
        .flex()
        .flex_none()
        .items_center()
        .gap(px(4.))
        .h(px(18.))
        .px(px(6.))
        .rounded(px(RADIUS_SM))
        .border_1()
        .border_color(p.border_subtle)
        .text_color(p.text_tertiary)
        .text_size(px(11.))
        .whitespace_nowrap()
        .hover(move |s| s.text_color(fg))
}

pub fn tone_colors(tone: Tone, p: &Palette) -> (Hsla, Hsla, Hsla) {
    match tone {
        Tone::Safe => (p.safe_bg, p.safe_text, p.safe_border),
        Tone::Caution => (p.caution_bg, p.caution_text, p.caution_border),
        Tone::Danger => (p.danger_bg, p.danger_text, p.danger_border),
        Tone::Neutral => (p.surface_sunken, p.text_tertiary, p.border_subtle),
    }
}

pub fn risk_tone(risk: RiskLevel) -> Tone {
    match risk {
        RiskLevel::Safe => Tone::Safe,
        RiskLevel::Caution => Tone::Caution,
        RiskLevel::Danger => Tone::Danger,
    }
}

/// Risk is never carried by colour alone: each badge pairs a glyph, a shape
/// and the word. Safe is a full pill; caution and danger are squarer, and
/// danger adds a left rule.
pub fn badge(tone: Tone, text: &str, glyph: Option<&str>, p: &Palette) -> Div {
    let (bg, fg, border) = tone_colors(tone, p);
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(3.))
        .h(px(16.))
        .px(px(if tone == Tone::Safe { 7. } else { 5. }))
        .rounded(px(if tone == Tone::Safe { 999. } else { RADIUS_XS }))
        .bg(bg)
        .border_1()
        .border_color(border)
        .when(tone == Tone::Danger, |d| {
            d.border_l_2().border_color(p.danger_border_strong)
        })
        .text_color(fg)
        .text_size(px(10.))
        .font_weight(FontWeight(600.))
        .whitespace_nowrap()
        .when_some(glyph, |d, g| d.child(icon(g, px(10.), fg)))
        .child(text.to_uppercase())
}

pub fn risk_badge(risk: RiskLevel, p: &Palette) -> Option<Div> {
    match risk {
        RiskLevel::Caution => Some(badge(Tone::Caution, "caution", Some("warning"), p)),
        RiskLevel::Danger => Some(badge(Tone::Danger, "danger", Some("alert-circle"), p)),
        RiskLevel::Safe => None,
    }
}

/// An agent name badge: accent-tinted, not uppercased.
pub fn agent_badge(name: &str, p: &Palette) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(3.))
        .h(px(16.))
        .px(px(5.))
        .rounded(px(RADIUS_XS))
        .bg(p.accent_subtle)
        .border_1()
        .border_color(p.accent_border)
        .text_color(p.accent_text)
        .text_size(px(10.))
        .font_weight(FontWeight(600.))
        .whitespace_nowrap()
        .when(!name.is_empty(), |d| d.child(name.to_string()))
}

/// An agent badge led by a glyph, as in the drawer header.
pub fn agent_badge_with_icon(name: &str, glyph: &str, p: &Palette) -> Div {
    agent_badge("", p)
        .child(icon(glyph, px(10.), p.accent_text))
        .child(name.to_string())
}

pub fn countpill(text: impl Into<SharedString>, p: &Palette) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .h(px(16.))
        .min_w(px(16.))
        .px(px(5.))
        .rounded(px(RADIUS_XS))
        .bg(p.surface_sunken)
        .text_color(p.text_tertiary)
        .text_size(px(10.))
        .font_weight(FontWeight(520.))
        .child(text.into())
}

/// Tri-state checkbox: `Some(true)` checked, `None` mixed, `Some(false)` off.
pub fn checkbox(state: Option<bool>, disabled: bool, p: &Palette) -> Div {
    let on = state != Some(false);
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(14.))
        .rounded(px(RADIUS_XS))
        .border_1()
        .map(|d| {
            if on {
                d.bg(p.accent_bg).border_color(p.accent_bg)
            } else {
                d.bg(p.surface_raised).border_color(p.border_strong)
            }
        })
        .when(disabled, |d| d.opacity(0.35))
        .when(on, |d| {
            d.child(icon(
                if state.is_none() { "minus" } else { "check" },
                px(11.),
                p.accent_on_solid,
            ))
        })
}

pub fn radio(on: bool, p: &Palette) -> Div {
    div()
        .flex_none()
        .mt(px(2.))
        .size(px(14.))
        .rounded_full()
        .bg(p.surface_raised)
        .map(|d| {
            if on {
                d.border_4().border_color(p.accent_bg)
            } else {
                d.border_1().border_color(p.border_strong)
            }
        })
}

/// A switch. The caller wraps it in something clickable.
pub fn toggle(on: bool, p: &Palette) -> Div {
    div()
        .flex_none()
        .relative()
        .w(px(32.))
        .h(px(18.))
        .rounded_full()
        // Off: in light mode the chart track matches the inset list behind
        // it and the control vanished, so it gets a visible neutral.
        .bg(if on {
            p.accent_bg
        } else if p.dark {
            p.chart_track
        } else {
            gpui_kit::rgb(0xC2C7CE).into()
        })
        .child(
            div()
                .absolute()
                .top(px(2.))
                .left(px(if on { 16. } else { 2. }))
                .size(px(14.))
                .rounded_full()
                .bg(p.surface_raised)
                .shadow_sm(),
        )
}

/// Size cell: number and unit as two elements, weighted by magnitude.
pub fn size_cell(bytes: u64, p: &Palette) -> Div {
    let mag = magnitude(bytes);
    if mag == Magnitude::Unknown {
        return div()
            .flex()
            .justify_end()
            .italic()
            .text_size(px(12.))
            .line_height(px(18.))
            .text_color(p.text_disabled)
            .child("unknown");
    }
    let (n, u) = split_bytes(Some(bytes));
    let (size, weight, color) = match mag {
        Magnitude::Xl => (15., 620., p.text_primary),
        Magnitude::Lg => (14., 620., p.text_primary),
        Magnitude::Md => (13., 560., p.text_primary),
        _ => (13., 450., p.text_secondary),
    };
    div()
        .flex()
        .flex_row()
        .items_baseline()
        .justify_end()
        .gap(px(3.))
        .child(
            div()
                .text_size(px(size))
                .font_weight(FontWeight(weight))
                .text_color(color)
                .child(n),
        )
        .child(
            div()
                .min_w(px(16.))
                .text_size(px(10.))
                .font_weight(FontWeight(500.))
                .text_color(p.text_tertiary)
                .child(u),
        )
}

/// A path with four emphasis roles, so the meaningful segments read as names
/// and the separators recede. One line, clipped at the end.
pub fn path_line(path: &str, project: Option<&str>, home: &str, p: &Palette) -> Div {
    let mut row = div()
        .flex()
        .flex_row()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .t_mono_xs();
    for (role, seg) in path_segments(path, project, home) {
        let (color, weight) = match role {
            SegRole::Root | SegRole::Dim => (p.text_tertiary, 430.),
            SegRole::Sep => (p.text_disabled, 430.),
            SegRole::Project => (p.text_secondary, 560.),
            SegRole::Leaf => (p.text_secondary, 500.),
        };
        row = row.child(
            div()
                .flex_none()
                .text_color(color)
                .font_weight(FontWeight(weight))
                .child(seg),
        );
    }
    row
}

/// A monospace plate for commands and paths.
pub fn cmdline(text: impl Into<SharedString>, p: &Palette) -> Div {
    div()
        .px(px(6.))
        .py(px(4.))
        .rounded(px(RADIUS_XS))
        .border_1()
        .border_color(p.border_subtle)
        .bg(p.surface_inset)
        .text_color(p.text_code)
        .t_mono_xs()
        .child(text.into())
}

pub fn error_plate(text: impl Into<SharedString>, p: &Palette) -> Div {
    div()
        .px(px(10.))
        .py(px(8.))
        .rounded(px(RADIUS_SM))
        .border_1()
        .border_color(p.border_subtle)
        .bg(p.surface_inset)
        .text_color(p.text_secondary)
        .t_mono_xs()
        .child(text.into())
}

pub fn card(p: &Palette) -> Div {
    div()
        .rounded(px(7.))
        .border_1()
        .border_color(p.border_subtle)
        .bg(p.surface_raised)
        .p(px(12.))
}

/// A spinning ring, the loading indicator everywhere.
pub fn spinner(id: impl Into<ElementId>, size: f32, color: Hsla) -> impl IntoElement {
    svg()
        .path("icons/spin.svg")
        .size(px(size))
        .flex_none()
        .text_color(color)
        .with_animation(
            id.into(),
            Animation::new(Duration::from_millis(900)).repeat(),
            |svg, delta| svg.with_transformation(Transformation::rotate(percentage(delta))),
        )
}

/// A horizontal bar with a filled share, 0..=1.
pub fn bar(share: f32, h: f32, fill: Hsla, p: &Palette) -> Div {
    div()
        .h(px(h))
        .w_full()
        .rounded(px(h / 2.))
        .bg(p.chart_track)
        .overflow_hidden()
        .child(
            div()
                .h_full()
                .rounded(px(h / 2.))
                .bg(fill)
                .w(gpui_kit::relative(share.clamp(0., 1.))),
        )
}

/// The indeterminate scan progress line under the command bar.
pub fn indeterminate(p: &Palette) -> impl IntoElement {
    let fill = p.accent_bg;
    div()
        .h(px(2.))
        .w_full()
        .flex_none()
        .relative()
        .overflow_hidden()
        .bg(p.chart_track)
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .w(gpui_kit::relative(0.32))
                .bg(fill)
                .with_animation(
                    "indeterminate",
                    Animation::new(Duration::from_millis(1150)).repeat(),
                    |d, delta| d.left(gpui_kit::relative(-0.35 + delta * 1.35)),
                ),
        )
}

/// A plain-text tooltip on any clickable element.
pub trait TooltipExt: StatefulInteractiveElement + Sized {
    fn tooltip_text(self, text: impl Into<SharedString>) -> Self {
        let text = text.into();
        self.tooltip(move |window, cx| {
            gpui_kit::component::tooltip::Tooltip::new(text.clone()).build(window, cx)
        })
    }
}

impl<E: StatefulInteractiveElement> TooltipExt for E {}

/// A labelled keyboard hint ("⌘↵").
pub fn kbd(text: &str, p: &Palette) -> Div {
    div()
        .flex()
        .items_center()
        .h(px(16.))
        .px(px(4.))
        .ml(px(2.))
        .rounded(px(RADIUS_XS))
        .border_1()
        .border_color(p.border_subtle)
        .bg(p.surface_sunken)
        .text_color(p.text_tertiary)
        .text_size(px(10.))
        .child(text.to_string())
}

/// ⌘ on macOS, Ctrl elsewhere — for the hints shown next to controls.
pub fn mod_key() -> &'static str {
    if cfg!(target_os = "macos") {
        "⌘"
    } else {
        "Ctrl+"
    }
}
