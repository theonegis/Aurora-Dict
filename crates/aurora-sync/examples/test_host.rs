//! Test-only host; never points at the user's normal vocabulary file.
use std::{path::PathBuf, time::Duration};
#[tokio::main]
async fn main() {
    let root = PathBuf::from(std::env::args().nth(1).expect("test directory"));
    assert!(
        root.starts_with("/private/tmp/") || root.starts_with("/tmp/"),
        "test data must be temporary"
    );
    std::fs::create_dir_all(&root).unwrap();
    let host = aurora_sync::Host::start(&root, root.join("desktop.sqlite3"))
        .await
        .unwrap();
    let (pair, _) = host.pairing(Some("127.0.0.1".into())).unwrap();
    std::fs::write(
        root.join("invitation.json"),
        serde_json::to_vec(&pair).unwrap(),
    )
    .unwrap();
    println!("Local test host ready on {}", host.status().port);
    while !root.join("stop").exists() {
        if root.join("refresh").exists() {
            let _ = std::fs::remove_file(root.join("refresh"));
            let (pair, _) = host.pairing(Some("127.0.0.1".into())).unwrap();
            std::fs::write(
                root.join("invitation.json"),
                serde_json::to_vec(&pair).unwrap(),
            )
            .unwrap();
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}
