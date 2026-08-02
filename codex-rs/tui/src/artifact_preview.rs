//! Latest assistant-authored static visualization shown above the composer.

use std::cell::Cell;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize as _;
use ratatui::text::Line;
use ratatui::widgets::Clear;
use ratatui::widgets::Widget;

use crate::inline_visualization::ValidatedVisualizationImage;
use crate::terminal_image::DrawRequest;
use crate::terminal_image::ImageSupport;

use crate::render::renderable::Renderable;

const MAX_PREVIEW_COLUMNS: u16 = 100;
const MAX_PREVIEW_ROWS: u16 = 18;
const TERMINAL_ROW_HEIGHT_PX: u16 = 15;
const CELL_ASPECT_CORRECTION: f64 = 0.52;

#[derive(Debug)]
pub(crate) struct ArtifactPreview {
    image: ValidatedVisualizationImage,
    support: ImageSupport,
    image_area: Cell<Option<Rect>>,
}

impl ArtifactPreview {
    pub(crate) fn new(image: ValidatedVisualizationImage) -> Self {
        Self::new_with_support(image, crate::terminal_image::detect_image_support())
    }

    fn new_with_support(image: ValidatedVisualizationImage, support: ImageSupport) -> Self {
        Self {
            image,
            support,
            image_area: Cell::new(None),
        }
    }

    pub(crate) fn image_enabled(&self) -> bool {
        self.support.protocol().is_some()
    }

    pub(crate) fn draw_request(&self) -> Option<DrawRequest> {
        let area = self.image_area.get()?;
        Some(DrawRequest {
            frame: self.image.path.clone(),
            protocol: self.support.protocol()?,
            x: area.x,
            y: area.y,
            clear_top_y: area.y,
            columns: area.width,
            rows: area.height,
            height_px: area.height.saturating_mul(TERMINAL_ROW_HEIGHT_PX),
            sixel_dir: self.image.path.parent()?.join("sixel"),
        })
    }

    fn image_size(&self, width: u16) -> Option<(u16, u16)> {
        if !self.image_enabled() {
            return None;
        }
        let available_columns = width.saturating_sub(2).min(MAX_PREVIEW_COLUMNS);
        if available_columns == 0 || self.image.width == 0 || self.image.height == 0 {
            return None;
        }
        let aspect =
            f64::from(self.image.height) / f64::from(self.image.width) * CELL_ASPECT_CORRECTION;
        let rows = (f64::from(available_columns) * aspect)
            .round()
            .max(1.0)
            .min(f64::from(MAX_PREVIEW_ROWS)) as u16;
        let columns = (f64::from(rows) / aspect)
            .round()
            .max(1.0)
            .min(f64::from(available_columns)) as u16;
        Some((columns, rows))
    }
}

impl Renderable for ArtifactPreview {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        let Some((columns, rows)) = self.image_size(area.width) else {
            self.image_area.set(None);
            return;
        };
        if area.height < rows.saturating_add(1) {
            self.image_area.set(None);
            return;
        }

        Line::from(format!("Visualization: {}", self.image.file_name).dim())
            .render(Rect::new(area.x, area.y, area.width, 1), buf);
        self.image_area.set(Some(Rect::new(
            area.x + area.width.saturating_sub(columns) / 2,
            area.y.saturating_add(1),
            columns,
            rows,
        )));
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.image_size(width)
            .map_or(0, |(_columns, rows)| rows.saturating_add(1))
    }
}

#[cfg(test)]
#[path = "artifact_preview_tests.rs"]
mod tests;
