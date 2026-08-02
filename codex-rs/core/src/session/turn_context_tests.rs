use super::*;

#[test]
fn visualization_grant_is_limited_to_the_thread_directory() {
    let workspace = tempfile::tempdir().expect("workspace");
    let visualization = tempfile::tempdir().expect("visualization root");
    let visualization = AbsolutePathBuf::from_absolute_path(visualization.path())
        .expect("absolute visualization root");

    let profile = grant_visualization_dir(
        PermissionProfile::read_only(),
        workspace.path(),
        Some(&visualization),
    );
    let policy = profile.file_system_sandbox_policy();

    assert!(policy.can_write_path_with_cwd(&visualization.join("chart.png"), workspace.path()));
    assert!(!policy.can_write_path_with_cwd(&workspace.path().join("source.rs"), workspace.path()));
}

#[test]
fn visualization_grant_does_not_modify_external_enforcement() {
    let visualization = tempfile::tempdir().expect("visualization root");
    let visualization = AbsolutePathBuf::from_absolute_path(visualization.path())
        .expect("absolute visualization root");
    let profile = PermissionProfile::External {
        network: codex_protocol::permissions::NetworkSandboxPolicy::Restricted,
    };

    assert_eq!(
        grant_visualization_dir(
            profile.clone(),
            visualization.as_path(),
            Some(&visualization)
        ),
        profile
    );
}
