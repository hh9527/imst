use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use camino::Utf8PathBuf;
use thiserror::Error;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::{AbortHandle, JoinError, JoinSet};

use crate::file::{FileEvent, FileStateError};
use crate::intent::{TopConfigSpec, UserIntentSpec};
use crate::state::{AnyEffect, AnyEvent, ReconcileState, StateSnapshot};
use crate::watcher::{InotifyFileWatcher, WatchError};

#[derive(Debug, Clone)]
pub struct RuntimeOptions {
    pub watch_debounce: Duration,
}

impl Default for RuntimeOptions {
    fn default() -> Self {
        Self {
            watch_debounce: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error(transparent)]
    Watch(#[from] WatchError),
    #[error(transparent)]
    State(#[from] FileStateError),
    #[error("runtime task failed: {0}")]
    Join(#[from] JoinError),
    #[error("runtime event channel closed")]
    EventChannelClosed,
}

#[derive(Debug)]
pub struct Reconciler {
    snapshots: watch::Receiver<StateSnapshot>,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<Result<(), RuntimeError>>>,
}

impl Reconciler {
    pub async fn start(
        config_path: Utf8PathBuf,
        options: RuntimeOptions,
    ) -> Result<Self, RuntimeError> {
        let top_watcher = InotifyFileWatcher::new(&config_path)?;
        let state = ReconcileState::new(config_path);
        let initial = state.snapshot();
        let (snapshot_tx, snapshots) = watch::channel(initial);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let task = tokio::spawn(run(state, top_watcher, options, snapshot_tx, shutdown_rx));

        Ok(Self {
            snapshots,
            shutdown: Some(shutdown_tx),
            task: Some(task),
        })
    }

    pub fn snapshot(&self) -> StateSnapshot {
        self.snapshots.borrow().clone()
    }

    pub async fn changed(&mut self) -> Result<StateSnapshot, RuntimeError> {
        let task = self.task.as_mut().expect("runtime task is present");
        tokio::select! {
            changed = self.snapshots.changed() => {
                changed.map_err(|_| RuntimeError::EventChannelClosed)?;
                Ok(self.snapshots.borrow_and_update().clone())
            }
            result = task => {
                self.task.take();
                match result {
                    Ok(Ok(())) => Err(RuntimeError::EventChannelClosed),
                    Ok(Err(error)) => Err(error),
                    Err(error) => Err(error.into()),
                }
            }
        }
    }

    pub async fn shutdown(mut self) -> Result<(), RuntimeError> {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        self.task.take().expect("runtime task is present").await??;
        Ok(())
    }
}

impl Drop for Reconciler {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

#[derive(Debug, Clone, Eq, Ord, PartialEq, PartialOrd)]
enum WatchTarget {
    TopConfig,
    UserIntent(Utf8PathBuf),
}

#[derive(Debug)]
enum Message {
    Event(AnyEvent),
    WatchChanged(WatchTarget),
}

async fn run(
    mut state: ReconcileState,
    top_watcher: InotifyFileWatcher,
    options: RuntimeOptions,
    snapshot_tx: watch::Sender<StateSnapshot>,
    mut shutdown: oneshot::Receiver<()>,
) -> Result<(), RuntimeError> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut effects = JoinSet::new();
    let mut services = JoinSet::new();
    let mut service_handles = BTreeMap::new();
    let mut debounce_handles: BTreeMap<String, AbortHandle> = BTreeMap::new();

    let handle = spawn_watcher(
        &mut services,
        tx.clone(),
        WatchTarget::TopConfig,
        top_watcher,
    );
    service_handles.insert(WatchTarget::TopConfig, handle);
    tx.send(Message::Event(AnyEvent::TopConfig(
        FileEvent::ReloadRequested { key: () },
    )))
    .map_err(|_| RuntimeError::EventChannelClosed)?;

    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            message = rx.recv() => {
                let message = message.ok_or(RuntimeError::EventChannelClosed)?;
                let actions = match message {
                    Message::Event(event) => state.reduce(event)?,
                    Message::WatchChanged(target) => {
                        let (key, event) = match target {
                            WatchTarget::TopConfig => (
                                "config:reload".to_owned(),
                                AnyEvent::TopConfig(FileEvent::ReloadRequested { key: () }),
                            ),
                            WatchTarget::UserIntent(path) => (
                                format!("intent-config:reload:{path}"),
                                AnyEvent::UserIntent(FileEvent::ReloadRequested { key: path }),
                            ),
                        };
                        vec![AnyEffect::Debounce {
                            key,
                            timeout: options.watch_debounce,
                            event,
                        }]
                    }
                };
                apply_actions(
                    actions,
                    &tx,
                    &mut effects,
                    &mut services,
                    &mut service_handles,
                    &mut debounce_handles,
                )?;
                snapshot_tx.send_replace(state.snapshot());
            }
            result = effects.join_next(), if !effects.is_empty() => {
                if let Err(error) = result.expect("guarded by is_empty")
                    && !error.is_cancelled()
                {
                    return Err(error.into());
                }
            }
            result = services.join_next(), if !services.is_empty() => {
                match result.expect("guarded by is_empty") {
                    Ok(Ok(())) => return Err(RuntimeError::EventChannelClosed),
                    Ok(Err(error)) => return Err(error.into()),
                    Err(error) if error.is_cancelled() => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
    }

    effects.abort_all();
    services.abort_all();
    Ok(())
}

fn apply_actions(
    actions: Vec<AnyEffect>,
    tx: &mpsc::UnboundedSender<Message>,
    effects: &mut JoinSet<()>,
    services: &mut JoinSet<Result<(), WatchError>>,
    service_handles: &mut BTreeMap<WatchTarget, AbortHandle>,
    debounce_handles: &mut BTreeMap<String, AbortHandle>,
) -> Result<(), RuntimeError> {
    for action in actions {
        match action {
            AnyEffect::ReloadTopConfig(effect) => {
                let tx = tx.clone();
                effects.spawn(async move {
                    let _ = tx.send(Message::Event(AnyEvent::TopConfig(FileEvent::<
                        TopConfigSpec,
                    >::ReloadStarted {
                        key: (),
                    })));
                    let _ = tx.send(Message::Event(AnyEvent::TopConfig(
                        effect.apply(now_millis()).await,
                    )));
                });
            }
            AnyEffect::ReloadUserIntent(effect) => {
                ensure_user_watcher(&effect.path, tx, services, service_handles);
                let tx = tx.clone();
                let key = effect.key.clone();
                effects.spawn(async move {
                    let _ = tx.send(Message::Event(AnyEvent::UserIntent(FileEvent::<
                        UserIntentSpec,
                    >::ReloadStarted {
                        key,
                    })));
                    let _ = tx.send(Message::Event(AnyEvent::UserIntent(
                        effect.apply(now_millis()).await,
                    )));
                });
            }
            AnyEffect::Debounce {
                key,
                timeout,
                event,
            } => {
                if let Some(previous) = debounce_handles.remove(&key) {
                    previous.abort();
                }
                let tx = tx.clone();
                let handle = effects.spawn(async move {
                    tokio::time::sleep(timeout).await;
                    let _ = tx.send(Message::Event(event));
                });
                debounce_handles.insert(key, handle);
            }
            AnyEffect::ReconcileWatches { added, removed } => {
                for path in removed {
                    if let Some(handle) =
                        service_handles.remove(&WatchTarget::UserIntent(path.clone()))
                    {
                        handle.abort();
                    }
                    if let Some(handle) =
                        debounce_handles.remove(&format!("intent-config:reload:{path}"))
                    {
                        handle.abort();
                    }
                }
                for path in added {
                    ensure_user_watcher(&path, tx, services, service_handles);
                }
            }
        }
    }
    Ok(())
}

fn ensure_user_watcher(
    path: &Utf8PathBuf,
    tx: &mpsc::UnboundedSender<Message>,
    services: &mut JoinSet<Result<(), WatchError>>,
    service_handles: &mut BTreeMap<WatchTarget, AbortHandle>,
) {
    let target = WatchTarget::UserIntent(path.clone());
    if service_handles.contains_key(&target) {
        return;
    }
    match InotifyFileWatcher::new(path) {
        Ok(watcher) => {
            let handle = spawn_watcher(services, tx.clone(), target.clone(), watcher);
            service_handles.insert(target, handle);
        }
        Err(error) => {
            log::warn!("failed to watch {path}; fallback reload will retry: {error}");
        }
    }
}

fn spawn_watcher(
    services: &mut JoinSet<Result<(), WatchError>>,
    tx: mpsc::UnboundedSender<Message>,
    target: WatchTarget,
    mut watcher: InotifyFileWatcher,
) -> AbortHandle {
    services.spawn(async move {
        loop {
            watcher.next_change().await?;
            if tx.send(Message::WatchChanged(target.clone())).is_err() {
                return Ok(());
            }
        }
    })
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}
