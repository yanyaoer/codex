//! Ambient terminal pets configured from the /pets slash command.
//!
//! The TUI treats built-in and custom pets differently on purpose:
//! built-in pets are versioned application assets fetched on demand into a
//! managed CODEX_HOME cache, while custom pets remain entirely user-owned data
//! under `$CODEX_HOME/pets/<pet-id>/pet.json` or legacy avatar directories.
//!
//! This module owns the TUI-facing contracts around that split:
//! resolving a selected pet id, preparing frames for terminal image protocols,
//! rendering the ambient sprite and picker preview, and preserving enough
//! metadata for `/pets` to behave like a first-class configuration surface.
//! It prepares built-in assets before loading pets, but does not own config
//! persistence or popup orchestration; callers must persist the final selection
//! only after the load succeeds.

mod ambient;
mod asset_pack;
mod catalog;
mod frames;
mod model;
mod picker;
mod preview;

use anyhow::Context;
use anyhow::Result;
use codex_http_client::RouteAwareClientPool;
use codex_utils_absolute_path::AbsolutePathBuf;

use crate::tui::FrameRequester;

pub(crate) use crate::terminal_image::DrawRequest as AmbientPetDraw;
#[cfg(test)]
use crate::terminal_image::ImageProtocol;
use crate::terminal_image::RenderError;
use crate::terminal_image::RenderState;
pub(crate) use ambient::AmbientPet;
pub(crate) use ambient::PetNotificationKind;
#[cfg(test)]
pub(crate) use ambient::test_ambient_pet;
pub(crate) use asset_pack::builtin_spritesheet_path;
#[cfg(test)]
pub(crate) use asset_pack::write_test_pack;
pub(crate) use picker::PET_PICKER_VIEW_ID;
pub(crate) use picker::build_pet_picker_params;
pub(crate) use preview::PetPickerPreviewState;

pub(crate) const DEFAULT_PET_ID: &str = "codex";
pub(crate) const DISABLED_PET_ID: &str = "disabled";

pub(crate) fn image_unsupported_message(
    reason: crate::terminal_image::ImageUnsupportedReason,
) -> &'static str {
    match reason {
        crate::terminal_image::ImageUnsupportedReason::Tmux => {
            "Pets are disabled in tmux. Terminal images don’t stay pane-local in tmux and can corrupt scrollback or move between panes. Run Codex outside tmux to use pets."
        }
        crate::terminal_image::ImageUnsupportedReason::Zellij => {
            "Pets are disabled in Zellij. Terminal images don’t stay reliably pane-local in Zellij. Run Codex outside Zellij to use pets."
        }
        crate::terminal_image::ImageUnsupportedReason::Iterm2TooOld => {
            "Pets require iTerm2 3.6 or newer. Upgrade iTerm2 to use terminal pets."
        }
        crate::terminal_image::ImageUnsupportedReason::Terminal => {
            "Pets aren’t available in this terminal. Terminal pets need image support, and this terminal environment doesn’t expose a supported image protocol. Try a terminal with Kitty graphics or Sixel support, or run Codex outside tmux."
        }
    }
}

/// Ensure that a selected built-in pet has a locally cached spritesheet.
///
/// Custom pets are intentionally a no-op here because their source of truth is
/// already local. Preparing this before loading keeps first-use preview and
/// persistence failures at the asset-fetch boundary rather than surfacing as
/// deeper image-loading errors.
async fn ensure_builtin_pack_for_pet(
    pet_id: &str,
    codex_home: &std::path::Path,
    http_client: &RouteAwareClientPool,
) -> Result<()> {
    if let Some(pet) = catalog::builtin_pet(pet_id) {
        asset_pack::ensure_builtin_pet(codex_home, pet, http_client).await?;
    }
    Ok(())
}

/// Prepare a pet's built-in assets and load its synchronous state off the runtime.
pub(crate) async fn load_pet_with_assets(
    pet_id: String,
    codex_home: AbsolutePathBuf,
    frame_requester: FrameRequester,
    animations_enabled: bool,
    http_client: &RouteAwareClientPool,
) -> Result<AmbientPet> {
    ensure_builtin_pack_for_pet(&pet_id, &codex_home, http_client).await?;
    tokio::task::spawn_blocking(move || {
        AmbientPet::load(
            Some(&pet_id),
            &codex_home,
            frame_requester,
            animations_enabled,
        )
    })
    .await
    .context("join pet load task")?
}

const AMBIENT_PET_IMAGE_ID: u32 = 0xC0DE;
const PET_PICKER_PREVIEW_IMAGE_ID: u32 = 0xC0DF;

pub(crate) fn render_ambient_pet_image(
    writer: &mut impl std::io::Write,
    state: &mut RenderState,
    request: Option<AmbientPetDraw>,
) -> std::result::Result<(), RenderError> {
    crate::terminal_image::render(writer, state, AMBIENT_PET_IMAGE_ID, request)
}

pub(crate) fn render_pet_picker_preview_image(
    writer: &mut impl std::io::Write,
    state: &mut RenderState,
    request: Option<AmbientPetDraw>,
) -> std::result::Result<(), RenderError> {
    crate::terminal_image::render(writer, state, PET_PICKER_PREVIEW_IMAGE_ID, request)
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;
    use std::io;
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn ambient_pet_image_restores_cursor_after_drawing() {
        let dir = tempfile::tempdir().unwrap();
        let frame = dir.path().join("frame.png");
        std::fs::write(&frame, b"png").unwrap();
        let request = AmbientPetDraw {
            frame,
            protocol: ImageProtocol::Kitty,
            x: 2,
            y: 3,
            clear_top_y: 3,
            columns: 4,
            rows: 5,
            height_px: 75,
            sixel_dir: PathBuf::new(),
        };
        let mut output = Vec::new();
        let mut state = RenderState::default();

        render_ambient_pet_image(&mut output, &mut state, Some(request)).unwrap();

        let output = String::from_utf8(output).unwrap();
        let save = output.find("\x1b7").expect("saves cursor position");
        let move_to = output.find("\x1b[4;3H").expect("moves to pet position");
        let image = output.find("cG5n").expect("writes image payload");
        let restore = output.find("\x1b8").expect("restores cursor position");
        assert!(save < move_to);
        assert!(move_to < image);
        assert!(image < restore);
    }

    #[test]
    fn kitty_pet_image_clear_deletes_without_moving_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let frame = dir.path().join("frame.png");
        std::fs::write(&frame, b"png").unwrap();
        let request = AmbientPetDraw {
            frame,
            protocol: ImageProtocol::Kitty,
            x: 2,
            y: 3,
            clear_top_y: 3,
            columns: 4,
            rows: 5,
            height_px: 75,
            sixel_dir: PathBuf::new(),
        };
        let mut output = Vec::new();
        let mut state = RenderState::default();

        render_ambient_pet_image(&mut output, &mut state, Some(request)).unwrap();
        output.clear();
        render_ambient_pet_image(&mut output, &mut state, /*request*/ None).unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("Ga=d,d=I,i=49374,q=2;"));
        assert!(!output.contains("\x1b7"));
        assert!(!output.contains("\x1b["));
        assert!(!output.contains("\x1b8"));
    }

    #[test]
    fn kitty_local_file_pet_image_uses_file_reference_without_inline_payload() {
        let dir = tempfile::tempdir().unwrap();
        let frame = dir.path().join("frame.png");
        std::fs::write(&frame, b"png").unwrap();
        let request = AmbientPetDraw {
            frame,
            protocol: ImageProtocol::KittyLocalFile,
            x: 2,
            y: 3,
            clear_top_y: 3,
            columns: 4,
            rows: 2,
            height_px: 75,
            sixel_dir: PathBuf::new(),
        };
        let mut output = Vec::new();
        let mut state = RenderState::default();

        render_ambient_pet_image(&mut output, &mut state, Some(request)).unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("a=d,d=I,i=49374,q=2;"));
        assert!(output.contains("\x1b[4;3H"));
        assert!(output.contains("a=T,t=f,f=100,c=4,r=2,q=2,i=49374;"));
        assert!(!output.contains("cG5n"));
        assert!(output.contains("\x1b8"));
    }

    #[test]
    fn sixel_pet_image_clears_cell_area_before_redrawing() {
        let dir = tempfile::tempdir().unwrap();
        let frame = dir.path().join("frame.png");
        std::fs::write(&frame, b"png").unwrap();
        let sixel_dir = dir.path().join("sixel");
        std::fs::create_dir(&sixel_dir).unwrap();
        let sixel_frame = sixel_dir.join("frame_h75_v2.six");
        std::fs::write(&sixel_frame, b"fake-sixel").unwrap();
        let request = AmbientPetDraw {
            frame,
            protocol: ImageProtocol::Sixel,
            x: 2,
            y: 3,
            clear_top_y: 1,
            columns: 4,
            rows: 2,
            height_px: 75,
            sixel_dir,
        };
        let mut output = Vec::new();
        let mut state = RenderState::default();

        render_ambient_pet_image(&mut output, &mut state, Some(request)).unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("\x1b[2;3H    \x1b[3;3H    \x1b[4;3H    \x1b[5;3H    \x1b[4;3H"));
        assert!(output.contains("fake-sixel"));
        assert!(output.contains("\x1b8"));
    }

    #[test]
    fn sixel_pet_image_clear_erases_last_drawn_area() {
        let dir = tempfile::tempdir().unwrap();
        let frame = dir.path().join("frame.png");
        std::fs::write(&frame, b"png").unwrap();
        let sixel_dir = dir.path().join("sixel");
        std::fs::create_dir(&sixel_dir).unwrap();
        let sixel_frame = sixel_dir.join("frame_h75_v2.six");
        std::fs::write(&sixel_frame, b"fake-sixel").unwrap();
        let request = AmbientPetDraw {
            frame,
            protocol: ImageProtocol::Sixel,
            x: 2,
            y: 3,
            clear_top_y: 1,
            columns: 4,
            rows: 2,
            height_px: 75,
            sixel_dir,
        };
        let mut output = Vec::new();
        let mut state = RenderState::default();

        render_ambient_pet_image(&mut output, &mut state, Some(request)).unwrap();
        output.clear();
        render_ambient_pet_image(&mut output, &mut state, /*request*/ None).unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(!output.contains("Ga=d,d=I,i=49374,q=2;"));
        assert!(output.contains("\x1b7"));
        assert!(output.contains("\x1b[2;3H    \x1b[3;3H    \x1b[4;3H    \x1b[5;3H    "));
        assert!(output.contains("\x1b8"));
        assert!(!output.contains("fake-sixel"));
    }

    #[test]
    fn missing_frame_is_an_asset_error() {
        let dir = tempfile::tempdir().unwrap();
        let request = AmbientPetDraw {
            frame: dir.path().join("missing.png"),
            protocol: ImageProtocol::Kitty,
            x: 2,
            y: 3,
            clear_top_y: 3,
            columns: 4,
            rows: 5,
            height_px: 75,
            sixel_dir: PathBuf::new(),
        };
        let mut output = Vec::new();
        let mut state = RenderState::default();

        let err = render_ambient_pet_image(&mut output, &mut state, Some(request)).unwrap_err();

        assert!(matches!(err, RenderError::Asset(_)));
        assert!(err.source().is_some());
    }

    #[test]
    fn writer_failure_is_a_terminal_error() {
        struct FailingWriter;

        impl io::Write for FailingWriter {
            fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "test writer failed",
                ))
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let request = AmbientPetDraw {
            frame: PathBuf::from("unused.png"),
            protocol: ImageProtocol::Kitty,
            x: 0,
            y: 0,
            clear_top_y: 0,
            columns: 1,
            rows: 1,
            height_px: 1,
            sixel_dir: PathBuf::new(),
        };
        let mut writer = FailingWriter;
        let mut state = RenderState::default();

        let err = render_ambient_pet_image(&mut writer, &mut state, Some(request)).unwrap_err();

        assert!(matches!(err, RenderError::Terminal(_)));
        assert!(err.source().is_some());
    }
}
