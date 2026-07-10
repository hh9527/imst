use std::fmt::Debug;
use std::time::Duration;

use camino::{Utf8Path, Utf8PathBuf};
use thiserror::Error;

use crate::intent::{ReuseUpdate, ReuseUpdateError};

pub trait FileSpec: Send + 'static {
    type Key: Clone + Debug + Eq + Send + 'static;
    type Data: ReuseUpdate;
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum LoaderStage {
    Idle,
    Submitted,
    Working,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct FileTimestamp {
    pub seconds: i64,
    pub nanoseconds: u32,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum FileType {
    Regular,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Stat {
    pub device: u64,
    pub inode: u64,
    pub changed: FileTimestamp,
    pub modified: FileTimestamp,
    pub file_type: FileType,
    pub len: u64,
    pub mode: u32,
}

#[derive(Debug, Error)]
pub enum FileLoadError {
    #[error("open failed: {0}")]
    Open(String),
    #[error("stat failed: {0}")]
    Stat(String),
    #[error("path is not a regular file")]
    NotRegularFile,
    #[error("read failed: {0}")]
    Read(String),
    #[error(transparent)]
    InvalidData(#[from] ReuseUpdateError),
}

#[derive(Debug)]
pub struct FileLoadErrorState {
    pub at: u64,
    pub stat: Option<Stat>,
    pub error: FileLoadError,
}

#[derive(Debug)]
pub struct FileState<S: FileSpec> {
    pub stat: Option<Stat>,
    pub stage: LoaderStage,
    pub invalidated: bool,
    pub value: S::Data,
    pub error: Option<FileLoadErrorState>,
}

impl<S: FileSpec> Default for FileState<S> {
    fn default() -> Self {
        Self {
            stat: None,
            stage: LoaderStage::Idle,
            invalidated: false,
            value: S::Data::default(),
            error: None,
        }
    }
}

#[derive(Debug)]
pub enum FileEvent<S: FileSpec> {
    ReloadRequested {
        key: S::Key,
    },
    ReloadStarted {
        key: S::Key,
    },
    ReloadFinished {
        key: S::Key,
        at: u64,
        stat: Option<Stat>,
        error: Option<FileLoadError>,
        data: Option<Vec<u8>>,
    },
}

#[derive(Debug)]
pub struct FileEffect<S: FileSpec> {
    pub key: S::Key,
    pub path: Utf8PathBuf,
    pub prev_stat: Option<Stat>,
}

#[derive(Debug)]
pub enum FileAction<S: FileSpec> {
    Reload(FileEffect<S>),
    DebounceReload { key: S::Key, timeout: Duration },
    ValueChanged { key: S::Key },
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum FileStateError {
    #[error("event key does not identify this file state")]
    WrongKey,
    #[error("ReloadStarted requires Submitted, found {0:?}")]
    UnexpectedStart(LoaderStage),
    #[error("ReloadFinished requires Working, found {0:?}")]
    UnexpectedFinish(LoaderStage),
    #[error("ReloadFinished has an invalid result shape")]
    InvalidFinishedResult,
}

impl<S: FileSpec> FileState<S> {
    pub fn reduce(
        &mut self,
        expected_key: &S::Key,
        path: &Utf8Path,
        event: FileEvent<S>,
    ) -> Result<Vec<FileAction<S>>, FileStateError> {
        match event {
            FileEvent::ReloadRequested { key } => {
                ensure_key(expected_key, &key)?;
                if self.stage == LoaderStage::Idle {
                    let prev_stat = self.stat.take();
                    self.stage = LoaderStage::Submitted;
                    self.invalidated = false;
                    Ok(vec![FileAction::Reload(FileEffect {
                        key,
                        path: path.to_owned(),
                        prev_stat,
                    })])
                } else {
                    self.invalidated = true;
                    Ok(Vec::new())
                }
            }
            FileEvent::ReloadStarted { key } => {
                ensure_key(expected_key, &key)?;
                if self.stage != LoaderStage::Submitted {
                    return Err(FileStateError::UnexpectedStart(self.stage));
                }
                self.stage = LoaderStage::Working;
                Ok(Vec::new())
            }
            FileEvent::ReloadFinished {
                key,
                at,
                stat,
                error,
                data,
            } => {
                ensure_key(expected_key, &key)?;
                if self.stage != LoaderStage::Working {
                    return Err(FileStateError::UnexpectedFinish(self.stage));
                }
                validate_finished(stat, error.as_ref(), data.as_ref())?;

                let timeout = if self.invalidated {
                    Duration::from_secs(1)
                } else {
                    Duration::from_secs(30)
                };
                self.stage = LoaderStage::Idle;
                self.invalidated = false;

                let mut actions = Vec::new();
                match (stat, error, data) {
                    (Some(stat), None, Some(data)) => match self.value.reuse_update(&data) {
                        Ok(changed) => {
                            if changed {
                                let previous_digest = *self.value.digest();
                                self.value.update_digest();
                                if self.value.digest() != &previous_digest {
                                    actions.push(FileAction::ValueChanged { key: key.clone() });
                                }
                            }
                            self.stat = Some(stat);
                            self.error = None;
                        }
                        Err(error) => {
                            self.stat = Some(stat);
                            self.error = Some(FileLoadErrorState {
                                at,
                                stat: Some(stat),
                                error: error.into(),
                            });
                        }
                    },
                    (Some(stat), None, None) => {
                        self.stat = Some(stat);
                    }
                    (stat, Some(error), None) => {
                        self.stat = stat;
                        self.error = Some(FileLoadErrorState { at, stat, error });
                    }
                    _ => unreachable!("validated above"),
                }

                actions.push(FileAction::DebounceReload { key, timeout });
                Ok(actions)
            }
        }
    }
}

fn ensure_key<K: Eq>(expected: &K, actual: &K) -> Result<(), FileStateError> {
    if expected == actual {
        Ok(())
    } else {
        Err(FileStateError::WrongKey)
    }
}

fn validate_finished(
    stat: Option<Stat>,
    error: Option<&FileLoadError>,
    data: Option<&Vec<u8>>,
) -> Result<(), FileStateError> {
    let valid = matches!(
        (stat, error, data),
        (Some(_), None, None)
            | (None, Some(_), None)
            | (Some(_), Some(_), None)
            | (Some(_), None, Some(_))
    );
    valid
        .then_some(())
        .ok_or(FileStateError::InvalidFinishedResult)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use camino::Utf8Path;

    use super::*;
    use crate::{ReuseUpdate, ReuseUpdateError, Sha256Digest, UserIntentSpec};

    #[derive(Debug, Default)]
    struct StableDigestData;

    impl ReuseUpdate for StableDigestData {
        fn reuse_update(&mut self, _new_bytes: &[u8]) -> Result<bool, ReuseUpdateError> {
            Ok(true)
        }

        fn update_digest(&mut self) {}

        fn digest(&self) -> &Sha256Digest {
            &Sha256Digest::ZERO
        }
    }

    #[derive(Debug)]
    struct StableDigestSpec;

    impl FileSpec for StableDigestSpec {
        type Key = ();
        type Data = StableDigestData;
    }

    fn path() -> &'static Utf8Path {
        Utf8Path::new("/tmp/intent.json")
    }

    fn stat(seconds: i64) -> Stat {
        Stat {
            device: 1,
            inode: seconds as u64,
            changed: FileTimestamp {
                seconds,
                nanoseconds: 0,
            },
            modified: FileTimestamp {
                seconds,
                nanoseconds: 0,
            },
            file_type: FileType::Regular,
            len: 10,
            mode: 0o100644,
        }
    }

    #[test]
    fn atomic_replacement_changes_stat_identity() {
        let old = stat(1);
        let mut replacement = old;
        replacement.inode += 1;

        assert_ne!(old, replacement);
    }

    fn start(state: &mut FileState<UserIntentSpec>) -> Vec<FileAction<UserIntentSpec>> {
        let key = path().to_owned();
        let actions = state
            .reduce(
                &key,
                path(),
                FileEvent::ReloadRequested { key: key.clone() },
            )
            .unwrap();
        state
            .reduce(&key, path(), FileEvent::ReloadStarted { key: key.clone() })
            .unwrap();
        actions
    }

    #[test]
    fn reload_moves_idle_submitted_working() {
        let mut state = FileState::<UserIntentSpec> {
            stat: Some(stat(1)),
            ..Default::default()
        };
        let actions = start(&mut state);
        assert_eq!(state.stage, LoaderStage::Working);
        assert_eq!(state.stat, None);
        assert!(matches!(
            actions.as_slice(),
            [FileAction::Reload(FileEffect {
                prev_stat: Some(previous),
                ..
            })] if *previous == stat(1)
        ));
    }

    #[test]
    fn busy_request_latches_invalidation_and_completion_uses_one_second() {
        let mut state = FileState::<UserIntentSpec>::default();
        start(&mut state);
        let key = path().to_owned();
        state
            .reduce(
                &key,
                path(),
                FileEvent::ReloadRequested { key: key.clone() },
            )
            .unwrap();
        assert!(state.invalidated);

        let actions = state
            .reduce(
                &key,
                path(),
                FileEvent::ReloadFinished {
                    key: key.clone(),
                    at: 1,
                    stat: Some(stat(1)),
                    error: None,
                    data: Some(br#"{"packages":[]}"#.to_vec()),
                },
            )
            .unwrap();

        assert_eq!(state.stage, LoaderStage::Idle);
        assert!(!state.invalidated);
        assert!(matches!(
            actions.last(),
            Some(FileAction::DebounceReload { timeout, .. })
                if *timeout == Duration::from_secs(1)
        ));
    }

    #[test]
    fn invalid_data_advances_stat_but_preserves_last_known_good_value() {
        let mut state = FileState::<UserIntentSpec>::default();
        state
            .value
            .reuse_update(br#"{"packages":[{"items":[],"name":"foo"}]}"#)
            .unwrap();
        state.value.update_digest();
        state.stat = Some(stat(1));
        let package = Arc::clone(&state.value.packages[0].spec);
        let digest = *state.value.digest();
        start(&mut state);
        let key = path().to_owned();

        state
            .reduce(
                &key,
                path(),
                FileEvent::ReloadFinished {
                    key: key.clone(),
                    at: 2,
                    stat: Some(stat(2)),
                    error: None,
                    data: Some(br#"{"packages": [}"#.to_vec()),
                },
            )
            .unwrap();

        assert_eq!(state.stat, Some(stat(2)));
        assert_eq!(state.value.digest(), &digest);
        assert!(Arc::ptr_eq(&package, &state.value.packages[0].spec));
        assert!(state.error.is_some());
    }

    #[test]
    fn semantic_change_emits_value_changed_and_uses_fallback_timeout() {
        let mut state = FileState::<UserIntentSpec>::default();
        start(&mut state);
        let key = path().to_owned();
        let actions = state
            .reduce(
                &key,
                path(),
                FileEvent::ReloadFinished {
                    key: key.clone(),
                    at: 1,
                    stat: Some(stat(1)),
                    error: None,
                    data: Some(br#"{"packages":[{"items":[],"name":"foo"}]}"#.to_vec()),
                },
            )
            .unwrap();

        assert!(matches!(
            actions.first(),
            Some(FileAction::ValueChanged { .. })
        ));
        assert!(matches!(
            actions.last(),
            Some(FileAction::DebounceReload { timeout, .. })
                if *timeout == Duration::from_secs(30)
        ));
    }

    #[test]
    fn unchanged_data_advances_stat_without_value_changed() {
        let mut state = FileState::<UserIntentSpec>::default();
        state
            .value
            .reuse_update(br#"{"packages":[{"items":[],"name":"foo"}]}"#)
            .unwrap();
        state.value.update_digest();
        state.stat = Some(stat(1));
        start(&mut state);
        let key = path().to_owned();

        let actions = state
            .reduce(
                &key,
                path(),
                FileEvent::ReloadFinished {
                    key: key.clone(),
                    at: 2,
                    stat: Some(stat(2)),
                    error: None,
                    data: Some(br#"{"packages":[{"items":[],"name":"foo"}]}"#.to_vec()),
                },
            )
            .unwrap();

        assert_eq!(state.stat, Some(stat(2)));
        assert!(state.error.is_none());
        assert!(
            !actions
                .iter()
                .any(|action| matches!(action, FileAction::ValueChanged { .. }))
        );
    }

    #[test]
    fn reload_takes_stat_even_when_previous_load_failed() {
        let mut state = FileState::<UserIntentSpec> {
            stat: Some(stat(1)),
            error: Some(FileLoadErrorState {
                at: 1,
                stat: Some(stat(1)),
                error: FileLoadError::Read("temporarily unavailable".into()),
            }),
            ..Default::default()
        };
        let actions = start(&mut state);

        assert!(matches!(
            actions.as_slice(),
            [FileAction::Reload(FileEffect {
                prev_stat: Some(previous),
                ..
            })] if *previous == stat(1)
        ));
        assert_eq!(state.stat, None);
    }

    #[test]
    fn short_circuit_restores_stat_and_retains_load_error() {
        let mut state = FileState::<UserIntentSpec> {
            stat: Some(stat(1)),
            error: Some(FileLoadErrorState {
                at: 1,
                stat: Some(stat(1)),
                error: FileLoadError::Read("temporarily unavailable".into()),
            }),
            ..Default::default()
        };
        start(&mut state);
        let key = path().to_owned();

        state
            .reduce(
                &key,
                path(),
                FileEvent::ReloadFinished {
                    key: key.clone(),
                    at: 2,
                    stat: Some(stat(1)),
                    error: None,
                    data: None,
                },
            )
            .unwrap();

        assert_eq!(state.stat, Some(stat(1)));
        assert!(state.error.is_some());
    }

    #[test]
    fn read_error_advances_successful_stat() {
        let mut state = FileState::<UserIntentSpec> {
            stat: Some(stat(1)),
            ..Default::default()
        };
        start(&mut state);
        let key = path().to_owned();

        state
            .reduce(
                &key,
                path(),
                FileEvent::ReloadFinished {
                    key: key.clone(),
                    at: 2,
                    stat: Some(stat(2)),
                    error: Some(FileLoadError::Read("temporarily unavailable".into())),
                    data: None,
                },
            )
            .unwrap();

        assert_eq!(state.stat, Some(stat(2)));
        assert!(state.error.is_some());
    }

    #[test]
    fn stat_error_replaces_previous_stat_with_none() {
        let mut state = FileState::<UserIntentSpec> {
            stat: Some(stat(1)),
            ..Default::default()
        };
        start(&mut state);
        let key = path().to_owned();

        state
            .reduce(
                &key,
                path(),
                FileEvent::ReloadFinished {
                    key: key.clone(),
                    at: 2,
                    stat: None,
                    error: Some(FileLoadError::Stat("temporarily unavailable".into())),
                    data: None,
                },
            )
            .unwrap();

        assert_eq!(state.stat, None);
        assert!(state.error.is_some());
    }

    #[test]
    fn unchanged_digest_does_not_emit_value_changed() {
        let mut state = FileState::<StableDigestSpec>::default();
        let actions = state
            .reduce(&(), path(), FileEvent::ReloadRequested { key: () })
            .unwrap();
        assert!(matches!(actions.as_slice(), [FileAction::Reload(_)]));
        state
            .reduce(&(), path(), FileEvent::ReloadStarted { key: () })
            .unwrap();

        let actions = state
            .reduce(
                &(),
                path(),
                FileEvent::ReloadFinished {
                    key: (),
                    at: 1,
                    stat: Some(stat(1)),
                    error: None,
                    data: Some(Vec::new()),
                },
            )
            .unwrap();

        assert!(
            !actions
                .iter()
                .any(|action| matches!(action, FileAction::ValueChanged { .. }))
        );
    }
}
