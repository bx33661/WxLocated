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
            accent: hex(0x2DD4BF),
            accent_soft: hexa(0x2DD4BF, 0.13),
            on_accent: hex(0x042F2E),
            ink: hex(0xF4F4F6),
            success: hex(0x4ADE80),
            warning: hex(0xFBBF24),
            error: hex(0xFB7185),
            raised: hex(0x1E1E24),
            fill: hexa(0xFFFFFF, 0.05),
            fill_strong: hexa(0xFFFFFF, 0.085),
            hairline: hexa(0xFFFFFF, 0.08),
            code: hex(0x111114),
            code_chrome: hex(0x17171C),
            sidebar: hexa(0x101013, 0.94),
            window: hex(0x141417),
            text: hex(0xF4F4F6),
            secondary: hex(0x9E9EA8),
            tertiary: hex(0x62626D),
            shadow: hexa(0x000000, 0.55),
            is_dark: true,
        }
    } else {
        Palette {
            accent: hex(0x0F766E),
            accent_soft: hexa(0x0F766E, 0.10),
            on_accent: hex(0xFFFFFF),
            ink: hex(0x18181B),
            success: hex(0x16A34A),
            warning: hex(0xD97706),
            error: hex(0xE11D48),
            raised: hex(0xFFFFFF),
            fill: hexa(0x18181B, 0.045),
            fill_strong: hexa(0x18181B, 0.08),
            hairline: hexa(0x18181B, 0.085),
            code: hex(0xFCFCFD),
            code_chrome: hex(0xF4F5F7),
            sidebar: hexa(0xF4F5F7, 0.94),
            window: hex(0xFFFFFF),
            text: hex(0x18181B),
            secondary: hex(0x52525B),
            tertiary: hex(0x9CA3AF),
            shadow: hexa(0x09090B, 0.10),
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
            traffic_light_position: cfg!(target_os = "macos").then(|| point(px(18.), px((toolbar_height - 12.) / 2.))),
        }),
        app_owns_titlebar_drag: true,
        window_background: WindowBackgroundAppearance::Blurred,
        focus: std::env::var_os("WXLOCATED_NO_FOCUS").is_none(),
        ..Default::default()
    }
}

/// Native buttons start at x=18 and extend to about x=76; keep a 20-point gap.
pub const TRAFFIC_LIGHTS: f32 = if cfg!(target_os = "macos") { 96. } else { 12. };

pub fn hex(value: u32) -> Hsla {
    rgb(value).into()
}

pub fn hexa(value: u32, alpha: f32) -> Hsla {
    let mut color: Hsla = rgb(value).into();
    color.a = alpha;
    color
}

/// Soft shadow for raised surfaces (crisper and restrained for Obsidian Studio).
pub fn soft_shadow(palette: &Palette, lifted: bool) -> Vec<BoxShadow> {
    let base = palette.shadow;
    let (near, far) = if lifted { (0.65, 1.0) } else { (0.4, 0.55) };
    vec![
        BoxShadow { color: base.opacity(near * 0.45), offset: point(px(0.), px(1.)), blur_radius: px(2.), spread_radius: px(0.), inset: false },
        BoxShadow {
            color: base.opacity(far * 0.5),
            offset: point(px(0.), px(if lifted { 8. } else { 3. })),
            blur_radius: px(if lifted { 20. } else { 10. }),
            spread_radius: px(-3.),
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
