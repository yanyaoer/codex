use pretty_assertions::assert_eq;

use super::*;

fn test_image(path: PathBuf, transport: KittyTransport) -> InlineImage {
    InlineImage {
        path,
        image_id: 0x0200_002a,
        columns: 2,
        rows: 2,
        transport,
    }
}

#[test]
fn placeholders_encode_all_image_id_bytes() {
    let lines = test_image(PathBuf::new(), KittyTransport::Direct).placeholder_lines();
    let text = lines
        .iter()
        .map(|line| line.spans[0].content.as_ref())
        .collect::<Vec<_>>();

    assert_eq!(crate::width::display_width(text[0]), 2);
    assert_eq!(
        text,
        vec![
            "\u{10eeee}\u{0305}\u{0305}\u{030e}\u{10eeee}\u{0305}\u{030d}\u{030e}",
            "\u{10eeee}\u{030d}\u{0305}\u{030e}\u{10eeee}\u{030d}\u{030d}\u{030e}",
        ]
    );
}

#[test]
fn wraps_each_command_for_nested_tmux_and_detects_supported_depths() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("image.png");
    fs::write(&path, [0_u8; 4096]).expect("write image");
    let mut output = Vec::new();
    test_image(path, KittyTransport::TmuxPassthrough { depth: 2 })
        .write_transmission(&mut output)
        .expect("write transmission");
    let output = String::from_utf8(output).expect("UTF-8 protocol");
    assert_eq!(output.matches("Ptmux;").count(), 4);
    assert_eq!(output.matches("_Ga=T,U=1").count(), 1);
    assert_eq!(output.matches("_Gm=0").count(), 1);
    assert_eq!(
        [
            tmux_passthrough_depth(Some("xterm-kitty"), false),
            tmux_passthrough_depth(Some("xterm-ghostty"), false),
            tmux_passthrough_depth(Some("tmux-256color"), true),
            tmux_passthrough_depth(Some("tmux-256color"), false),
            tmux_passthrough_depth(Some("xterm-256color"), true),
        ],
        [Some(1), Some(1), Some(2), None, None]
    );
}
