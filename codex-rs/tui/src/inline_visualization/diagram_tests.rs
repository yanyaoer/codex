use super::*;
use pretty_assertions::assert_eq;

fn dark_palette() -> DiagramPalette {
    DiagramPalette {
        foreground: (230, 230, 230),
        background: (30, 30, 30),
        accent: (137, 180, 250),
    }
}

#[test]
fn preparation_removes_card_background_and_maps_terminal_palette() {
    let temporary = tempfile::tempdir().expect("temp dir");
    let source_path = temporary.path().join("source.png");
    let cache = temporary.path().join("cache");
    let mut source = RgbaImage::from_pixel(5, 3, Rgba([255, 255, 255, 255]));
    source.put_pixel(1, 1, Rgba([247, 248, 254, 255]));
    source.put_pixel(2, 1, Rgba([10, 15, 37, 255]));
    source.put_pixel(3, 1, Rgba([13, 50, 178, 255]));
    source.save(&source_path).expect("save source");

    let prepared = prepare(&source_path, &cache, dark_palette()).expect("prepare diagram");
    let output = image::open(&prepared.path)
        .expect("decode prepared diagram")
        .into_rgba8();

    assert_eq!((prepared.width, prepared.height), (5, 3));
    assert_eq!(
        [
            output.get_pixel(0, 0).0,
            output.get_pixel(1, 1).0,
            output.get_pixel(2, 1).0,
            output.get_pixel(3, 1).0,
        ],
        [
            [0, 0, 0, 0],
            [50, 50, 50, 255],
            [230, 230, 230, 255],
            [137, 180, 250, 255],
        ]
    );
}

#[test]
fn transparent_source_does_not_invent_a_card_background() {
    let mut source = RgbaImage::new(3, 3);
    source.put_pixel(1, 1, Rgba([51, 51, 51, 128]));

    assert_eq!(bright_edge_background(&source), None);
}
