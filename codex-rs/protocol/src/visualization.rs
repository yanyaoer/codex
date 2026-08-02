use std::path::Path;
use std::path::PathBuf;

use chrono::DateTime;
use uuid::Uuid;

use crate::ThreadId;

/// Return the durable artifact directory owned by one Codex thread.
pub fn thread_visualization_dir(codex_home: &Path, thread_id: ThreadId) -> Option<PathBuf> {
    let thread_id = thread_id.to_string();
    let uuid = Uuid::parse_str(&thread_id).ok()?;
    let timestamp = uuid.get_timestamp()?;
    let (seconds, nanos) = timestamp.to_unix();
    let created_at = DateTime::from_timestamp(i64::try_from(seconds).ok()?, nanos)?;

    Some(
        codex_home
            .join("visualizations")
            .join(created_at.format("%Y/%m/%d").to_string())
            .join(thread_id),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_a_thread_scoped_path_for_uuid_v7() {
        let thread_id = ThreadId::new();
        let path = thread_visualization_dir(Path::new("codex-home"), thread_id)
            .expect("UUIDv7 thread path");
        let thread_id = thread_id.to_string();

        assert!(path.starts_with(Path::new("codex-home").join("visualizations")));
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some(thread_id.as_str())
        );
    }

    #[test]
    fn rejects_thread_ids_without_a_timestamp() {
        let thread_id =
            ThreadId::from_string("123e4567-e89b-42d3-a456-426614174000").expect("valid UUIDv4");

        assert!(thread_visualization_dir(Path::new("codex-home"), thread_id).is_none());
    }
}
