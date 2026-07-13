use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use camino::Utf8PathBuf;
use sha2::{Digest as _, Sha256};

use crate::digest::Sha256Digest;
use crate::file::{FileAction, FileEffect, FileEvent, FileState, FileStateError};
use crate::intent::{PackageSpecItem, ReuseUpdate as _, TopConfigSpec, UserIntentSpec};

const INTENT_STATE_DIGEST_DOMAIN: &[u8] = b"imst:intent-state:v1";

#[derive(Debug)]
pub enum AnyEvent {
    TopConfig(FileEvent<TopConfigSpec>),
    UserIntent(FileEvent<UserIntentSpec>),
}

#[derive(Debug)]
pub enum AnyEffect {
    ReloadTopConfig(FileEffect<TopConfigSpec>),
    ReloadUserIntent(FileEffect<UserIntentSpec>),
    Debounce {
        key: String,
        timeout: Duration,
        event: AnyEvent,
    },
    ReconcileWatches {
        added: BTreeSet<Utf8PathBuf>,
        removed: BTreeSet<Utf8PathBuf>,
    },
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct IntentState {
    pub sources: BTreeMap<Utf8PathBuf, Vec<PackageSpecItem>>,
    pub digest: Sha256Digest,
}

impl Default for IntentState {
    fn default() -> Self {
        Self {
            sources: BTreeMap::new(),
            digest: digest_sources(&BTreeMap::new()),
        }
    }
}

#[derive(Debug)]
pub struct ReconcileState {
    pub config_path: Utf8PathBuf,
    pub top_config: FileState<TopConfigSpec>,
    pub watch_list: BTreeSet<Utf8PathBuf>,
    pub user_intents: BTreeMap<Utf8PathBuf, FileState<UserIntentSpec>>,
    pub intent: IntentState,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct StateSnapshot {
    pub top_config_digest: Sha256Digest,
    pub watch_list: BTreeSet<Utf8PathBuf>,
    pub source_digests: BTreeMap<Utf8PathBuf, Sha256Digest>,
    pub intent: IntentState,
}

impl ReconcileState {
    pub fn new(config_path: Utf8PathBuf) -> Self {
        Self {
            config_path,
            top_config: FileState::default(),
            watch_list: BTreeSet::new(),
            user_intents: BTreeMap::new(),
            intent: IntentState::default(),
        }
    }

    pub fn snapshot(&self) -> StateSnapshot {
        StateSnapshot {
            top_config_digest: *self.top_config.value.digest(),
            watch_list: self.watch_list.clone(),
            source_digests: self
                .user_intents
                .iter()
                .map(|(path, state)| (path.clone(), *state.value.digest()))
                .collect(),
            intent: self.intent.clone(),
        }
    }

    pub fn reduce(&mut self, event: AnyEvent) -> Result<Vec<AnyEffect>, FileStateError> {
        match event {
            AnyEvent::TopConfig(event) => {
                let actions = self.top_config.reduce(&(), &self.config_path, event)?;
                self.apply_top_actions(actions)
            }
            AnyEvent::UserIntent(event) => {
                let path = event.key().clone();
                if !self.watch_list.contains(&path) {
                    return Ok(Vec::new());
                }
                let state = self
                    .user_intents
                    .get_mut(&path)
                    .expect("WatchList and user_intents must agree");
                let actions = state.reduce(&path, &path, event)?;
                Ok(self.apply_user_actions(actions))
            }
        }
    }

    fn apply_top_actions(
        &mut self,
        actions: Vec<FileAction<TopConfigSpec>>,
    ) -> Result<Vec<AnyEffect>, FileStateError> {
        let mut output = Vec::new();
        for action in actions {
            match action {
                FileAction::Reload(effect) => output.push(AnyEffect::ReloadTopConfig(effect)),
                FileAction::DebounceReload { key: (), timeout } => {
                    output.push(AnyEffect::Debounce {
                        key: "config:reload".into(),
                        timeout,
                        event: AnyEvent::TopConfig(FileEvent::ReloadRequested { key: () }),
                    });
                }
                FileAction::ValueChanged { key: () } => {
                    output.extend(self.reconcile_watch_list()?);
                }
            }
        }
        Ok(output)
    }

    fn reconcile_watch_list(&mut self) -> Result<Vec<AnyEffect>, FileStateError> {
        let next = self.top_config.value.subscribe.clone();
        let added: BTreeSet<_> = next.difference(&self.watch_list).cloned().collect();
        let removed: BTreeSet<_> = self.watch_list.difference(&next).cloned().collect();

        for path in &removed {
            self.user_intents.remove(path);
        }
        for path in &added {
            self.user_intents.entry(path.clone()).or_default();
        }
        self.watch_list = next;
        self.rebuild_intent();

        let mut output = vec![AnyEffect::ReconcileWatches {
            added: added.clone(),
            removed,
        }];
        for path in added {
            output.extend(
                self.reduce(AnyEvent::UserIntent(FileEvent::ReloadRequested {
                    key: path,
                }))?,
            );
        }
        Ok(output)
    }

    fn apply_user_actions(&mut self, actions: Vec<FileAction<UserIntentSpec>>) -> Vec<AnyEffect> {
        let mut output = Vec::new();
        let mut changed = false;
        for action in actions {
            match action {
                FileAction::Reload(effect) => output.push(AnyEffect::ReloadUserIntent(effect)),
                FileAction::DebounceReload { key, timeout } => {
                    output.push(AnyEffect::Debounce {
                        key: format!("intent-config:reload:{key}"),
                        timeout,
                        event: AnyEvent::UserIntent(FileEvent::ReloadRequested { key }),
                    });
                }
                FileAction::ValueChanged { .. } => changed = true,
            }
        }
        if changed {
            self.rebuild_intent();
        }
        output
    }

    fn rebuild_intent(&mut self) {
        let sources: BTreeMap<_, _> = self
            .watch_list
            .iter()
            .filter_map(|path| {
                self.user_intents
                    .get(path)
                    .map(|state| (path.clone(), state.value.packages.clone()))
            })
            .collect();
        self.intent = IntentState {
            digest: digest_sources(&sources),
            sources,
        };
    }
}

fn digest_sources(sources: &BTreeMap<Utf8PathBuf, Vec<PackageSpecItem>>) -> Sha256Digest {
    let mut hasher = Sha256::new();
    hasher.update(INTENT_STATE_DIGEST_DOMAIN);
    hasher.update((sources.len() as u64).to_be_bytes());
    for (path, packages) in sources {
        let path = path.as_str().as_bytes();
        hasher.update((path.len() as u64).to_be_bytes());
        hasher.update(path);
        hasher.update((packages.len() as u64).to_be_bytes());
        for package in packages {
            hasher.update(package.digest.as_bytes());
        }
    }
    Sha256Digest::from_hasher(hasher)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FileTimestamp, FileType, Stat};

    fn stat(version: i64) -> Stat {
        Stat {
            device: 1,
            inode: version as u64,
            changed: FileTimestamp {
                seconds: version,
                nanoseconds: 0,
            },
            modified: FileTimestamp {
                seconds: version,
                nanoseconds: 0,
            },
            file_type: FileType::Regular,
            len: 1,
            mode: 0o100644,
        }
    }

    fn finish_top(state: &mut ReconcileState, version: i64, json: &[u8]) {
        state
            .reduce(AnyEvent::TopConfig(FileEvent::ReloadRequested { key: () }))
            .unwrap();
        state
            .reduce(AnyEvent::TopConfig(FileEvent::ReloadStarted { key: () }))
            .unwrap();
        state
            .reduce(AnyEvent::TopConfig(FileEvent::ReloadFinished {
                key: (),
                at: version as u64,
                stat: Some(stat(version)),
                error: None,
                data: Some(json.to_vec()),
            }))
            .unwrap();
    }

    #[test]
    fn completion_from_removed_source_is_ignored() {
        let config = Utf8PathBuf::from("/tmp/config.json");
        let source = Utf8PathBuf::from("/tmp/b.json");
        let mut state = ReconcileState::new(config);

        finish_top(&mut state, 1, br#"{"subscribe":["/tmp/b.json"]}"#);
        state
            .reduce(AnyEvent::UserIntent(FileEvent::ReloadStarted {
                key: source.clone(),
            }))
            .unwrap();
        state
            .reduce(AnyEvent::UserIntent(FileEvent::ReloadFinished {
                key: source.clone(),
                at: 1,
                stat: Some(stat(1)),
                error: None,
                data: Some(br#"{"packages":[{"items":[],"name":"old"}]}"#.to_vec()),
            }))
            .unwrap();
        assert!(state.intent.sources.contains_key(&source));

        state
            .reduce(AnyEvent::UserIntent(FileEvent::ReloadRequested {
                key: source.clone(),
            }))
            .unwrap();
        state
            .reduce(AnyEvent::UserIntent(FileEvent::ReloadStarted {
                key: source.clone(),
            }))
            .unwrap();
        finish_top(&mut state, 2, br#"{"subscribe":[]}"#);
        let digest_after_removal = state.intent.digest;

        let actions = state
            .reduce(AnyEvent::UserIntent(FileEvent::ReloadFinished {
                key: source.clone(),
                at: 2,
                stat: Some(stat(2)),
                error: None,
                data: Some(br#"{"packages":[{"items":[],"name":"late"}]}"#.to_vec()),
            }))
            .unwrap();

        assert!(actions.is_empty());
        assert!(!state.intent.sources.contains_key(&source));
        assert_eq!(state.intent.digest, digest_after_removal);
    }
}
