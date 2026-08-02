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
    }
}
