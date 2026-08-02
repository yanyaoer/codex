//! Terminal-owned rendering for static images outside Ratatui's text buffer.
//!
//! Callers own layout and semantic lifecycle. This module owns terminal protocol
//! selection, Kitty/Sixel transport, image identity, and cleanup state.

use std::io::Write;
use std::path::PathBuf;

use anyhow::Context;

mod protocol;
mod sixel;

pub(crate) use protocol::ImageProtocol;
pub(crate) use protocol::ImageSupport;
pub(crate) use protocol::ImageUnsupportedReason;
#[cfg(not(test))]
pub(crate) use protocol::ProtocolSelection;
pub(crate) use protocol::detect_image_support;

#[derive(Debug, Clone)]
pub(crate) struct DrawRequest {
    pub(crate) frame: PathBuf,
    pub(crate) protocol: ImageProtocol,
    pub(crate) x: u16,
    pub(crate) y: u16,
    pub(crate) clear_top_y: u16,
    pub(crate) columns: u16,
    pub(crate) rows: u16,
    pub(crate) height_px: u16,
    pub(crate) sixel_dir: PathBuf,
}

#[derive(Debug)]
pub(crate) enum RenderError {
    Terminal(std::io::Error),
    Asset(anyhow::Error),
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Terminal(err) => write!(f, "terminal image write failed: {err}"),
            Self::Asset(err) => write!(f, "terminal image asset unavailable: {err}"),
        }
    }
}

impl std::error::Error for RenderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Terminal(err) => Some(err),
            Self::Asset(err) => Some(err.as_ref()),
        }
    }
}

impl From<std::io::Error> for RenderError {
    fn from(err: std::io::Error) -> Self {
        Self::Terminal(err)
    }
}

#[derive(Debug, Default)]
pub(crate) struct RenderState {
    last_sixel_clear_area: Option<SixelClearArea>,
    last_protocol: Option<ImageProtocol>,
}

pub(crate) fn render(
    writer: &mut impl Write,
    state: &mut RenderState,
    image_id: u32,
    request: Option<DrawRequest>,
) -> std::result::Result<(), RenderError> {
    use crossterm::cursor::MoveTo;
    use crossterm::cursor::RestorePosition;
    use crossterm::cursor::SavePosition;
    use crossterm::queue;

    let Some(request) = request else {
        if state.last_protocol.take().is_some_and(is_kitty_protocol) {
            write!(writer, "{}", protocol::kitty_delete_image(image_id))?;
        }
        if let Some(area) = state.last_sixel_clear_area.take() {
            queue!(writer, SavePosition)?;
            clear_sixel_area(writer, area)?;
            queue!(writer, RestorePosition)?;
        }
        writer.flush()?;
        return Ok(());
    };

    if state.last_protocol.take().is_some_and(is_kitty_protocol)
        || is_kitty_protocol(request.protocol)
    {
        write!(writer, "{}", protocol::kitty_delete_image(image_id))?;
    }
    state.last_protocol = Some(request.protocol);

    let payload = match request.protocol {
        ImageProtocol::Kitty => Payload::Text(
            protocol::kitty_transmit_png_with_id(
                &request.frame,
                request.columns,
                request.rows,
                Some(image_id),
            )
            .map_err(RenderError::Asset)?,
        ),
        ImageProtocol::KittyLocalFile => Payload::Text(
            protocol::kitty_transmit_png_file_with_id(
                &request.frame,
                request.columns,
                request.rows,
                Some(image_id),
            )
            .map_err(RenderError::Asset)?,
        ),
        ImageProtocol::Sixel => {
            let path = protocol::sixel_frame(&request.frame, &request.sixel_dir, request.height_px)
                .map_err(RenderError::Asset)?;
            let sixel = std::fs::read(&path)
                .with_context(|| format!("read {}", path.display()))
                .map_err(RenderError::Asset)?;
            Payload::Bytes(sixel)
        }
    };

    queue!(writer, SavePosition)?;
    let current_sixel_clear_area =
        matches!(request.protocol, ImageProtocol::Sixel).then(|| SixelClearArea::from(&request));
    if let Some(previous_area) = state.last_sixel_clear_area.take()
        && Some(previous_area) != current_sixel_clear_area
    {
        clear_sixel_area(writer, previous_area)?;
    }
    if let Some(area) = current_sixel_clear_area {
        clear_sixel_area(writer, area)?;
        state.last_sixel_clear_area = Some(area);
    }
    queue!(writer, MoveTo(request.x, request.y))?;
    match payload {
        Payload::Text(payload) => write!(writer, "{payload}")?,
        Payload::Bytes(payload) => writer.write_all(&payload)?,
    }
    queue!(writer, RestorePosition)?;
    writer.flush()?;
    Ok(())
}

enum Payload {
    Text(String),
    Bytes(Vec<u8>),
}

fn is_kitty_protocol(protocol: ImageProtocol) -> bool {
    matches!(
        protocol,
        ImageProtocol::Kitty | ImageProtocol::KittyLocalFile
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SixelClearArea {
    x: u16,
    clear_top_y: u16,
    clear_bottom_y: u16,
    columns: u16,
}

impl From<&DrawRequest> for SixelClearArea {
    fn from(request: &DrawRequest) -> Self {
        Self {
            x: request.x,
            clear_top_y: request.clear_top_y,
            clear_bottom_y: request.y.saturating_add(request.rows),
            columns: request.columns,
        }
    }
}

fn clear_sixel_area(writer: &mut impl Write, area: SixelClearArea) -> std::io::Result<()> {
    use crossterm::cursor::MoveTo;
    use crossterm::queue;

    let blank = " ".repeat(area.columns.into());
    for row in area.clear_top_y..area.clear_bottom_y {
        queue!(writer, MoveTo(area.x, row))?;
        write!(writer, "{blank}")?;
    }
    Ok(())
}
