//! Terminal-aware palette preparation for Mermaid and DOT images.

use image::Rgba;
use image::RgbaImage;
use ratatui::style::Color;
use std::path::Path;

use super::png_cache::CachedPng;

const FALLBACK_DARK_BACKGROUND: (u8, u8, u8) = (30, 30, 30);
const FALLBACK_LIGHT_BACKGROUND: (u8, u8, u8) = (245, 245, 245);
const FALLBACK_DARK_FOREGROUND: (u8, u8, u8) = (230, 230, 230);
const FALLBACK_LIGHT_FOREGROUND: (u8, u8, u8) = (32, 32, 32);
const FALLBACK_DARK_ACCENT: (u8, u8, u8) = (137, 180, 250);
const FALLBACK_LIGHT_ACCENT: (u8, u8, u8) = (0, 95, 135);
const COLOR_CHROMA_THRESHOLD: u8 = 32;
const BACKGROUND_DISTANCE: u8 = 2;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct DiagramPalette {
    pub(super) foreground: (u8, u8, u8),
    pub(super) background: (u8, u8, u8),
    pub(super) accent: (u8, u8, u8),
}

impl DiagramPalette {
    pub(super) fn detect() -> Self {
        let terminal_foreground = crate::terminal_palette::default_fg();
        let background = crate::terminal_palette::default_bg().unwrap_or_else(|| {
            if terminal_foreground.is_some_and(crate::color::is_light) {
                FALLBACK_DARK_BACKGROUND
            } else if terminal_foreground.is_some() {
                FALLBACK_LIGHT_BACKGROUND
            } else {
                FALLBACK_DARK_BACKGROUND
            }
        });
        let foreground = terminal_foreground.unwrap_or_else(|| {
            if crate::color::is_light(background) {
                FALLBACK_LIGHT_FOREGROUND
            } else {
                FALLBACK_DARK_FOREGROUND
            }
        });
        let accent = codex_accent_rgb().unwrap_or_else(|| {
            if crate::color::is_light(background) {
                FALLBACK_LIGHT_ACCENT
            } else {
                FALLBACK_DARK_ACCENT
            }
        });
        Self {
            foreground,
            background,
            accent,
        }
    }
}

pub(super) fn prepare(
    source: &Path,
    cache_dir: &Path,
    palette: DiagramPalette,
) -> Option<CachedPng> {
    let mut image = image::open(source).ok()?.into_rgba8();
    let source_background = bright_edge_background(&image);
    let surface_alpha = if crate::color::is_light(palette.background) {
        0.06
    } else {
        0.10
    };
    let accent_surface_alpha = if crate::color::is_light(palette.background) {
        0.12
    } else {
        0.18
    };
    let surface = crate::color::blend(palette.foreground, palette.background, surface_alpha);
    let accent_surface =
        crate::color::blend(palette.accent, palette.background, accent_surface_alpha);

    for pixel in image.pixels_mut() {
        let alpha = pixel.0[3];
        let source_rgb = (pixel.0[0], pixel.0[1], pixel.0[2]);
        if alpha == 0
            || source_background.is_some_and(|background| {
                color_distance(source_rgb, background) <= BACKGROUND_DISTANCE
            })
        {
            *pixel = Rgba([
                palette.background.0,
                palette.background.1,
                palette.background.2,
                255,
            ]);
            continue;
        }
        let mapped = map_color(source_rgb, palette, surface, accent_surface);
        let composite = crate::color::blend(mapped, palette.background, f32::from(alpha) / 255.0);
        *pixel = Rgba([composite.0, composite.1, composite.2, 255]);
    }

    super::png_cache::persist(image, cache_dir)
}

fn map_color(
    source: (u8, u8, u8),
    palette: DiagramPalette,
    surface: (u8, u8, u8),
    accent_surface: (u8, u8, u8),
) -> (u8, u8, u8) {
    let maximum = source.0.max(source.1).max(source.2);
    let minimum = source.0.min(source.1).min(source.2);
    let chroma = maximum - minimum;
    let luminance = relative_luminance(source);
    if chroma >= COLOR_CHROMA_THRESHOLD {
        let strength = ((0.82 - luminance) / 0.58).clamp(0.0, 1.0) as f32;
        crate::color::blend(palette.accent, accent_surface, strength)
    } else {
        let strength = ((0.95 - luminance) / 0.75).clamp(0.0, 1.0) as f32;
        crate::color::blend(palette.foreground, surface, strength)
    }
}

fn bright_edge_background(image: &RgbaImage) -> Option<(u8, u8, u8)> {
    if image.width() == 0 || image.height() == 0 {
        return None;
    }
    let corners = [
        image.get_pixel(0, 0),
        image.get_pixel(image.width() - 1, 0),
        image.get_pixel(0, image.height() - 1),
        image.get_pixel(image.width() - 1, image.height() - 1),
    ];
    corners.iter().find_map(|candidate| {
        let rgb = (candidate.0[0], candidate.0[1], candidate.0[2]);
        (candidate.0[3] > 0
            && relative_luminance(rgb) >= 0.9
            && corners
                .iter()
                .filter(|corner| {
                    corner.0[3] > 0
                        && color_distance(rgb, (corner.0[0], corner.0[1], corner.0[2]))
                            <= BACKGROUND_DISTANCE
                })
                .count()
                >= 2)
            .then_some(rgb)
    })
}

fn codex_accent_rgb() -> Option<(u8, u8, u8)> {
    let color = crate::render::highlight::foreground_style_for_scopes(&[
        "entity.name.type",
        "support.type",
        "variable",
    ])?
    .fg?;
    match color {
        Color::Rgb(red, green, blue) => Some((red, green, blue)),
        Color::Indexed(index) if index >= 16 => {
            Some(crate::terminal_palette::XTERM_COLORS[usize::from(index)])
        }
        _ => None,
    }
}

fn color_distance(left: (u8, u8, u8), right: (u8, u8, u8)) -> u8 {
    left.0
        .abs_diff(right.0)
        .max(left.1.abs_diff(right.1))
        .max(left.2.abs_diff(right.2))
}

fn relative_luminance((red, green, blue): (u8, u8, u8)) -> f64 {
    (0.2126 * f64::from(red) + 0.7152 * f64::from(green) + 0.0722 * f64::from(blue)) / 255.0
}

#[cfg(test)]
#[path = "diagram_tests.rs"]
mod tests;
