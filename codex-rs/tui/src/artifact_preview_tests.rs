use std::path::PathBuf;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::*;
use crate::terminal_image::ImageProtocol;
use crate::terminal_image::ImageUnsupportedReason;

fn image(width: u32, height: u32) -> ValidatedVisualizationImage {
    ValidatedVisualizationImage {
        path: PathBuf::from("/tmp/chart.png"),
        file_name: "chart.png".to_string(),
        width,
        height,
    }
}

#[test]
fn reserves_a_bounded_aspect_aware_preview() {
    let preview = ArtifactPreview::new_with_support(
        image(/*width*/ 1_200, /*height*/ 600),
        ImageSupport::Supported(ImageProtocol::Kitty),
    );
    let area = Rect::new(0, 5, 80, preview.desired_height(/*width*/ 80));
    let mut buffer = Buffer::empty(area);

    preview.render(area, &mut buffer);
    let request = preview.draw_request().expect("image draw request");
    let label = buffer
        .content
        .iter()
        .take(usize::from(area.width))
        .map(ratatui::buffer::Cell::symbol)
        .collect::<String>()
        .trim_end()
        .to_string();

    insta::assert_snapshot!(
        "artifact_preview_layout",
        format!(
            "desired_height={}\nimage_area=({}, {}, {}, {})\nheight_px={}\nlabel={label}",
            area.height, request.x, request.y, request.columns, request.rows, request.height_px
        )
    );
    assert!(request.columns <= MAX_PREVIEW_COLUMNS);
    assert!(request.rows <= MAX_PREVIEW_ROWS);
}

#[test]
fn unsupported_terminals_do_not_reserve_space() {
    let preview = ArtifactPreview::new_with_support(
        image(/*width*/ 400, /*height*/ 300),
        ImageSupport::Unsupported(ImageUnsupportedReason::Terminal),
    );

    assert_eq!(preview.desired_height(/*width*/ 80), 0);
    assert!(preview.draw_request().is_none());
}
