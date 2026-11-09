//! The WxLocated look: a light and a dark theme that follow the system
//! appearance, plus the few colours the component theme has no slot for
//! (code surfaces, raised cards, status tints).

use gpui_kit::component::{ActiveTheme as _, Theme, ThemeMode, ThemeRegistry};
use gpui_kit::*;

const THEMES: &str = include_str!("../assets/themes.json");

pub fn init(cx: &mut App) {
    ThemeRegistry::global_mut(cx).load_themes_from_str(THEMES).expect("bundled themes parse");
    let themes = ThemeRegistry::global(cx).themes().clone();
    if let (Some(light), Some(dark)) = (themes.get("WxLocated Light"), themes.get("WxLocated Dark")) {
        let theme = Theme::global_mut(cx);
        theme.light_theme = light.clone();
        theme.dark_theme = dark.clone();
    }
    apply(cx);
}

/// Applies the appearance setting: a fixed mode, or the system's.
pub fn apply(cx: &mut App) {
    let mode = match crate::library::Library::global(cx).read(cx).settings.appearance.as_str() {
        "light" => ThemeMode::Light,
        "dark" => ThemeMode::Dark,
        _ => mode_for(cx.window_appearance()),
    };
    Theme::change(mode, None, cx);
}

/// Keeps the theme in step with the system appearance for this window.
pub fn follow_appearance(window: &mut Window, _cx: &mut App) {
    window
        .observe_window_appearance(|window, cx| {
            if crate::library::Library::global(cx).read(cx).settings.appearance != "system" {
                return;
            }
            let mode = mode_for(window.appearance());
            if cx.theme().mode != mode {
                Theme::change(mode, None, cx);
            }
        })
        .detach();
}

fn mode_for(appearance: WindowAppearance) -> ThemeMode {
    match appearance {
        WindowAppearance::Dark | WindowAppearance::VibrantDark => ThemeMode::Dark,
        _ => ThemeMode::Light,
    }
}

/// Colours outside the component theme. The brand colour is the mint of the
/// mascot; `ink` is its outline green, used for large headings.
#[derive(Clone, Copy)]
pub struct Palette {
    pub accent: Hsla,
    /// Faint accent wash for selected rows and highlighted ranges.
    pub accent_soft: Hsla,
    /// Text on an accent fill.
    pub on_accent: Hsla,
    pub ink: Hsla,
    pub success: Hsla,
    pub warning: Hsla,
    pub error: Hsla,
    /// Cards and tiles that sit above the window background.
    pub raised: Hsla,
    /// Faint fill for grouped content and chips.
    pub fill: Hsla,
    pub fill_strong: Hsla,
    pub hairline: Hsla,
    pub code: Hsla,
    pub code_chrome: Hsla,
    /// Sidebars and the title bar: translucent over the blurred desktop.
    pub sidebar: Hsla,
    pub window: Hsla,
    pub text: Hsla,
    pub secondary: Hsla,
    pub tertiary: Hsla,
    pub shadow: Hsla,
    pub is_dark: bool,
}

pub fn palette(cx: &App) -> Palette {
    if cx.theme().is_dark() {
        Palette {
            accent: hex(0x3CCBA9),
            accent_soft: hexa(0x3CCBA9, 0.16),
            on_accent: hex(0x06221B),
            ink: hex(0xB9F0DC),
            success: hex(0x63D38A),
            warning: hex(0xE6B95F),
            error: hex(0xFF7D8C),
            raised: hex(0x1E2423),
            fill: hexa(0xFFFFFF, 0.045),
            fill_strong: hexa(0xFFFFFF, 0.08),
            hairline: hexa(0xFFFFFF, 0.08),
            code: hex(0x121615),
            code_chrome: hex(0x181D1C),
            sidebar: hexa(0x1A1F1E, 0.86),
            window: hex(0x151918),
            text: hex(0xE3E9E7),
            secondary: hex(0x97A39F),
            tertiary: hex(0x63706C),
            shadow: hexa(0x000000, 0.45),
            is_dark: true,
        }
    } else {
        Palette {
            accent: hex(0x0E957C),
            accent_soft: hexa(0x0E957C, 0.11),
            on_accent: hex(0xFFFFFF),
            ink: hex(0x254B43),
            success: hex(0x2E9D57),
            warning: hex(0xC27D0E),
            error: hex(0xD2465A),
            raised: hex(0xFFFFFF),
            fill: hexa(0x0F2E26, 0.04),
            fill_strong: hexa(0x0F2E26, 0.075),
            hairline: hexa(0x0F2E26, 0.09),
            code: hex(0xFBFCFC),
            code_chrome: hex(0xF3F6F5),
            sidebar: hexa(0xEDF2F0, 0.84),
            window: hex(0xF6F8F7),
            text: hex(0x1A201E),
            secondary: hex(0x5F6B67),
            tertiary: hex(0x98A39F),
            shadow: hexa(0x0F2E26, 0.11),
            is_dark: false,
        }
    }
}

/// Window options shared by every window: the title bar is ours (traffic
/// lights sit inside the toolbar) and the background behind translucent
/// sidebars is the blurred desktop.
pub fn window_options(title: &str, bounds: Bounds<Pixels>, min: Option<Size<Pixels>>, toolbar_height: f32) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: min,
        titlebar: Some(TitlebarOptions {
            title: Some(title.to_string().into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(18.), px((toolbar_height - 12.) / 2.))),
        }),
        app_owns_titlebar_drag: true,
        window_background: WindowBackgroundAppearance::Blurred,
        focus: std::env::var_os("WXLOCATED_NO_FOCUS").is_none(),
        ..Default::default()
    }
}

/// Native buttons start at x=18 and extend to about x=76; keep a 20-point gap.
pub const TRAFFIC_LIGHTS: f32 = 96.;

pub fn hex(value: u32) -> Hsla {
    rgb(value).into()
}

pub fn hexa(value: u32, alpha: f32) -> Hsla {
    let mut color: Hsla = rgb(value).into();
    color.a = alpha;
    color
}

/// Soft two-layer shadow for raised surfaces.
pub fn soft_shadow(palette: &Palette, lifted: bool) -> Vec<BoxShadow> {
    let base = palette.shadow;
    let (near, far) = if lifted { (0.9, 1.4) } else { (0.6, 0.8) };
    vec![
        BoxShadow { color: base.opacity(near * 0.5), offset: point(px(0.), px(1.)), blur_radius: px(2.), spread_radius: px(0.), inset: false },
        BoxShadow {
            color: base.opacity(far * 0.6),
            offset: point(px(0.), px(if lifted { 10. } else { 4. })),
            blur_radius: px(if lifted { 28. } else { 14. }),
            spread_radius: px(-4.),
            inset: false,
        },
    ]
}

/// A coloured glow under an app icon, tinted by the icon itself.
pub fn glow(color: Hsla, strength: f32, lifted: bool) -> Vec<BoxShadow> {
    vec![BoxShadow {
        color: color.opacity(strength),
        offset: point(px(0.), px(if lifted { 9. } else { 3. })),
        blur_radius: px(if lifted { 22. } else { 10. }),
        spread_radius: px(-3.),
        inset: false,
    }]
}
