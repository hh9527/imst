use std::ffi::{OsStr, OsString};
use std::io;

use camino::{Utf8Path, Utf8PathBuf};
use futures_util::StreamExt as _;
use inotify::{EventMask, EventStream, Inotify, WatchMask};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum WatchError {
    #[error("watch target has no parent: {0}")]
    MissingParent(Utf8PathBuf),
    #[error("watch target has no file name: {0}")]
    MissingFileName(Utf8PathBuf),
    #[error("inotify failed: {0}")]
    Inotify(#[from] io::Error),
    #[error("inotify event stream ended")]
    StreamEnded,
}

#[derive(Debug)]
pub struct InotifyFileWatcher {
    stream: EventStream<Vec<u8>>,
    target_name: OsString,
}

impl InotifyFileWatcher {
    pub fn new(target: &Utf8Path) -> Result<Self, WatchError> {
        let parent = target
            .parent()
            .ok_or_else(|| WatchError::MissingParent(target.to_owned()))?;
        let target_name = target
            .file_name()
            .ok_or_else(|| WatchError::MissingFileName(target.to_owned()))?;

        let inotify = Inotify::init()?;
        inotify.watches().add(
            parent,
            WatchMask::MOVED_TO
                | WatchMask::MOVED_FROM
                | WatchMask::DELETE
                | WatchMask::CLOSE_WRITE,
        )?;
        let stream = inotify.into_event_stream(vec![0; 4096])?;

        Ok(Self {
            stream,
            target_name: OsString::from(target_name),
        })
    }

    pub async fn next_change(&mut self) -> Result<(), WatchError> {
        while let Some(event) = self.stream.next().await {
            let event = event?;
            if is_target_change(event.name.as_deref(), event.mask, &self.target_name) {
                return Ok(());
            }
        }
        Err(WatchError::StreamEnded)
    }
}

fn is_target_change(name: Option<&OsStr>, mask: EventMask, target_name: &OsStr) -> bool {
    name == Some(target_name)
        && mask.intersects(
            EventMask::MOVED_TO
                | EventMask::MOVED_FROM
                | EventMask::DELETE
                | EventMask::CLOSE_WRITE,
        )
}
