use super::*;
use pretty_assertions::assert_eq;

fn style() -> FormulaStyle {
    FormulaStyle {
        foreground: (12, 34, 56),
        cell_width_px: 8,
        cell_height_px: 16,
    }
}

#[test]
fn formula_plan_uses_cell_aligned_three_to_six_row_canvas() {
    let regular = formula_plan(300, 100, /*available_width*/ 80, style()).expect("regular plan");
    let tall = formula_plan(80, 200, /*available_width*/ 80, style()).expect("tall plan");
    let wide = formula_plan(4_000, 100, /*available_width*/ 40, style()).expect("wide plan");

    assert_eq!(regular.rows, 4);
    assert_eq!(tall.rows, 6);
    assert_eq!(wide.rows, 3);
    for plan in [regular, tall, wide] {
        assert!((MIN_FORMULA_ROWS..=MAX_FORMULA_ROWS).contains(&plan.rows));
        assert_eq!(plan.canvas_width_px % 8, 0);
        assert_eq!(plan.canvas_height_px % 16, 0);
        assert!(plan.columns <= 80);
    }
}

#[test]
fn preparation_crops_alpha_recolors_and_preserves_transparency() {
    let temporary = tempfile::tempdir().expect("temp dir");
    let source_path = temporary.path().join("source.png");
    let cache = temporary.path().join("cache");
    let mut source = RgbaImage::new(24, 16);
    for y in 4..12 {
        for x in 5..19 {
            source.put_pixel(x, y, Rgba([0, 0, 0, if x == 5 { 64 } else { 255 }]));
        }
    }
    source.save(&source_path).expect("save source");

    let prepared =
        prepare(&source_path, &cache, /*available_width*/ 60, style()).expect("prepare formula");
    let output = image::open(&prepared.path)
        .expect("decode prepared formula")
        .into_rgba8();
    let bounds = alpha_bounds(&output).expect("visible formula");

    assert!((MIN_FORMULA_ROWS..=MAX_FORMULA_ROWS).contains(&prepared.rows));
    assert_eq!(output.width(), u32::from(prepared.columns) * 8);
    assert_eq!(output.height(), u32::from(prepared.rows) * 16);
    assert!(bounds.0 > 0 && bounds.1 > 0);
    assert!(bounds.2 < output.width() && bounds.3 < output.height());
    assert!(
        output
            .pixels()
            .filter(|pixel| pixel.0[3] > 0)
            .all(|pixel| { pixel.0[..3] == [12, 34, 56] })
    );
    assert!(
        output
            .pixels()
            .any(|pixel| pixel.0[3] > 64 && pixel.0[3] < 255)
    );
}

#[test]
fn cell_dimension_rejects_missing_or_implausible_pixel_metrics() {
    assert_eq!(cell_dimension(2_400, 120, 8), 20);
    assert_eq!(cell_dimension(0, 120, 8), 8);
    assert_eq!(cell_dimension(32_000, 1, 8), 8);
}
