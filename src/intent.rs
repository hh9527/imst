use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use camino::{Utf8Component, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use url::Url;

use crate::Sha256Digest;
use crate::file::FileSpec;

const USER_INTENT_DIGEST_DOMAIN: &[u8] = b"imst:user-intent:v1";

#[derive(Debug, Error)]
pub enum ReuseUpdateError {
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("subscription path must be absolute and normalized: {0}")]
    InvalidSubscriptionPath(Utf8PathBuf),
    #[error("invalid package {package}: {message}")]
    InvalidPackage { package: String, message: String },
}

pub trait ReuseUpdate: Default + Send + 'static {
    fn reuse_update(&mut self, new_bytes: &[u8]) -> Result<bool, ReuseUpdateError>;
    fn update_digest(&mut self);
    fn digest(&self) -> &Sha256Digest;
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct TopConfigDocument {
    #[serde(default)]
    subscribe: Vec<Utf8PathBuf>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TopConfigData {
    pub subscribe: BTreeSet<Utf8PathBuf>,
    digest: Sha256Digest,
}

impl Default for TopConfigData {
    fn default() -> Self {
        let mut value = Self {
            subscribe: BTreeSet::new(),
            digest: Sha256Digest::ZERO,
        };
        value.update_digest();
        value
    }
}

impl ReuseUpdate for TopConfigData {
    fn reuse_update(&mut self, new_bytes: &[u8]) -> Result<bool, ReuseUpdateError> {
        let document: TopConfigDocument = serde_json::from_slice(new_bytes)?;
        let mut subscribe = BTreeSet::new();

        for path in document.subscribe {
            if !is_normalized_absolute(&path) {
                return Err(ReuseUpdateError::InvalidSubscriptionPath(path));
            }
            subscribe.insert(path);
        }

        if subscribe == self.subscribe {
            return Ok(false);
        }
        self.subscribe = subscribe;
        Ok(true)
    }

    fn update_digest(&mut self) {
        let mut hasher = Sha256::new();
        hasher.update(b"imst:top-config:v1");
        hasher.update((self.subscribe.len() as u64).to_be_bytes());
        for path in &self.subscribe {
            let bytes = path.as_str().as_bytes();
            hasher.update((bytes.len() as u64).to_be_bytes());
            hasher.update(bytes);
        }
        self.digest = Sha256Digest::from_hasher(hasher);
    }

    fn digest(&self) -> &Sha256Digest {
        &self.digest
    }
}

fn is_normalized_absolute(path: &Utf8PathBuf) -> bool {
    let text = path.as_str();
    path.is_absolute()
        && path
            .components()
            .all(|component| !matches!(component, Utf8Component::CurDir | Utf8Component::ParentDir))
        && text
            .split('/')
            .skip(1)
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct UserIntentDocument {
    #[serde(default)]
    packages: Vec<PackageSpec>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct UserIntentData {
    pub packages: Vec<PackageSpecItem>,
    digest: Sha256Digest,
}

impl Default for UserIntentData {
    fn default() -> Self {
        let mut value = Self {
            packages: Vec::new(),
            digest: Sha256Digest::ZERO,
        };
        value.update_digest();
        value
    }
}

impl ReuseUpdate for UserIntentData {
    fn reuse_update(&mut self, new_bytes: &[u8]) -> Result<bool, ReuseUpdateError> {
        let document: UserIntentDocument = serde_json::from_slice(new_bytes)?;
        let packages = reuse_packages(&self.packages, document.packages)?;
        if packages == self.packages {
            return Ok(false);
        }
        self.packages = packages;
        Ok(true)
    }

    fn update_digest(&mut self) {
        let mut hasher = Sha256::new();
        hasher.update(USER_INTENT_DIGEST_DOMAIN);
        hasher.update((self.packages.len() as u64).to_be_bytes());
        for package in &self.packages {
            hasher.update(package.digest.as_bytes());
        }
        self.digest = Sha256Digest::from_hasher(hasher);
    }

    fn digest(&self) -> &Sha256Digest {
        &self.digest
    }
}

fn reuse_packages(
    current: &[PackageSpecItem],
    incoming: Vec<PackageSpec>,
) -> Result<Vec<PackageSpecItem>, ReuseUpdateError> {
    let mut previous_by_digest: HashMap<Sha256Digest, Vec<&PackageSpecItem>> = HashMap::new();
    for item in current {
        previous_by_digest
            .entry(item.digest)
            .or_default()
            .push(item);
    }

    incoming
        .into_iter()
        .map(|package| {
            package.validate()?;
            let digest = package.digest()?;
            if let Some(previous) = previous_by_digest.get(&digest).and_then(|candidates| {
                candidates
                    .iter()
                    .find(|candidate| candidate.spec.as_ref() == &package)
            }) {
                return Ok((*previous).clone());
            }
            Ok(PackageSpecItem {
                spec: Arc::new(package),
                digest,
            })
        })
        .collect()
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PackageSpecItem {
    pub spec: Arc<PackageSpec>,
    pub digest: Sha256Digest,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackageSpec {
    pub items: Vec<ItemSpec>,
    pub name: String,
}

impl PackageSpec {
    fn validate(&self) -> Result<(), ReuseUpdateError> {
        if self.name.is_empty() {
            return Err(ReuseUpdateError::InvalidPackage {
                package: self.name.clone(),
                message: "name must not be empty".into(),
            });
        }
        Ok(())
    }

    fn digest(&self) -> Result<Sha256Digest, ReuseUpdateError> {
        Ok(Sha256Digest::digest(serde_json::to_vec(self)?))
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ItemSpec {
    pub dest: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digest: Option<ItemDigest>,
    pub kind: ItemKind,
    pub src: Url,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ItemDigest {
    pub sha256: String,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type")]
pub enum ItemKind {
    Archive {
        format: ArchiveFormat,
        strip_components: u32,
    },
    BinaryFile,
    RegularFile,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub enum ArchiveFormat {
    TarGzip,
}

#[derive(Debug)]
pub struct TopConfigSpec;

impl FileSpec for TopConfigSpec {
    type Data = TopConfigData;
    type Key = ();
}

#[derive(Debug)]
pub struct UserIntentSpec;

impl FileSpec for UserIntentSpec {
    type Data = UserIntentData;
    type Key = Utf8PathBuf;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intent(name: &str) -> Vec<u8> {
        format!(r#"{{"packages":[{{"items":[],"name":"{name}"}}]}}"#).into_bytes()
    }

    #[test]
    fn failed_update_is_transactional() {
        let mut value = UserIntentData::default();
        assert!(value.reuse_update(&intent("foo")).unwrap());
        value.update_digest();
        let previous = value.clone();

        assert!(value.reuse_update(br#"{"packages": [}"#).is_err());
        assert_eq!(value, previous);
    }

    #[test]
    fn unchanged_packages_reuse_arc() {
        let mut value = UserIntentData::default();
        assert!(value.reuse_update(&intent("foo")).unwrap());
        value.update_digest();
        let package = Arc::clone(&value.packages[0].spec);

        assert!(!value.reuse_update(&intent("foo")).unwrap());
        assert!(Arc::ptr_eq(&package, &value.packages[0].spec));
    }

    #[test]
    fn changed_document_reuses_unchanged_package_arc() {
        let mut value = UserIntentData::default();
        value
            .reuse_update(br#"{"packages":[{"items":[],"name":"foo"}]}"#)
            .unwrap();
        value.update_digest();
        let foo = Arc::clone(&value.packages[0].spec);

        assert!(
            value
                .reuse_update(
                    br#"{"packages":[{"items":[],"name":"foo"},{"items":[],"name":"bar"}]}"#,
                )
                .unwrap()
        );
        value.update_digest();

        assert!(Arc::ptr_eq(&foo, &value.packages[0].spec));
        assert_eq!(value.packages.len(), 2);
    }

    #[test]
    fn package_order_changes_user_intent_digest() {
        let mut first = UserIntentData::default();
        first
            .reuse_update(br#"{"packages":[{"items":[],"name":"foo"},{"items":[],"name":"bar"}]}"#)
            .unwrap();
        first.update_digest();

        let mut second = UserIntentData::default();
        second
            .reuse_update(br#"{"packages":[{"items":[],"name":"bar"},{"items":[],"name":"foo"}]}"#)
            .unwrap();
        second.update_digest();

        assert_ne!(first.digest(), second.digest());
    }

    #[test]
    fn top_config_rejects_non_normalized_path_without_mutation() {
        let mut value = TopConfigData::default();
        let previous = value.clone();
        assert!(
            value
                .reuse_update(br#"{"subscribe":["/tmp/../intent.json"]}"#)
                .is_err()
        );
        assert_eq!(value, previous);
    }

    #[test]
    fn top_config_subscription_order_is_not_semantic() {
        let mut value = TopConfigData::default();
        assert!(
            value
                .reuse_update(br#"{"subscribe":["/tmp/a.json","/tmp/b.json"]}"#)
                .unwrap()
        );
        value.update_digest();
        let digest = *value.digest();

        assert!(
            !value
                .reuse_update(br#"{"subscribe":["/tmp/b.json","/tmp/a.json"]}"#)
                .unwrap()
        );
        assert_eq!(value.digest(), &digest);
    }
}
