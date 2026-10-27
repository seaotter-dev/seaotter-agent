use std::fs;

use harness_core::Effect;
use harness_effects::Search;
use serde_json::json;
use tempfile::TempDir;

#[tokio::test]
async fn a_file_inside_the_root_matches() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("workspace");
    let outside = dir.path().join("outside");
    fs::create_dir_all(root.join("notes")).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(root.join("note.txt"), "find the needle").unwrap();
    fs::write(root.join("notes/other.txt"), "nothing here").unwrap();
    fs::write(outside.join("secret.txt"), "needle outside").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.join("secret.txt"), root.join("link.txt")).unwrap();

    let found = Search::new(&root)
        .unwrap()
        .run(json!({"query": "needle"}))
        .await
        .unwrap();
    assert_eq!(found["paths"], json!(["note.txt"]));
}
