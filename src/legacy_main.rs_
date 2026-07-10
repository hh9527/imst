use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use anyhow::Context as _;
use camino::{Utf8Path, Utf8PathBuf};
use chrono::{DateTime, Utc};
use clap::Parser;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tempfile::NamedTempFile;
use thiserror::Error;
use url::Url;

#[derive(Debug, Parser)]
struct Cli {
    #[arg(long)]
    requests: PathBuf,
    #[arg(long)]
    store: Utf8PathBuf,
    #[arg(long, default_value_t = true)]
    dry_run: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();

    let cli = Cli::parse();
    let input = fs::read_to_string(&cli.requests)
        .with_context(|| format!("failed to read {}", cli.requests.display()))?;
    let request_set: RequestSet = serde_json::from_str(&input)
        .with_context(|| format!("failed to parse {}", cli.requests.display()))?;

    let ctx = ActionContext::new(cli.store, SystemClock, cli.dry_run);
    reconcile(&ctx, &request_set).await?;

    Ok(())
}

async fn reconcile<C: Clock>(
    ctx: &ActionContext<C>,
    request_set: &RequestSet,
) -> Result<(), ImstError> {
    for spec in &request_set.packages {
        spec.validate()?;
        let pkg_id = spec.pkg_id()?;

        if (CheckInstalled { pkg_id: &pkg_id }).apply(ctx).await? {
            continue;
        }

        ensure_downloads(ctx, &spec.items).await?;

        for item in &spec.items {
            match &item.kind {
                ItemKind::Archive {
                    format,
                    strip_components,
                } => {
                    UnpackArchive {
                        format,
                        source: item.src.clone(),
                        strip_components: *strip_components,
                        sink: pkg_id.clone(),
                        target: item.dest.clone(),
                    }
                    .apply(ctx)
                    .await?;
                }
                ItemKind::BinaryFile => {
                    LinkBinaryFile {
                        source: item.src.clone(),
                        sink: pkg_id.clone(),
                        target: item.dest.clone(),
                    }
                    .apply(ctx)
                    .await?;
                }
                ItemKind::RegularFile => {
                    LinkFile {
                        source: item.src.clone(),
                        sink: pkg_id.clone(),
                        target: item.dest.clone(),
                    }
                    .apply(ctx)
                    .await?;
                }
            }
        }

        FinishInstall { pkg_id }.apply(ctx).await?;
    }

    Ok(())
}

async fn ensure_downloads<C: Clock>(
    ctx: &ActionContext<C>,
    items: &[ItemSpec],
) -> Result<(), ImstError> {
    let mut tasks = tokio::task::JoinSet::new();
    let mut seen = HashSet::new();

    for item in items {
        let job = DownloadJob::new(ctx, item);
        if !seen.insert(job.src.clone()) {
            continue;
        }
        tasks.spawn(async move { job.apply().await });
    }

    while let Some(result) = tasks.join_next().await {
        result.map_err(ImstError::Join)??;
    }

    Ok(())
}

#[derive(Debug, Deserialize, Serialize)]
struct RequestSet {
    packages: Vec<PackageSpec>,
}

#[derive(Debug, Deserialize, Serialize)]
struct PackageSpec {
    // Canonical contract: fields are declared in alphabetical order.
    items: Vec<ItemSpec>,
    name: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ItemSpec {
    // Canonical contract: fields are declared in alphabetical order.
    dest: String,
    // Canonical contract: absent optional fields are omitted from canonical JSON.
    #[serde(skip_serializing_if = "Option::is_none")]
    digest: Option<ItemDigest>,
    kind: ItemKind,
    src: Url,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ItemDigest {
    sha256: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type")]
enum ItemKind {
    Archive {
        format: ArchiveFormat,
        strip_components: u32,
    },
    BinaryFile,
    RegularFile,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
enum ArchiveFormat {
    TarGzip,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct PkgId {
    name: String,
    rev: String,
}

#[derive(Debug, Clone, Copy)]
enum VPath<'a> {
    Dl(&'a Url),
    DlTmp(&'a Url),
    Pkg(&'a PkgId),
    PkgTmp(&'a PkgId),
}

impl VPath<'_> {
    fn to_utf8_path(&self, store_root: &Utf8Path) -> Utf8PathBuf {
        match self {
            Self::Dl(url) => store_root.join("dl").join(download_hash(url)),
            Self::DlTmp(url) => store_root
                .join("dl")
                .join(format!(".tmp.{}", download_hash(url))),
            Self::Pkg(pkg_id) => store_root
                .join("installed")
                .join(&pkg_id.name)
                .join(&pkg_id.rev),
            Self::PkgTmp(pkg_id) => store_root
                .join("installed")
                .join(&pkg_id.name)
                .join(format!(".tmp.{}", pkg_id.rev)),
        }
    }
}

impl fmt::Display for VPath<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dl(url) => write!(f, "dl:{url}"),
            Self::DlTmp(url) => write!(f, "dl-tmp:{url}"),
            Self::Pkg(pkg_id) => {
                write!(f, "pkg:{}@{}", pkg_id.name, short_hash(&pkg_id.rev))
            }
            Self::PkgTmp(pkg_id) => {
                write!(f, "pkg-tmp:{}@{}", pkg_id.name, short_hash(&pkg_id.rev))
            }
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct InstalledMarker {
    installed_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct DownloadInput<'a> {
    // Canonical contract: fields are declared in alphabetical order.
    src: &'a Url,
}

struct DownloadJob {
    digest: Option<ItemDigest>,
    dry_run: bool,
    src: Url,
    store_root: Utf8PathBuf,
}

impl DownloadJob {
    fn new<C: Clock>(ctx: &ActionContext<C>, item: &ItemSpec) -> Self {
        Self {
            digest: item.digest.clone(),
            dry_run: ctx.dry_run,
            src: item.src.clone(),
            store_root: ctx.store_root.clone(),
        }
    }

    async fn apply(self) -> Result<(), ImstError> {
        let ctx = ActionContext::new(self.store_root, SystemClock, self.dry_run);
        if (CheckDownloaded { src: &self.src }).apply(&ctx).await? {
            return Ok(());
        }

        DownloadFile {
            digest: self.digest.as_ref(),
            src: &self.src,
        }
        .apply(&ctx)
        .await?;

        FinishDownload { src: self.src }.apply(&ctx).await
    }
}

trait Clock {
    fn now(&self) -> DateTime<Utc>;
}

#[derive(Clone, Copy)]
struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

struct ActionContext<C> {
    store_root: Utf8PathBuf,
    clock: C,
    dry_run: bool,
}

impl<C> ActionContext<C> {
    fn new(store_root: Utf8PathBuf, clock: C, dry_run: bool) -> Self {
        Self {
            store_root,
            clock,
            dry_run,
        }
    }
}

struct CheckDownloaded<'a> {
    src: &'a Url,
}

impl CheckDownloaded<'_> {
    async fn apply<C: Clock>(&self, ctx: &ActionContext<C>) -> Result<bool, ImstError> {
        let path = VPath::Dl(self.src);
        log::info!("checking \"{}\"", path);
        Ok(path.to_utf8_path(&ctx.store_root).exists())
    }
}

struct CheckInstalled<'a> {
    pkg_id: &'a PkgId,
}

impl CheckInstalled<'_> {
    async fn apply<C: Clock>(&self, ctx: &ActionContext<C>) -> Result<bool, ImstError> {
        log::info!(
            "checking \"{}\"",
            display_vpath_with_rel(VPath::Pkg(self.pkg_id), Some(".imst.json"))
        );
        Ok(VPath::Pkg(self.pkg_id)
            .to_utf8_path(&ctx.store_root)
            .join(".imst.json")
            .exists())
    }
}

struct DownloadFile<'a> {
    digest: Option<&'a ItemDigest>,
    src: &'a Url,
}

impl DownloadFile<'_> {
    async fn apply<C: Clock>(&self, ctx: &ActionContext<C>) -> Result<(), ImstError> {
        if !ctx.dry_run {
            return Err(ImstError::UnsupportedRealInstall);
        }

        if let Some(digest) = self.digest {
            log::info!(
                "fetching \"{}\" digest=\"sha256:{}\"",
                self.src,
                short_hash(&digest.sha256)
            );
        } else {
            log::info!("fetching \"{}\"", self.src);
        }
        let temp_path = VPath::DlTmp(self.src).to_utf8_path(&ctx.store_root);
        write_download_tmp(&temp_path)?;
        Ok(())
    }
}

struct FinishDownload {
    src: Url,
}

impl FinishDownload {
    async fn apply<C: Clock>(&self, ctx: &ActionContext<C>) -> Result<(), ImstError> {
        let source = VPath::DlTmp(&self.src);
        let sink = VPath::Dl(&self.src);
        let tmp_path = source.to_utf8_path(&ctx.store_root);
        let final_path = sink.to_utf8_path(&ctx.store_root);
        if final_path.exists() {
            return Ok(());
        }
        log::info!("finishing-download \"{}\"", sink);
        finish_download_file(&tmp_path, &final_path)?;
        Ok(())
    }
}

struct UnpackArchive<'a> {
    format: &'a ArchiveFormat,
    source: Url,
    strip_components: u32,
    sink: PkgId,
    target: String,
}

impl UnpackArchive<'_> {
    async fn apply<C: Clock>(&self, ctx: &ActionContext<C>) -> Result<(), ImstError> {
        log::info!(
            "unpacking \"{}\" \"{}\" format={:?} strip_components={}",
            VPath::Dl(&self.source),
            display_vpath_with_rel(VPath::PkgTmp(&self.sink), Some(&self.target)),
            self.format,
            self.strip_components
        );

        if !ctx.dry_run {
            return Err(ImstError::UnsupportedRealInstall);
        }

        Ok(())
    }
}

struct LinkBinaryFile {
    source: Url,
    sink: PkgId,
    target: String,
}

impl LinkBinaryFile {
    async fn apply<C: Clock>(&self, ctx: &ActionContext<C>) -> Result<(), ImstError> {
        log::info!(
            "copying \"{}\" \"{}\" mode=755",
            VPath::Dl(&self.source),
            display_vpath_with_rel(VPath::PkgTmp(&self.sink), Some(&self.target))
        );

        if !ctx.dry_run {
            return Err(ImstError::UnsupportedRealInstall);
        }

        Ok(())
    }
}

struct LinkFile {
    source: Url,
    sink: PkgId,
    target: String,
}

impl LinkFile {
    async fn apply<C: Clock>(&self, ctx: &ActionContext<C>) -> Result<(), ImstError> {
        log::info!(
            "linking \"{}\" \"{}\" mode=644",
            VPath::Dl(&self.source),
            display_vpath_with_rel(VPath::PkgTmp(&self.sink), Some(&self.target))
        );

        if !ctx.dry_run {
            return Err(ImstError::UnsupportedRealInstall);
        }

        Ok(())
    }
}

struct FinishInstall {
    pkg_id: PkgId,
}

impl FinishInstall {
    async fn apply<C: Clock>(&self, ctx: &ActionContext<C>) -> Result<(), ImstError> {
        let source = VPath::PkgTmp(&self.pkg_id);
        let sink = VPath::Pkg(&self.pkg_id);
        let tmp_root = source.to_utf8_path(&ctx.store_root);
        let final_root = sink.to_utf8_path(&ctx.store_root);
        if final_root.exists() {
            return Ok(());
        }
        if tmp_root.exists() {
            fs::remove_dir_all(&tmp_root)?;
        }
        fs::create_dir_all(&tmp_root)?;

        let marker_path = tmp_root.join(".imst.json");
        let marker = InstalledMarker {
            installed_at: ctx.clock.now(),
        };
        log::info!(
            "installing-marker \"{}\"",
            display_vpath_with_rel(source, Some(".imst.json"))
        );
        write_marker_atomic(&marker_path, &marker)?;

        log::info!("finishing-install \"{}\"", sink);
        fs::rename(&tmp_root, &final_root)?;
        Ok(())
    }
}

impl PackageSpec {
    fn validate(&self) -> Result<(), ImstError> {
        validate_name(&self.name)?;
        for item in &self.items {
            item.validate()?;
        }
        Ok(())
    }

    fn pkg_id(&self) -> Result<PkgId, ImstError> {
        Ok(PkgId {
            name: self.name.clone(),
            rev: self.rev()?,
        })
    }

    fn rev(&self) -> Result<String, ImstError> {
        let bytes = serde_json::to_vec(self)?;
        let digest = Sha256::digest(bytes);
        Ok(hex_lower(&digest))
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn short_hash(value: &str) -> &str {
    value.get(..8).unwrap_or(value)
}

fn download_hash(url: &Url) -> String {
    let input = DownloadInput { src: url };
    let bytes = serde_json::to_vec(&input).expect("DownloadInput serialization must succeed");
    let digest = Sha256::digest(bytes);
    hex_lower(&digest)
}

fn display_vpath_with_rel(path: VPath<'_>, rel: Option<&str>) -> String {
    match rel {
        Some(rel) => format!("{path}:{rel}"),
        None => path.to_string(),
    }
}

impl ItemSpec {
    fn validate(&self) -> Result<(), ImstError> {
        validate_src(&self.src)?;
        if let Some(digest) = &self.digest {
            validate_sha256_hex(&digest.sha256)?;
        }
        validate_dest(&self.dest, matches!(self.kind, ItemKind::Archive { .. }))?;
        Ok(())
    }
}

fn validate_name(name: &str) -> Result<(), ImstError> {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return Err(ImstError::InvalidName(name.to_owned()));
    };
    if !first.is_ascii_lowercase() {
        return Err(ImstError::InvalidName(name.to_owned()));
    }

    let mut prev_sep = false;
    let mut last = first;
    for ch in chars {
        let is_valid =
            ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '_' | '-' | '.');
        if !is_valid {
            return Err(ImstError::InvalidName(name.to_owned()));
        }

        let is_sep = matches!(ch, '_' | '-' | '.');
        if prev_sep && is_sep {
            return Err(ImstError::InvalidName(name.to_owned()));
        }
        prev_sep = is_sep;
        last = ch;
    }

    if matches!(last, '_' | '-' | '.') {
        return Err(ImstError::InvalidName(name.to_owned()));
    }

    Ok(())
}

fn validate_src(src: &Url) -> Result<(), ImstError> {
    match src.scheme() {
        "http" | "https" => Ok(()),
        _ => Err(ImstError::InvalidSrc(src.to_string())),
    }
}

fn validate_sha256_hex(value: &str) -> Result<(), ImstError> {
    if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ImstError::InvalidDigest(value.to_owned()));
    }
    Ok(())
}

fn validate_dest(dest: &str, allow_empty: bool) -> Result<(), ImstError> {
    if dest.is_empty() {
        return if allow_empty {
            Ok(())
        } else {
            Err(ImstError::InvalidDest(dest.to_owned()))
        };
    }

    if dest == "/" || dest.starts_with('/') {
        return Err(ImstError::InvalidDest(dest.to_owned()));
    }

    for segment in dest.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(ImstError::InvalidDest(dest.to_owned()));
        }
    }

    let path = Utf8Path::new(dest);
    if path.as_str() != dest {
        return Err(ImstError::InvalidDest(dest.to_owned()));
    }

    Ok(())
}

#[cfg(test)]
fn marker_ready<C>(ctx: &ActionContext<C>, pkg_id: &PkgId) -> bool {
    let marker_path = VPath::Pkg(pkg_id)
        .to_utf8_path(&ctx.store_root)
        .join(".imst.json");
    marker_path.exists()
}

fn write_marker_atomic<T: Serialize>(marker_path: &Utf8Path, marker: &T) -> Result<(), ImstError> {
    let parent = marker_path
        .parent()
        .ok_or_else(|| ImstError::InvalidMarkerPath(marker_path.to_owned()))?;
    fs::create_dir_all(parent)?;
    let mut tmp = NamedTempFile::new_in(parent.as_std_path())?;
    serde_json::to_writer(&mut tmp, marker)?;
    tmp.write_all(b"\n")?;
    tmp.flush()?;
    tmp.as_file().sync_all()?;
    tmp.persist(marker_path.as_std_path())
        .map_err(|err| ImstError::PersistMarker(err.error))?;
    Ok(())
}

fn write_download_tmp(tmp_path: &Utf8Path) -> Result<(), ImstError> {
    let parent = tmp_path
        .parent()
        .ok_or_else(|| ImstError::InvalidMarkerPath(tmp_path.to_owned()))?;
    fs::create_dir_all(parent)?;
    if tmp_path.exists() {
        fs::remove_file(tmp_path)?;
    }
    fs::write(tmp_path, b"dry-run download cache\n")?;
    fs::set_permissions(tmp_path, fs::Permissions::from_mode(0o644))?;
    Ok(())
}

fn finish_download_file(tmp_path: &Utf8Path, final_path: &Utf8Path) -> Result<(), ImstError> {
    let parent = final_path
        .parent()
        .ok_or_else(|| ImstError::InvalidMarkerPath(final_path.to_owned()))?;
    fs::create_dir_all(parent)?;
    if final_path.exists() {
        return Ok(());
    }
    fs::rename(tmp_path, final_path)?;
    Ok(())
}

#[derive(Debug, Error)]
enum ImstError {
    #[error("invalid package name: {0}")]
    InvalidName(String),
    #[error("invalid src URI: {0}")]
    InvalidSrc(String),
    #[error("invalid sha256 digest: {0}")]
    InvalidDigest(String),
    #[error("invalid dest: {0}")]
    InvalidDest(String),
    #[error("invalid marker path: {0}")]
    InvalidMarkerPath(Utf8PathBuf),
    #[error("real install is not supported by RFC 0001")]
    UnsupportedRealInstall,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Join(#[from] tokio::task::JoinError),
    #[error("failed to persist marker: {0}")]
    PersistMarker(std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[derive(Clone, Copy)]
    struct FixedClock;

    impl Clock for FixedClock {
        fn now(&self) -> DateTime<Utc> {
            Utc.with_ymd_and_hms(2026, 7, 8, 12, 0, 0).unwrap()
        }
    }

    fn parse_request(input: &str) -> RequestSet {
        serde_json::from_str(input).unwrap()
    }

    fn sample_request() -> RequestSet {
        parse_request(
            r#"
            {
              "packages": [
                {
                  "name": "node",
                  "items": [
                    {
                      "src": "https://example.invalid/node-v20.tar.gz",
                      "digest": {
                        "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                      },
                      "dest": "",
                      "kind": {
                        "type": "Archive",
                        "format": "TarGzip",
                        "strip_components": 1
                      }
                    }
                  ]
                },
                {
                  "name": "node",
                  "items": [
                    {
                      "src": "https://example.invalid/node-v22.tar.gz",
                      "digest": {
                        "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                      },
                      "dest": "",
                      "kind": {
                        "type": "Archive",
                        "format": "TarGzip",
                        "strip_components": 1
                      }
                    }
                  ]
                }
              ]
            }
            "#,
        )
    }

    #[test]
    fn rev_is_stable_and_distinguishes_specs() {
        let requests = sample_request();
        let first = requests.packages[0].rev().unwrap();
        let first_again = requests.packages[0].rev().unwrap();
        let second = requests.packages[1].rev().unwrap();

        assert_eq!(first, first_again);
        assert_ne!(first, second);
    }

    #[test]
    fn same_items_under_different_names_have_different_revs() {
        let left = parse_request(
            r#"
            {
              "packages": [
                {
                  "name": "node",
                  "items": [
                    {
                      "src": "https://example.invalid/tool.tar.gz",
                      "digest": {
                        "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                      },
                      "dest": "",
                      "kind": {
                        "type": "Archive",
                        "format": "TarGzip",
                        "strip_components": 1
                      }
                    }
                  ]
                }
              ]
            }
            "#,
        );
        let right = parse_request(
            r#"
            {
              "packages": [
                {
                  "name": "python",
                  "items": [
                    {
                      "src": "https://example.invalid/tool.tar.gz",
                      "digest": {
                        "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                      },
                      "dest": "",
                      "kind": {
                        "type": "Archive",
                        "format": "TarGzip",
                        "strip_components": 1
                      }
                    }
                  ]
                }
              ]
            }
            "#,
        );

        assert_ne!(
            left.packages[0].rev().unwrap(),
            right.packages[0].rev().unwrap()
        );
    }

    #[tokio::test]
    async fn marker_is_written_and_second_run_skips() {
        let temp = tempfile::tempdir().unwrap();
        let store = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).unwrap();
        let ctx = ActionContext::new(store, FixedClock, true);
        let requests = sample_request();

        reconcile(&ctx, &requests).await.unwrap();

        let identities: Vec<_> = requests
            .packages
            .iter()
            .map(|spec| spec.pkg_id().unwrap())
            .collect();
        for identity in &identities {
            let final_root = VPath::Pkg(identity).to_utf8_path(&ctx.store_root);
            let tmp_root = VPath::PkgTmp(identity).to_utf8_path(&ctx.store_root);
            let marker_path = final_root.join(".imst.json");
            assert!(marker_path.exists());
            assert!(!tmp_root.exists());
            let marker: InstalledMarker =
                serde_json::from_str(&fs::read_to_string(marker_path).unwrap()).unwrap();
            assert_eq!(
                marker.installed_at,
                Utc.with_ymd_and_hms(2026, 7, 8, 12, 0, 0).unwrap()
            );
        }
        for item in requests.packages.iter().flat_map(|spec| &spec.items) {
            let cache_path = VPath::Dl(&item.src).to_utf8_path(&ctx.store_root);
            let temp_path = VPath::DlTmp(&item.src).to_utf8_path(&ctx.store_root);
            assert!(cache_path.exists());
            assert!(!temp_path.exists());
        }

        reconcile(&ctx, &requests).await.unwrap();
        for identity in &identities {
            assert!(marker_ready(&ctx, identity));
        }
    }

    #[test]
    fn invalid_names_are_rejected() {
        for name in ["Node", "node-", "node_", "node.", "node--x", "node_.x"] {
            assert!(validate_name(name).is_err(), "{name} should be invalid");
        }
        for name in ["node", "node20", "node-20", "node_20", "node.20"] {
            assert!(validate_name(name).is_ok(), "{name} should be valid");
        }
    }

    #[test]
    fn dest_rules_follow_kind() {
        assert!(validate_dest("", true).is_ok());
        assert!(validate_dest("", false).is_err());
        for dest in ["/", "/a", "a/./b", "a/../b", "a//b"] {
            assert!(
                validate_dest(dest, true).is_err(),
                "{dest} should be invalid"
            );
        }
        assert!(validate_dest("abc/xyz", false).is_ok());
    }

    #[test]
    fn uri_scheme_is_limited() {
        assert!(validate_src(&Url::parse("https://example.invalid/a").unwrap()).is_ok());
        assert!(validate_src(&Url::parse("http://example.invalid/a").unwrap()).is_ok());
        assert!(validate_src(&Url::parse("file:///tmp/a").unwrap()).is_err());
    }
}
