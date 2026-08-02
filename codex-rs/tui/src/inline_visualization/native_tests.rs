use super::*;

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

```dot
digraph { agent -> tool }
```
"#;

    let artifacts = artifact_blocks(markdown);

    assert_eq!(artifacts.len(), 2);
    assert_eq!(artifacts[0].format, NativeArtifactFormat::Mermaid);
    assert_eq!(artifacts[0].source.trim(), "flowchart LR\nagent --> tool");
    assert_eq!(artifacts[1].format, NativeArtifactFormat::Dot);
}

#[test]
fn artifact_file_is_content_addressed_and_strictly_recognized() {
    let dot = artifact_file(NativeArtifactFormat::Dot, "digraph { a -> b }\n");
    let same = artifact_file(NativeArtifactFormat::Dot, "digraph { a -> b }\n");
    let mermaid = artifact_file(NativeArtifactFormat::Mermaid, "digraph { a -> b }\n");

    assert_eq!(dot, same);
    assert_ne!(dot, mermaid);
    assert!(is_native_artifact_file(&dot));
    assert!(!is_native_artifact_file(
        "codex-inline-viz-not-a-digest.png"
    ));
    assert!(!is_native_artifact_file("../codex-inline-viz-deadbeef.png"));
}

#[test]
fn common_validation_bounds_source_before_spawning() {
    assert!(validate_source("").is_err());
    assert!(validate_source("a\0b").is_err());
    assert!(validate_source(&"x".repeat(MAX_SOURCE_BYTES + 1)).is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn helper_output_is_validated_and_atomically_persisted() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::tempdir().expect("temp dir");
    let fixture = directory.path().join("fixture.png");
    image::RgbaImage::from_pixel(3, 2, image::Rgba([12, 34, 56, 128]))
        .save(&fixture)
        .expect("save fixture");
    let helper = directory.path().join("renderer");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\noutput=\nwhile [ \"$#\" -gt 0 ]; do\n  if [ \"$1\" = --output ]; then output=$2; shift 2; else shift; fi\ndone\n/bin/cp '{}' \"$output\"\n",
            fixture.display()
        ),
    )
    .expect("write helper");
    let mut permissions = fs::metadata(&helper)
        .expect("helper metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&helper, permissions).expect("make helper executable");
    let artifact = artifact(NativeArtifactFormat::Mermaid, "flowchart LR\na --> b\n");

    assert!(
        render_artifact(directory.path(), &helper, &artifact)
            .await
            .expect("render artifact")
    );
    assert!(
        !render_artifact(directory.path(), &helper, &artifact)
            .await
            .expect("reuse artifact")
    );
    let output = directory
        .path()
        .join(artifact_file(artifact.format, &artifact.source));
    let decoded = image::open(output).expect("decode persisted PNG");
    assert_eq!((decoded.width(), decoded.height()), (3, 2));
}

#[cfg(unix)]
#[tokio::test]
async fn helper_diagnostics_are_returned_without_creating_an_artifact() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::tempdir().expect("temp dir");
    let helper = directory.path().join("renderer");
    fs::write(
        &helper,
        "#!/bin/sh\nprintf 'Mermaid parse error: unexpected token\\n' >&2\nexit 1\n",
    )
    .expect("write helper");
    let mut permissions = fs::metadata(&helper)
        .expect("helper metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&helper, permissions).expect("make helper executable");
    let artifact = artifact(NativeArtifactFormat::Mermaid, "invalid");

    let error = render_artifact(directory.path(), &helper, &artifact)
        .await
        .expect_err("helper should fail")
        .to_string();

    assert!(error.contains("Mermaid parse error: unexpected token"));
    assert!(
        !directory
            .path()
            .join(artifact_file(artifact.format, &artifact.source))
            .exists()
    );
}

#[tokio::test]
async fn invalid_existing_artifact_is_not_treated_as_a_cache_hit() {
    let directory = tempfile::tempdir().expect("temp dir");
    let artifact = artifact(NativeArtifactFormat::Dot, "digraph { a -> b }");
    fs::write(
        directory
            .path()
            .join(artifact_file(artifact.format, &artifact.source)),
        b"not a PNG",
    )
    .expect("write invalid cache entry");

    let error = render_artifact(directory.path(), Path::new("unused"), &artifact)
        .await
        .expect_err("invalid cached artifact must fail validation")
        .to_string();

    assert!(error.contains("validate existing inline visualization artifact"));
}
