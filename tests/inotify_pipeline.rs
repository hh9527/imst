use std::fs;
use std::time::Duration;

use camino::{Utf8Path, Utf8PathBuf};
use imst::{
    FileAction, FileEvent, FileState, InotifyFileWatcher, LoaderStage, ReuseUpdate as _,
    TopConfigSpec, UserIntentSpec,
};
use tempfile::TempDir;
use tokio::time::timeout;

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

async fn wait_for_change(watcher: &mut InotifyFileWatcher) {
    timeout(Duration::from_secs(2), watcher.next_change())
        .await
        .expect("timed out waiting for inotify")
        .expect("inotify failed");
}

#[tokio::test]
async fn atomic_rename_reloads_json_and_updates_digest() {
    let dir = tempfile::tempdir().unwrap();
    let target = utf8_path(&dir, "intent.json");
    let mut watcher = InotifyFileWatcher::new(&target).unwrap();
    let mut state = FileState::<UserIntentSpec>::default();
    let empty_digest = *state.value.digest();

    atomic_replace(
        &target,
        br#"{"packages":[{"items":[],"name":"from-inotify"}]}"#,
    );
    wait_for_change(&mut watcher).await;

    let mut actions = state
        .reduce(
            &target,
            &target,
            FileEvent::ReloadRequested {
                key: target.clone(),
            },
        )
        .unwrap();
    let reload = match actions.remove(0) {
        FileAction::Reload(effect) => effect,
        _ => panic!("expected reload effect"),
    };
    state
        .reduce(
            &target,
            &target,
            FileEvent::ReloadStarted {
                key: target.clone(),
            },
        )
        .unwrap();

    let finished = reload.apply(1).await;
    let actions = state.reduce(&target, &target, finished).unwrap();

    assert_eq!(state.stage, LoaderStage::Idle);
    assert_eq!(state.value.packages.len(), 1);
    assert_eq!(state.value.packages[0].spec.name, "from-inotify");
    assert_ne!(state.value.digest(), &empty_digest);
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, FileAction::ValueChanged { .. }))
    );
}

#[tokio::test]
async fn watcher_accepts_only_target_move_delete_and_close_write_events() {
    let dir = tempfile::tempdir().unwrap();
    let target = utf8_path(&dir, "intent.json");
    let backup = utf8_path(&dir, "intent.json.bak");
    let adjacent = utf8_path(&dir, "other.json");
    let mut watcher = InotifyFileWatcher::new(&target).unwrap();

    atomic_replace(&adjacent, b"{}");
    assert!(
        timeout(Duration::from_millis(50), watcher.next_change())
            .await
            .is_err()
    );

    atomic_replace(&target, b"{}");
    wait_for_change(&mut watcher).await;

    fs::write(&target, br#"{"packages":[]}"#).unwrap();
    wait_for_change(&mut watcher).await;

    fs::rename(&target, &backup).unwrap();
    wait_for_change(&mut watcher).await;

    fs::rename(&backup, &target).unwrap();
    wait_for_change(&mut watcher).await;

    fs::remove_file(&target).unwrap();
    wait_for_change(&mut watcher).await;
}

#[tokio::test]
async fn reordered_top_config_updates_stat_without_downstream_change() {
    let dir = tempfile::tempdir().unwrap();
    let target = utf8_path(&dir, "config.json");
    let mut watcher = InotifyFileWatcher::new(&target).unwrap();
    let mut state = FileState::<TopConfigSpec>::default();

    atomic_replace(&target, br#"{"subscribe":["/tmp/a.json","/tmp/b.json"]}"#);
    wait_for_change(&mut watcher).await;
    let actions = state
        .reduce(&(), &target, FileEvent::ReloadRequested { key: () })
        .unwrap();
    let reload = match actions.into_iter().next().unwrap() {
        FileAction::Reload(effect) => effect,
        _ => panic!("expected reload effect"),
    };
    state
        .reduce(&(), &target, FileEvent::ReloadStarted { key: () })
        .unwrap();
    let actions = state.reduce(&(), &target, reload.apply(1).await).unwrap();
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, FileAction::ValueChanged { .. }))
    );
    let first_digest = *state.value.digest();
    let first_stat = state.stat.unwrap();

    atomic_replace(&target, br#"{"subscribe":["/tmp/b.json","/tmp/a.json"]}"#);
    wait_for_change(&mut watcher).await;
    let actions = state
        .reduce(&(), &target, FileEvent::ReloadRequested { key: () })
        .unwrap();
    let reload = match actions.into_iter().next().unwrap() {
        FileAction::Reload(effect) => effect,
        _ => panic!("expected reload effect"),
    };
    state
        .reduce(&(), &target, FileEvent::ReloadStarted { key: () })
        .unwrap();
    let actions = state.reduce(&(), &target, reload.apply(2).await).unwrap();

    assert_ne!(state.stat, Some(first_stat));
    assert_eq!(state.value.digest(), &first_digest);
    assert!(
        !actions
            .iter()
            .any(|action| matches!(action, FileAction::ValueChanged { .. }))
    );
}
