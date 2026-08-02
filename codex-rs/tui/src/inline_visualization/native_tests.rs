use super::*;
use codex_protocol::ThreadId;

fn artifact(format: NativeArtifactFormat, source: &str) -> NativeArtifact {
    NativeArtifact {
        format,
        source: source.to_string(),
    }
}

#[test]
fn parses_only_top_level_supported_fences() {
    let markdown = r#"Before

```d2
user -> agent
```

> ```mermaid
> flowchart LR
> a --> b
> ```

- ```latex
  E = mc^2
  ```

```Mermaid,theme=dark
flowchart LR
agent --> tool
```
"#;

    let artifacts = artifact_blocks(markdown);

    assert_eq!(artifacts.len(), 2);
    assert_eq!(artifacts[0].format, NativeArtifactFormat::D2);
    assert_eq!(artifacts[0].source.trim(), "user -> agent");
    assert_eq!(artifacts[1].format, NativeArtifactFormat::Mermaid);
    assert_eq!(artifacts[1].source.trim(), "flowchart LR\nagent --> tool");
}

#[test]
fn artifact_file_is_content_addressed_and_strictly_recognized() {
    let d2 = artifact_file(NativeArtifactFormat::D2, "a -> b\n");
    let same = artifact_file(NativeArtifactFormat::D2, "a -> b\n");
    let mermaid = artifact_file(NativeArtifactFormat::Mermaid, "a -> b\n");

    assert_eq!(d2, same);
    assert_ne!(d2, mermaid);
    assert!(is_native_artifact_file(&d2));
    assert!(!is_native_artifact_file(
        "codex-inline-viz-not-a-digest.png"
    ));
    assert!(!is_native_artifact_file("../codex-inline-viz-deadbeef.png"));
}

#[test]
fn automatic_rendering_rejects_external_resource_syntax() {
    assert!(validate_source(&artifact(NativeArtifactFormat::D2, "@x: file.d2")).is_err());
    assert!(
        validate_source(&artifact(
            NativeArtifactFormat::D2,
            "server: {\n  shape: image\n  icon: ./secret.png\n}"
        ))
        .is_err()
    );
    assert!(
        validate_source(&artifact(
            NativeArtifactFormat::Mermaid,
            "flowchart LR\nclick x https://example.com"
        ))
        .is_err()
    );
    assert!(
        validate_source(&artifact(
            NativeArtifactFormat::Latex,
            "\\input{/etc/passwd}"
        ))
        .is_err()
    );
}

#[test]
fn automatic_rendering_allows_d2_node_named_image() {
    assert!(
        validate_source(&artifact(
            NativeArtifactFormat::D2,
            "display: {\n  image: \"InlineImage\\nhash-derived image ID\"\n}"
        ))
        .is_ok()
    );
}

#[tokio::test]
async fn real_renderer_materializes_all_formats_when_requested() {
    if std::env::var_os("CODEX_INLINE_VIZ_REAL_SMOKE").as_deref() != Some(std::ffi::OsStr::new("1"))
    {
        return;
    }

    let codex_home = tempfile::tempdir().expect("temp codex home");
    let mut context = InlineVisualizationContext::new(codex_home.path(), ThreadId::new())
        .expect("visualization context");
    context.native_rendering_enabled = true;
    let markdown = r#"```d2
direction: right
user -> agent -> tool
display: {
  image: "InlineImage\nhash-derived image ID"
  terminal: "Kitty Graphics Protocol"
  image -> terminal
}
tool -> display.image
```

```mermaid
flowchart LR
user --> agent --> tool
```

```latex
E = mc^2
```
"#;

    std::fs::create_dir_all(&context.thread_dir).expect("create thread directory");
    for artifact in artifact_blocks(markdown) {
        let commands = RenderCommands::resolve(artifact.format, &context.managed_bin_dir)
            .unwrap_or_else(|error| panic!("resolve {:?} renderer: {error:#}", artifact.format));
        assert!(
            render_artifact(&context.thread_dir, &commands, &artifact)
                .await
                .unwrap_or_else(|error| panic!("render {:?}: {error:#}", artifact.format))
        );
        let path = context
            .thread_dir
            .join(artifact_file(artifact.format, &artifact.source));
        let image = image::open(&path)
            .unwrap_or_else(|error| panic!("{} should be a decoded PNG: {error}", path.display()));
        assert!(image.width() > 0);
        assert!(image.height() > 0);
        if artifact.format == NativeArtifactFormat::Latex {
            let rgba = image.into_rgba8();
            assert!(rgba.width() >= 400);
            assert!(rgba.height() >= 200);
            assert!(rgba.pixels().any(|pixel| pixel.0[3] == 0));
            assert!(rgba.pixels().any(|pixel| pixel.0[3] > 0));
            let prepared = super::super::formula::prepare(
                &path,
                &context.image_cache_dir,
                /* available_width */ 80,
                super::super::formula::FormulaStyle {
                    foreground: (230, 230, 230),
                    cell_width_px: 16,
                    cell_height_px: 32,
                },
            )
            .expect("prepare terminal-sized formula");
            let display = image::open(&prepared.path).expect("decode terminal-sized formula");
            let zoom = image::open(&prepared.open_path)
                .expect("decode high-resolution formula")
                .into_rgba8();
            assert!(zoom.width() > display.width());
            assert!(zoom.height() > display.height());
            assert!(
                zoom.pixels()
                    .filter(|pixel| pixel.0[3] > 0)
                    .all(|pixel| pixel.0[..3] == [230, 230, 230])
            );
        } else {
            let prepared = super::super::diagram::prepare(
                &path,
                &context.image_cache_dir,
                super::super::diagram::DiagramPalette {
                    foreground: (230, 230, 230),
                    background: (30, 30, 30),
                    accent: (137, 180, 250),
                },
            )
            .expect("prepare terminal-colored diagram");
            let styled = image::open(&prepared.path)
                .expect("decode terminal-colored diagram")
                .into_rgba8();
            assert_eq!(
                (styled.width(), styled.height()),
                (image.width(), image.height())
            );
            assert!(styled.pixels().any(|pixel| pixel.0[3] == 0));
            assert!(styled.pixels().any(|pixel| pixel.0[3] > 0));
            assert!(
                styled
                    .pixels()
                    .filter(|pixel| pixel.0[3] > 0)
                    .all(|pixel| pixel.0[..3] != [255, 255, 255])
            );
        }
    }
}
