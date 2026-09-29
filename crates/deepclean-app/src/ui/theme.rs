//! Design tokens, copied from the Tauri build's `tokens.css` so both builds
//! render the same colours, type scale and metrics.

use deepclean_core::config::{Density, Theme as ThemePref};
use deepclean_core::model::Ecosystem;
use gpui_kit::{px, rgb, rgba, Hsla, Pixels, WindowAppearance};

fn c(hex: u32) -> Hsla {
    rgb(hex).into()
}

fn ca(hex: u32) -> Hsla {
    rgba(hex).into()
}

/// Semantic colours for one theme.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub dark: bool,
    pub surface_base: Hsla,
    pub surface_raised: Hsla,
    pub surface_sunken: Hsla,
    pub surface_overlay: Hsla,
    pub surface_inset: Hsla,
    pub surface_scrim: Hsla,
    pub border_subtle: Hsla,
    pub border_default: Hsla,
    pub border_strong: Hsla,
    pub text_primary: Hsla,
    pub text_secondary: Hsla,
    pub text_tertiary: Hsla,
    pub text_disabled: Hsla,
    pub text_code: Hsla,
    pub accent_bg: Hsla,
    pub accent_bg_hover: Hsla,
    pub accent_subtle: Hsla,
    pub accent_border: Hsla,
    pub accent_border_strong: Hsla,
    pub accent_text: Hsla,
    pub accent_on_solid: Hsla,
    pub safe_text: Hsla,
    pub safe_bg: Hsla,
    pub safe_border: Hsla,
    pub caution_text: Hsla,
    pub caution_bg: Hsla,
    pub caution_border: Hsla,
    pub caution_border_strong: Hsla,
    pub caution_solid: Hsla,
    pub danger_text: Hsla,
    pub danger_bg: Hsla,
    pub danger_border: Hsla,
    pub danger_border_strong: Hsla,
    pub danger_solid: Hsla,
    pub danger_solid_hover: Hsla,
    pub danger_on_solid: Hsla,
    pub state_hover: Hsla,
    pub state_active: Hsla,
    pub state_selected: Hsla,
    pub state_selected_border: Hsla,
    pub focus_ring: Hsla,
    pub chart_track: Hsla,
    pub eco: [Hsla; 15],
    pub series: [Hsla; 9],
}

impl Palette {
    pub fn light() -> Self {
        Self {
            dark: false,
            surface_base: c(0xFAFBFD),
            surface_raised: c(0xFFFFFF),
            surface_sunken: c(0xF3F6F9),
            surface_overlay: c(0xFFFFFF),
            surface_inset: c(0xE5E8ED),
            surface_scrim: ca(0x1C232C73),
            border_subtle: c(0xE5E8ED),
            border_default: c(0xD7DCE1),
            border_strong: c(0x868D95),
            text_primary: c(0x1C232C),
            text_secondary: c(0x515963),
            text_tertiary: c(0x69717A),
            text_disabled: c(0xA5ABB3),
            text_code: c(0x363E48),
            accent_bg: c(0x0064B5),
            accent_bg_hover: c(0x004986),
            accent_subtle: c(0xEDF5FF),
            accent_border: c(0xBEDCFF),
            accent_border_strong: c(0x007FE2),
            accent_text: c(0x0064B5),
            accent_on_solid: c(0xFFFFFF),
            safe_text: c(0x007D61),
            safe_bg: c(0xE8FFF6),
            safe_border: c(0x77F8CF),
            caution_text: c(0x695000),
            caution_bg: c(0xFFF8E7),
            caution_border: c(0xFFD875),
            caution_border_strong: c(0xA68000),
            caution_solid: c(0x695000),
            danger_text: c(0x6D0020),
            danger_bg: c(0xFFF6F6),
            danger_border: c(0xFFCED1),
            danger_border_strong: c(0xE14660),
            danger_solid: c(0xBF2145),
            danger_solid_hover: c(0x9E0032),
            danger_on_solid: c(0xFFFFFF),
            state_hover: c(0xF3F3F3),
            state_active: c(0xE8E9EA),
            state_selected: c(0xEDF5FF),
            state_selected_border: c(0x007FE2),
            focus_ring: c(0x007FE2),
            chart_track: c(0xE5E8ED),
            eco: eco_colors(false),
            series: [
                c(0x2a78d6),
                c(0xeb6834),
                c(0x1baf7a),
                c(0xeda100),
                c(0xe87ba4),
                c(0x008300),
                c(0x4a3aa7),
                c(0xe34948),
                c(0xA5ABB3),
            ],
        }
    }

    pub fn dark() -> Self {
        Self {
            dark: true,
            surface_base: c(0x060B11),
            surface_raised: c(0x0F151D),
            surface_sunken: c(0x010306),
            surface_overlay: c(0x1C232C),
            surface_inset: c(0x010306),
            surface_scrim: ca(0x010306C7),
            border_subtle: c(0x1C232C),
            border_default: c(0x283039),
            border_strong: c(0x69717A),
            text_primary: c(0xF3F6F9),
            text_secondary: c(0xA5ABB3),
            text_tertiary: c(0x868D95),
            text_disabled: c(0x515963),
            text_code: c(0xD7DCE1),
            accent_bg: c(0x389BFF),
            accent_bg_hover: c(0x6BB2FF),
            accent_subtle: c(0x15283D),
            accent_border: c(0x1D436A),
            accent_border_strong: c(0x007FE2),
            accent_text: c(0x6BB2FF),
            accent_on_solid: c(0x010306),
            safe_text: c(0x6AECC4),
            safe_bg: c(0x182A2E),
            safe_border: c(0x2C5A52),
            caution_text: c(0xE9B600),
            caution_bg: c(0x25251A),
            caution_border: c(0x554914),
            caution_border_strong: c(0xB58C00),
            caution_solid: c(0xE9B600),
            danger_text: c(0xEF546C),
            danger_bg: c(0x251B25),
            danger_border: c(0x572936),
            danger_border_strong: c(0xEF546C),
            danger_solid: c(0xEF546C),
            danger_solid_hover: c(0xFF7887),
            danger_on_solid: c(0x010306),
            state_hover: c(0x1B2128),
            state_active: c(0x252A31),
            state_selected: c(0x15283D),
            state_selected_border: c(0x389BFF),
            focus_ring: c(0x6BB2FF),
            chart_track: c(0x1C232C),
            eco: eco_colors(true),
            series: [
                c(0x3987e5),
                c(0xd95926),
                c(0x199e70),
                c(0xc98500),
                c(0xd55181),
                c(0x008300),
                c(0x9085e9),
                c(0xe66767),
                c(0x515963),
            ],
        }
    }

    /// Resolve the user's preference against the window's appearance.
    pub fn resolve(pref: ThemePref, appearance: WindowAppearance) -> Self {
        let dark = match pref {
            ThemePref::Light => false,
            ThemePref::Dark => true,
            ThemePref::System => matches!(
                appearance,
                WindowAppearance::Dark | WindowAppearance::VibrantDark
            ),
        };
        if dark {
            Self::dark()
        } else {
            Self::light()
        }
    }

    pub fn eco(&self, e: Ecosystem) -> Hsla {
        self.eco[eco_index(e)]
    }

    /// Usage-chart series colour; `None` is the neutral "other" slot.
    pub fn series(&self, slot: Option<u8>) -> Hsla {
        match slot {
            Some(n @ 1..=8) => self.series[n as usize - 1],
            _ => self.series[8],
        }
    }
}

fn eco_index(e: Ecosystem) -> usize {
    Ecosystem::ALL.iter().position(|x| *x == e).unwrap_or(0)
}

/// In `Ecosystem::ALL` order.
fn eco_colors(dark: bool) -> [Hsla; 15] {
    let pick = |light: u32, dark_hex: u32| if dark { c(dark_hex) } else { c(light) };
    let mut out = [c(0); 15];
    for (i, e) in Ecosystem::ALL.iter().enumerate() {
        out[i] = match e {
            Ecosystem::Worktrees => pick(0x0B69C7, 0x4DA3FF),
            Ecosystem::AgentData => pick(0x5B45C9, 0x9C8CFF),
            Ecosystem::Models => pick(0x9A6B00, 0xD9A520),
            Ecosystem::Rust => pick(0xBB5F00, 0xFF8505),
            Ecosystem::Node => pick(0x097200, 0x3AA831),
            Ecosystem::Python => pick(0x004E85, 0x2174B7),
            Ecosystem::Apple => pick(0x6D74D8, 0xAAB4FF),
            Ecosystem::Docker => pick(0x007397, 0x009AC7),
            Ecosystem::Go => pick(0x0095A1, 0x31C7D5),
            Ecosystem::Java => pick(0x954D3F, 0xC37D6F),
            Ecosystem::DotNet => pick(0x712DC3, 0xA676F9),
            Ecosystem::Homebrew => pick(0x777B18, 0xA0A645),
            Ecosystem::JetBrains => pick(0x811288, 0xA34AAA),
            Ecosystem::System => pick(0xC63E86, 0xFF89C0),
            Ecosystem::Projects => pick(0x5E6873, 0x9AA4AF),
        };
    }
    out
}

/// Row metrics per density.
#[derive(Debug, Clone, Copy)]
pub struct Metrics {
    pub row_h: Pixels,
    pub kind_size: Pixels,
}

pub fn metrics(density: Density) -> Metrics {
    match density {
        Density::Comfortable => Metrics {
            row_h: px(44.),
            kind_size: px(13.),
        },
        Density::Compact => Metrics {
            row_h: px(32.),
            kind_size: px(12.),
        },
    }
}

pub const RAIL_W: f32 = 56.;
pub const HEADER_H: f32 = 52.;
pub const TOOLBAR_H: f32 = 40.;
pub const STATUSBAR_H: f32 = 28.;
pub const COLHEAD_H: f32 = 28.;
pub const SUMMARY_H: f32 = 64.;
pub const GROUP_H: f32 = 36.;
pub const DRAWER_W: f32 = 400.;
pub const PAD_X: f32 = 16.;

pub const RADIUS_XS: f32 = 3.;
pub const RADIUS_SM: f32 = 5.;
pub const RADIUS_MD: f32 = 7.;
pub const RADIUS_LG: f32 = 10.;

/// Monospace family for paths, commands and error text.
pub fn mono_family() -> &'static str {
    if cfg!(target_os = "macos") {
        "Menlo"
    } else if cfg!(target_os = "windows") {
        "Consolas"
    } else {
        "DejaVu Sans Mono"
    }
}

/// Point gpui-component's own theme (text fields, sliders) at our palette so
/// its widgets sit in the design instead of on top of it.
pub fn sync_component_theme(p: &Palette, cx: &mut gpui_kit::App) {
    use gpui_kit::component::theme::{Theme, ThemeMode};
    Theme::change(
        if p.dark {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
        },
        None,
        cx,
    );
    Theme::update(cx, |t| {
        let c = &mut t.colors;
        c.background = p.surface_sunken;
        c.foreground = p.text_primary;
        c.input = p.border_strong;
        c.border = p.border_default;
        c.muted_foreground = p.text_disabled;
        c.ring = p.accent_border_strong;
        c.caret = p.text_primary;
        c.selection = p.accent_subtle;
        c.primary = p.accent_bg;
        c.primary_foreground = p.accent_on_solid;
        c.accent = p.state_hover;
        c.popover = p.surface_overlay;
        c.slider_bar = p.accent_bg;
        c.slider_thumb = p.surface_raised;
        t.radius = px(RADIUS_SM);
    });
}
