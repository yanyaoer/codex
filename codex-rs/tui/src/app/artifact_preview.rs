//! Terminal lifecycle for the latest assistant-authored artifact preview.

use super::*;

impl App {
    pub(super) fn clear_artifact_preview_image(&mut self, tui: &mut tui::Tui) -> Result<()> {
        if let Err(err) = tui.clear_artifact_preview_image() {
            match err {
                crate::terminal_image::RenderError::Terminal(err) => return Err(err.into()),
                crate::terminal_image::RenderError::Asset(err) => {
                    tracing::warn!(error = %err, "failed to clear artifact preview image");
                }
            }
        }
        Ok(())
    }

    pub(super) fn handle_artifact_preview_image_render_error(
        &mut self,
        tui: &mut tui::Tui,
        err: crate::terminal_image::RenderError,
    ) -> Result<()> {
        match err {
            crate::terminal_image::RenderError::Terminal(err) => Err(err.into()),
            crate::terminal_image::RenderError::Asset(err) => {
                tracing::warn!(error = %err, "failed to render artifact preview image");
                self.chat_widget.clear_artifact_preview();
                self.clear_artifact_preview_image(tui)
            }
        }
    }
}
