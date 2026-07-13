use std::fs;
use std::time::Duration;

use camino::{Utf8Path, Utf8PathBuf};
use imst::{Reconciler, RuntimeOptions, StateSnapshot};
use serde_json::json;
use tempfile::TempDir;
use tokio::time::{sleep, timeout};

fn utf8_path(dir: &TempDir, name: &str) -> Utf8PathBuf {
    Utf8PathBuf::from_path_buf(dir.path().join(name)).unwrap()
}

fn atomic_replace(target: &Utf8Path, bytes: &[u8]) {
    let temporary = target.with_file_name(format!(
        ".{}.tmp",
        target.file_name().expect("target file name")
    ));
    fs::write(&temporary, bytes).unwrap();
    fs::rename(temporary, target).unwrap();
}

fn intent(name: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "packages": [{ "items": [], "name": name }]
    }))
    .unwrap()
}

fn top_config(paths: &[&Utf8Path]) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "subscribe": paths.iter().map(|path| path.as_str()).collect::<Vec<_>>()
    }))
    .unwrap()
}

async fn wait_until(
    reconciler: &mut Reconciler,
    predicate: impl Fn(&StateSnapshot) -> bool,
) -> StateSnapshot {
    timeout(Duration::from_secs(3), async {
        loop {
            let snapshot = reconciler.snapshot();
            if predicate(&snapshot) {
                return snapshot;
            }
            reconciler.changed().await.unwrap();
        }
    })
    .await
    .expect("state did not converge")
}

#[tokio::test]
async fn removed_subscription_stops_contributing_during_continuous_reconciliation() {
    let dir = tempfile::tempdir().unwrap();
    let config = utf8_path(&dir, "config.json");
    let source_a = utf8_path(&dir, "a.json");
    let source_b = utf8_path(&dir, "b.json");
    fs::write(&source_a, intent("a-v1")).unwrap();
    fs::write(&source_b, intent("b-v1")).unwrap();
    fs::write(&config, top_config(&[&source_a])).unwrap();

    let mut reconciler = Reconciler::start(
        config.clone(),
        RuntimeOptions {
            watch_debounce: Duration::from_millis(10),
        },
    )
    .await
    .unwrap();

    let initially_loaded = wait_until(&mut reconciler, |snapshot| {
        snapshot
            .intent
            .sources
            .get(&source_a)
            .is_some_and(|items| items.first().is_some_and(|item| item.spec.name == "a-v1"))
            && !snapshot.intent.sources.contains_key(&source_b)
    })
    .await;
    assert_eq!(initially_loaded.watch_list.len(), 1);

    atomic_replace(&config, &top_config(&[&source_a, &source_b]));
    let added = wait_until(&mut reconciler, |snapshot| {
        snapshot
            .intent
            .sources
            .get(&source_b)
            .is_some_and(|items| items.first().is_some_and(|item| item.spec.name == "b-v1"))
    })
    .await;
    assert_eq!(added.watch_list.len(), 2);

    atomic_replace(&config, &top_config(&[&source_a]));
    let removed = wait_until(&mut reconciler, |snapshot| {
        snapshot.watch_list.len() == 1
            && snapshot.watch_list.contains(&source_a)
            && !snapshot.intent.sources.contains_key(&source_b)
    })
    .await;
    let digest_after_removal = removed.intent.digest;

    atomic_replace(&source_b, &intent("b-v2"));
    sleep(Duration::from_millis(100)).await;
    let after_ignored_change = reconciler.snapshot();
    assert_eq!(after_ignored_change.intent.digest, digest_after_removal);
    assert!(!after_ignored_change.intent.sources.contains_key(&source_b));

    atomic_replace(&source_a, &intent("a-v2"));
    let updated = wait_until(&mut reconciler, |snapshot| {
        snapshot
            .intent
            .sources
            .get(&source_a)
            .is_some_and(|items| items.first().is_some_and(|item| item.spec.name == "a-v2"))
    })
    .await;
    assert_ne!(updated.intent.digest, digest_after_removal);

    reconciler.shutdown().await.unwrap();
}
