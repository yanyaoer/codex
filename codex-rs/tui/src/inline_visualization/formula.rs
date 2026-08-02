//! Terminal-aware preparation for transparent LaTeX images.

use image::GrayImage;
use image::Luma;
use image::Rgba;
use image::RgbaImage;
use image::imageops::FilterType;
use std::path::Path;
use std::path::PathBuf;

const MIN_FORMULA_ROWS: u16 = 3;
const MAX_FORMULA_ROWS: u16 = 6;
const MAX_FORMULA_COLUMNS: u16 = 100;
const FALLBACK_CELL_WIDTH_PX: u16 = 8;
const FALLBACK_CELL_HEIGHT_PX: u16 = 16;
const FALLBACK_DARK_FOREGROUND: (u8, u8, u8) = (230, 230, 230);
const FALLBACK_LIGHT_FOREGROUND: (u8, u8, u8) = (32, 32, 32);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct FormulaStyle {
    pub(super) foreground: (u8, u8, u8),
    pub(super) cell_width_px: u16,
    pub(super) cell_height_px: u16,
}

impl FormulaStyle {
    pub(super) fn detect() -> Self {
        let foreground = crate::terminal_palette::default_fg().unwrap_or_else(|| {
            crate::terminal_palette::default_bg().map_or(FALLBACK_DARK_FOREGROUND, |background| {
                if relative_luminance(background) > 0.5 {
                    FALLBACK_LIGHT_FOREGROUND
                } else {
                    FALLBACK_DARK_FOREGROUND
                }
            })
        });
        let window = crossterm::terminal::window_size().ok();
        let cell_width_px = window.as_ref().map_or(FALLBACK_CELL_WIDTH_PX, |window| {
            cell_dimension(window.width, window.columns, FALLBACK_CELL_WIDTH_PX)
        });
        let cell_height_px = window.as_ref().map_or(FALLBACK_CELL_HEIGHT_PX, |window| {
            cell_dimension(window.height, window.rows, FALLBACK_CELL_HEIGHT_PX)
        });
        Self {
            foreground,
            cell_width_px,
            cell_height_px,
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct PreparedFormula {
    pub(super) path: PathBuf,
    pub(super) open_path: PathBuf,
    pub(super) digest: [u8; 32],
    pub(super) columns: u16,
    pub(super) rows: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FormulaPlan {
    columns: u16,
    rows: u16,
    canvas_width_px: u32,
    canvas_height_px: u32,
    content_width_px: u32,
    content_height_px: u32,
    content_x_px: u32,
    content_y_px: u32,
}

pub(super) fn prepare(
    source: &Path,
    cache_dir: &Path,
    available_width: usize,
    style: FormulaStyle,
) -> Option<PreparedFormula> {
    let source = image::open(source).ok()?.into_rgba8();
    let (left, top, right, bottom) = alpha_bounds(&source)?;
    let coverage = GrayImage::from_fn(right - left, bottom - top, |x, y| {
        Luma([source.get_pixel(left + x, top + y).0[3]])
    });
    let zoom = super::png_cache::persist(recolor_coverage(&coverage, style.foreground), cache_dir)?;
    let plan = formula_plan(coverage.width(), coverage.height(), available_width, style)?;
    let resized = image::imageops::resize(
        &coverage,
        plan.content_width_px,
        plan.content_height_px,
        FilterType::Lanczos3,
    );
    let mut canvas = RgbaImage::new(plan.canvas_width_px, plan.canvas_height_px);
    for (x, y, pixel) in recolor_coverage(&resized, style.foreground).enumerate_pixels() {
        if pixel.0[3] == 0 {
            continue;
        }
        canvas.put_pixel(plan.content_x_px + x, plan.content_y_px + y, *pixel);
    }
    let display = super::png_cache::persist(canvas, cache_dir)?;
    Some(PreparedFormula {
        path: display.path,
        open_path: zoom.path,
        digest: display.digest,
        columns: plan.columns,
        rows: plan.rows,
    })
}

fn recolor_coverage(coverage: &GrayImage, foreground: (u8, u8, u8)) -> RgbaImage {
    RgbaImage::from_fn(coverage.width(), coverage.height(), |x, y| {
        let alpha = enhance_alpha(coverage.get_pixel(x, y).0[0]);
        Rgba([foreground.0, foreground.1, foreground.2, alpha])
    })
}

fn alpha_bounds(image: &RgbaImage) -> Option<(u32, u32, u32, u32)> {
    let mut left = image.width();
    let mut top = image.height();
    let mut right = 0;
    let mut bottom = 0;
    for (x, y, pixel) in image.enumerate_pixels() {
        if pixel.0[3] == 0 {
            continue;
        }
        left = left.min(x);
        top = top.min(y);
        right = right.max(x + 1);
        bottom = bottom.max(y + 1);
    }
    (left < right && top < bottom).then_some((left, top, right, bottom))
}

fn formula_plan(
    source_width_px: u32,
    source_height_px: u32,
    available_width: usize,
    style: FormulaStyle,
) -> Option<FormulaPlan> {
    if source_width_px == 0 || source_height_px == 0 {
        return None;
    }
    let available_columns = u16::try_from(available_width)
        .unwrap_or(u16::MAX)
        .min(MAX_FORMULA_COLUMNS);
    if available_columns < 3 {
        return None;
    }
    let aspect = f64::from(source_width_px) / f64::from(source_height_px);
    let preferred_rows = if aspect < 0.8 {
        MAX_FORMULA_ROWS
    } else if aspect < 1.6 {
        5
    } else {
        4
    };
    for rows in (MIN_FORMULA_ROWS..=preferred_rows).rev() {
        let plan = plan_for_rows(
            source_width_px,
            source_height_px,
            available_columns,
            rows,
            style,
        )?;
        let vertical_content_height = u32::from(rows - 1) * u32::from(style.cell_height_px);
        let width_at_full_height = (f64::from(source_width_px) * f64::from(vertical_content_height)
            / f64::from(source_height_px))
        .round() as u32;
        if width_at_full_height + 2 * u32::from(style.cell_width_px)
            <= u32::from(available_columns) * u32::from(style.cell_width_px)
        {
            return Some(plan);
        }
    }
    plan_for_rows(
        source_width_px,
        source_height_px,
        available_columns,
        MIN_FORMULA_ROWS,
        style,
    )
}

fn plan_for_rows(
    source_width_px: u32,
    source_height_px: u32,
    available_columns: u16,
    rows: u16,
    style: FormulaStyle,
) -> Option<FormulaPlan> {
    let cell_width = u32::from(style.cell_width_px);
    let cell_height = u32::from(style.cell_height_px);
    let canvas_height = u32::from(rows) * cell_height;
    let padding_x = cell_width;
    let padding_y = cell_height / 2;
    let max_content_width = u32::from(available_columns)
        .checked_mul(cell_width)?
        .checked_sub(2 * padding_x)?;
    let max_content_height = canvas_height.checked_sub(2 * padding_y)?;
    let scale = (f64::from(max_content_width) / f64::from(source_width_px))
        .min(f64::from(max_content_height) / f64::from(source_height_px));
    let content_width = (f64::from(source_width_px) * scale).round().max(1.0) as u32;
    let content_height = (f64::from(source_height_px) * scale).round().max(1.0) as u32;
    let required_width = content_width + 2 * padding_x;
    let columns = required_width
        .div_ceil(cell_width)
        .min(u32::from(available_columns)) as u16;
    let canvas_width = u32::from(columns) * cell_width;
    Some(FormulaPlan {
        columns,
        rows,
        canvas_width_px: canvas_width,
        canvas_height_px: canvas_height,
        content_width_px: content_width,
        content_height_px: content_height,
        content_x_px: (canvas_width - content_width) / 2,
        content_y_px: (canvas_height - content_height) / 2,
    })
}

fn enhance_alpha(alpha: u8) -> u8 {
    let coverage = f64::from(alpha) / 255.0;
    let contrasted = ((coverage - 0.5) * 1.06 + 0.5).clamp(0.0, 1.0);
    (contrasted.powf(0.92) * 255.0).round() as u8
}

fn cell_dimension(pixels: u16, cells: u16, fallback: u16) -> u16 {
    if pixels == 0 || cells == 0 {
        return fallback;
    }
    let dimension = (u32::from(pixels) + u32::from(cells) / 2) / u32::from(cells);
    u16::try_from(dimension)
        .ok()
        .filter(|dimension| (4..=128).contains(dimension))
        .unwrap_or(fallback)
}

fn relative_luminance((red, green, blue): (u8, u8, u8)) -> f64 {
    (0.2126 * f64::from(red) + 0.7152 * f64::from(green) + 0.0722 * f64::from(blue)) / 255.0
}

#[cfg(test)]
#[path = "formula_tests.rs"]
mod tests;
