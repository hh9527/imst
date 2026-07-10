use std::fs::Metadata;
use std::os::unix::fs::MetadataExt as _;

use tokio::fs::File;
use tokio::io::AsyncReadExt as _;

use crate::file::{FileEffect, FileEvent, FileLoadError, FileSpec, FileTimestamp, FileType, Stat};

impl<S: FileSpec> FileEffect<S> {
    pub async fn apply(self, at: u64) -> FileEvent<S> {
        let Self {
            key,
            path,
            prev_stat,
        } = self;

        let mut file = match File::open(&path).await {
            Ok(file) => file,
            Err(error) => {
                return FileEvent::ReloadFinished {
                    key,
                    at,
                    stat: None,
                    error: Some(FileLoadError::Open(error.to_string())),
                    data: None,
                };
            }
        };

        let metadata = match file.metadata().await {
            Ok(metadata) => metadata,
            Err(error) => {
                return FileEvent::ReloadFinished {
                    key,
                    at,
                    stat: None,
                    error: Some(FileLoadError::Stat(error.to_string())),
                    data: None,
                };
            }
        };
        let stat = metadata_to_stat(&metadata);

        if prev_stat == Some(stat) {
            return FileEvent::ReloadFinished {
                key,
                at,
                stat: Some(stat),
                error: None,
                data: None,
            };
        }

        if stat.file_type != FileType::Regular {
            return FileEvent::ReloadFinished {
                key,
                at,
                stat: Some(stat),
                error: Some(FileLoadError::NotRegularFile),
                data: None,
            };
        }

        let mut data = Vec::with_capacity(stat.len.try_into().unwrap_or(0));
        match file.read_to_end(&mut data).await {
            Ok(_) => FileEvent::ReloadFinished {
                key,
                at,
                stat: Some(stat),
                error: None,
                data: Some(data),
            },
            Err(error) => FileEvent::ReloadFinished {
                key,
                at,
                stat: Some(stat),
                error: Some(FileLoadError::Read(error.to_string())),
                data: None,
            },
        }
    }
}

fn metadata_to_stat(metadata: &Metadata) -> Stat {
    Stat {
        device: metadata.dev(),
        inode: metadata.ino(),
        changed: FileTimestamp {
            seconds: metadata.ctime(),
            nanoseconds: metadata.ctime_nsec() as u32,
        },
        modified: FileTimestamp {
            seconds: metadata.mtime(),
            nanoseconds: metadata.mtime_nsec() as u32,
        },
        file_type: if metadata.is_file() {
            FileType::Regular
        } else if metadata.is_dir() {
            FileType::Directory
        } else if metadata.file_type().is_symlink() {
            FileType::Symlink
        } else {
            FileType::Other
        },
        len: metadata.len(),
        mode: metadata.mode(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use camino::Utf8PathBuf;
    use tempfile::tempdir;

    use super::*;
    use crate::UserIntentSpec;

    #[tokio::test]
    async fn loader_short_circuits_the_same_open_file_stat() {
        let dir = tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().join("intent.json")).unwrap();
        fs::write(&path, br#"{"packages":[]}"#).unwrap();

        let first = FileEffect::<UserIntentSpec> {
            key: path.clone(),
            path: path.clone(),
            prev_stat: None,
        }
        .apply(1)
        .await;
        let stat = match first {
            FileEvent::ReloadFinished {
                stat: Some(stat),
                data: Some(_),
                ..
            } => stat,
            _ => panic!("expected loaded file"),
        };

        let second = FileEffect::<UserIntentSpec> {
            key: path.clone(),
            path,
            prev_stat: Some(stat),
        }
        .apply(2)
        .await;
        assert!(matches!(
            second,
            FileEvent::ReloadFinished {
                stat: Some(current),
                data: None,
                error: None,
                ..
            } if current == stat
        ));
    }
}
