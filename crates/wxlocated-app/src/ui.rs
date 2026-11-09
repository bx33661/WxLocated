//! Small shared pieces: badges, chips, section labels, key/value rows, empty
//! states, the mascot, app icons, and formatting helpers.

use std::sync::{Arc, LazyLock};

use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::theme::{Palette, glow};

/// A shared, persistent save failure notice with an explicit retry action.
pub fn settings_notice(cx: &mut App) -> Option<AnyElement> {
    use gpui_kit::component::button::{Button, ButtonVariants as _};
    use gpui_kit::component::Sizable as _;
    let library = crate::library::Library::global(cx);
    let error = library.read(cx).settings_error.clone()?;
    let palette = crate::theme::palette(cx);
    Some(h_flex()
        .w_full().flex_none().gap_3().px_3().py_2()
        .bg(palette.fill).border_b_1().border_color(palette.hairline)
        .child(Button::new("retry-settings-save").label("重试保存").small().ghost()
            .on_click(move |_, _, cx| library.update(cx, |library, cx| library.save_settings(cx))))
        .child(v_flex().id("settings-save-error").role(Role::Status).aria_label(format!("设置尚未保存：{error}")).flex_1().min_w_0().gap_1()
            .child(div().text_size(px(12.)).text_color(palette.text).child("设置尚未保存，当前更改仍保留在此 App 中"))
            .child(div().text_size(px(11.)).text_color(palette.secondary).child(error)))
        .into_any_element())
}

const MASCOT_SVG: &[u8] = include_bytes!("../assets/mascot.svg");

static MASCOT: LazyLock<Arc<Image>> = LazyLock::new(|| Arc::new(Image::from_bytes(ImageFormat::Svg, MASCOT_SVG.to_vec())));

/// The mint cat with a magnifier.
pub fn mascot(size: f32) -> impl IntoElement {
    img(MASCOT.clone()).size(px(size)).flex_none()
}

/// Quiet capsule label: tinted text on a faint fill of the same colour.
pub fn badge(text: impl Into<SharedString>, color: Hsla) -> Div {
    div()
        .flex_none()
        .px(px(7.))
        .py(px(1.5))
        .rounded_full()
        .bg(color.opacity(0.13))
        .text_color(color)
        .text_size(px(10.5))
        .font_weight(FontWeight::MEDIUM)
        .child(text.into())
}

/// Value chip for paths, pages and package roots.
pub fn chip(text: impl Into<SharedString>, palette: &Palette, mono: bool) -> Div {
    div()
        .flex_none()
        .max_w(px(320.))
        .truncate()
        .px(px(7.))
        .py(px(2.5))
        .rounded(px(6.))
        .bg(palette.fill)
        .border_1()
        .border_color(palette.hairline)
        .text_size(px(11.))
        .text_color(palette.text)
        .when(mono, |this| this.font_family(mono_font()))
        .child(text.into())
}

/// Small-caps style section heading with an optional trailing note.
pub fn section_label(title: impl Into<SharedString>, trailing: Option<String>, palette: &Palette) -> Div {
    h_flex()
        .w_full()
        .items_baseline()
        .gap_2()
        .child(div().text_size(px(11.5)).font_weight(FontWeight::SEMIBOLD).text_color(palette.secondary).child(title.into()))
        .child(div().flex_1())
        .when_some(trailing, |this, text| this.child(div().text_size(px(11.)).text_color(palette.tertiary).child(text)))
}

/// Two-column key/value row used in inspectors.
pub fn info_row(label: impl Into<SharedString>, value: impl Into<SharedString>, palette: &Palette, mono: bool) -> Div {
    h_flex()
        .w_full()
        .items_start()
        .gap_2p5()
        .text_size(px(12.))
        .child(div().w(px(60.)).flex_none().text_color(palette.secondary).child(label.into()))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_color(palette.text)
                .when(mono, |this| this.font_family(mono_font()).text_size(px(11.)))
                .child(value.into()),
        )
}

/// Rounded group background for cards in content areas.
pub fn card(palette: &Palette) -> Div {
    v_flex().w_full().rounded(px(12.)).bg(palette.fill).border_1().border_color(palette.hairline)
}

/// Centered empty / unavailable state.
pub fn empty_state(
    icon: Option<IconName>,
    title: impl Into<SharedString>,
    message: impl Into<SharedString>,
    palette: &Palette,
) -> Div {
    v_flex()
        .flex_1()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .p_8()
        .child(match icon {
            Some(icon) => div().mb_1().text_size(px(34.)).text_color(palette.tertiary).child(icon).into_any_element(),
            None => div().mb_1().child(mascot(84.)).into_any_element(),
        })
        .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).text_color(palette.text).child(title.into()))
        .child(div().max_w(px(400.)).text_size(px(12.5)).text_center().text_color(palette.secondary).child(message.into()))
}

/// Status dot.
pub fn dot(color: Hsla, size: f32) -> Div {
    div().flex_none().size(px(size)).rounded_full().bg(color)
}

/// The app's icon (with a soft glow in its own colour), or a tinted square
/// with its first character.
pub fn app_icon(icon: Option<Arc<Image>>, ambient: Option<Hsla>, name: &str, size: f32, palette: &Palette, lifted: bool) -> AnyElement {
    let radius = px(size * 0.225);
    let frame = div().flex_none().size(px(size)).rounded(radius);
    match icon {
        Some(image) => {
            // Logos are often wide or transparent: sit them on a white tile.
            let shadow = glow(ambient.unwrap_or(palette.shadow), if lifted { 0.5 } else { 0.22 }, lifted);
            frame
                .shadow(shadow)
                .bg(white())
                .border_1()
                .border_color(palette.hairline)
                .overflow_hidden()
                .flex()
                .items_center()
                .justify_center()
                .child(img(image).size_full().object_fit(ObjectFit::Contain))
                .into_any_element()
        }
        None if name.is_empty() || name.starts_with("未识别") => frame
            .bg(linear_gradient(
                160.,
                linear_color_stop(palette.fill_strong.opacity(1.6), 0.),
                linear_color_stop(palette.fill_strong, 1.),
            ))
            .border_1()
            .border_color(palette.hairline)
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(size * 0.42))
            .text_color(palette.tertiary)
            .child(IconName::Package)
            .into_any_element(),
        None => {
            let initial: String = name.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default();
            // A stable hue per name, so letter icons are told apart at a glance.
            let hash = name.bytes().fold(5381u32, |h, b| h.wrapping_mul(33) ^ b as u32);
            let hues = [0.47, 0.53, 0.58, 0.64, 0.72, 0.80, 0.92, 0.07, 0.11];
            let hue = hues[(hash % hues.len() as u32) as usize];
            let (top, bottom) = if palette.is_dark {
                (hsla(hue, 0.55, 0.42, 1.), hsla(hue, 0.60, 0.28, 1.))
            } else {
                (hsla(hue, 0.70, 0.66, 1.), hsla(hue, 0.62, 0.48, 1.))
            };
            let shadow = glow(bottom, if lifted { 0.45 } else { 0.18 }, lifted);
            frame
                .relative()
                .overflow_hidden()
                .shadow(shadow)
                .bg(linear_gradient(150., linear_color_stop(top, 0.), linear_color_stop(bottom, 1.)))
                // Glossy top half, like a lit surface.
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .h(px(size * 0.5))
                        .bg(linear_gradient(180., linear_color_stop(white().opacity(0.22), 0.), linear_color_stop(white().opacity(0.), 1.))),
                )
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(size * 0.44))
                        .font_weight(FontWeight::BOLD)
                        .text_color(white())
                        .child(initial),
                )
                .into_any_element()
        }
    }
}

pub fn mono_font() -> &'static str {
    if cfg!(target_os = "macos") { "Menlo" } else { "Consolas" }
}

pub fn format_size(bytes: i64) -> String {
    let bytes = bytes as f64;
    if bytes >= 1024.0 * 1024.0 {
        format!("{:.1} MB", bytes / 1024.0 / 1024.0)
    } else if bytes >= 1024.0 {
        format!("{:.0} KB", bytes / 1024.0)
    } else {
        format!("{bytes:.0} B")
    }
}

/// `今天 14:05`, `昨天 09:12`, or `11月3日 18:40`, in local time.
pub fn format_time(time: std::time::SystemTime) -> String {
    use chrono::{Datelike as _, Local};
    let time: chrono::DateTime<Local> = time.into();
    let today = Local::now().date_naive();
    match (today - time.date_naive()).num_days() {
        0 => time.format("今天 %H:%M").to_string(),
        1 => time.format("昨天 %H:%M").to_string(),
        _ => format!("{}月{}日 {}", time.month(), time.day(), time.format("%H:%M")),
    }
}

/// The average colour of an icon's opaque pixels, slightly saturated, for
/// its glow.
pub fn ambient_color(bytes: &[u8]) -> Option<Hsla> {
    let image = image::load_from_memory(bytes).ok()?.thumbnail(24, 24).to_rgba8();
    let (mut r, mut g, mut b, mut n) = (0u64, 0u64, 0u64, 0u64);
    for pixel in image.pixels() {
        if pixel[3] > 200 {
            r += pixel[0] as u64;
            g += pixel[1] as u64;
            b += pixel[2] as u64;
            n += 1;
        }
    }
    if n == 0 {
        return None;
    }
    let color: Hsla = rgb(((r / n) << 16 | (g / n) << 8 | (b / n)) as u32).into();
    Some(hsla(color.h, (color.s * 1.25).min(1.0), color.l.clamp(0.35, 0.6), 1.0))
}

/// Lucide icon for a file path.
pub fn file_icon(path: &str) -> IconName {
    match extension(path).as_str() {
        "js" | "mjs" | "cjs" | "ts" => IconName::FileCode,
        "wxs" => IconName::FileCode,
        "json" | "map" => IconName::Braces,
        "wxml" | "html" | "htm" | "xml" => IconName::Code,
        "wxss" | "css" | "scss" | "less" => IconName::Palette,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" => IconName::Image,
        "md" | "txt" => IconName::FileText,
        _ => IconName::File,
    }
}

/// A colour per file type, used for tree and crumb icons.
pub fn file_tint(path: &str, palette: &Palette) -> Hsla {
    let tint = |light: u32, dark: u32| crate::theme::hex(if palette.is_dark { dark } else { light });
    match extension(path).as_str() {
        "js" | "mjs" | "cjs" | "ts" | "wxs" => tint(0xB58A00, 0xE3C25B),
        "json" | "map" => tint(0x7A5AF8, 0xA592FF),
        "wxml" | "html" | "htm" | "xml" => tint(0xD9622B, 0xF09566),
        "wxss" | "css" | "scss" | "less" => tint(0x2F80ED, 0x6FA8F5),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" => tint(0x1F9D6E, 0x5BD1A2),
        _ => palette.secondary,
    }
}

pub fn extension(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default()
}

pub fn language_for(path: &str) -> &'static str {
    match extension(path).as_str() {
        "js" | "mjs" | "cjs" | "wxs" | "ts" => "javascript",
        "json" | "map" => "json",
        "wxss" | "css" | "scss" | "less" => "css",
        "wxml" | "html" | "htm" | "xml" | "svg" => "html",
        "md" => "markdown",
        "diff" | "patch" => "diff",
        _ => "text",
    }
}

/// `~` for the home folder; a long path keeps its end, where folders differ.
pub fn display_path(path: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let path = match path.strip_prefix(&home) {
        Some(rest) if !home.is_empty() => format!("~{rest}"),
        _ => path.to_string(),
    };
    let characters: Vec<char> = path.chars().collect();
    if characters.len() <= 72 {
        return path;
    }
    format!("…{}", characters[characters.len() - 71..].iter().collect::<String>())
}

/// Writes `text` to the clipboard.
pub fn copy(text: impl Into<String>, cx: &mut App) {
    cx.write_to_clipboard(ClipboardItem::new_string(text.into()));
}

/// A window's toolbar, which is also its title bar: draggable, double-click
/// zooms, and the traffic lights sit at its left.
pub fn toolbar(height: f32, palette: &Palette) -> gpui_kit::component::TitleBar {
    gpui_kit::component::TitleBar::new()
        .h(px(height))
        .pl(px(crate::theme::TRAFFIC_LIGHTS))
        .pr(px(12.))
        .bg(palette.sidebar)
        .border_b_1()
        .border_color(palette.hairline)
}

#[cfg(test)]
mod tests {
    use super::{display_path, extension, format_size, language_for};

    #[test]
    fn sizes() {
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(2048), "2 KB");
        assert_eq!(format_size(3 * 1024 * 1024 + 300_000), "3.3 MB");
    }

    #[test]
    fn extensions_and_languages() {
        assert_eq!(extension("pages/a/index.WXML"), "wxml");
        assert_eq!(extension("dir.v2/README"), "");
        assert_eq!(language_for("a/b.wxs"), "javascript");
        assert_eq!(language_for("app.wxss"), "css");
        assert_eq!(language_for("x.json"), "json");
        assert_eq!(language_for("LICENSE"), "text");
    }

    #[test]
    fn long_paths_keep_their_end() {
        let path = format!("/opt/{}/applet/packages/wx0123456789abcdef", "x".repeat(80));
        let shown = display_path(&path);
        assert!(shown.starts_with('…'));
        assert!(shown.ends_with("wx0123456789abcdef"));
        assert_eq!(shown.chars().count(), 72);
    }
}
