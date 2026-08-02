//! Chat lifecycle integration for assistant-authored static visualization previews.

use super::*;

impl ChatWidget {
    pub(super) fn update_artifact_preview(&mut self, markdown: &str) {
        self.artifact_preview = self
            .thread_id
            .and_then(|thread_id| {
                crate::inline_visualization::InlineVisualizationContext::from_config(
                    &self.config,
                    thread_id,
                )
            })
            .and_then(|context| context.latest_image(markdown))
            .map(crate::artifact_preview::ArtifactPreview::new);
        self.request_redraw();
    }

    pub(crate) fn artifact_preview_image_enabled(&self) -> bool {
        self.artifact_preview
            .as_ref()
            .is_some_and(crate::artifact_preview::ArtifactPreview::image_enabled)
    }

    pub(crate) fn artifact_preview_draw(&self) -> Option<crate::terminal_image::DrawRequest> {
        if !self.bottom_pane.no_modal_or_popup_active() {
            return None;
        }
        self.artifact_preview.as_ref()?.draw_request()
    }

    pub(crate) fn clear_artifact_preview(&mut self) {
        if self.artifact_preview.take().is_some() {
            self.request_redraw();
        }
    }
}
