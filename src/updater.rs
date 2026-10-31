//! Updates switch executable/data pairs. A trial never writes the last working data.
#[cfg(windows)]
mod http;
#[cfg(windows)]
pub mod process;
#[cfg(windows)]
pub mod runtime;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

pub const ASSET_NAME: &str = "multi-point-clicker-windows-x86_64.exe";
pub const REPOSITORY: &str = "mashfromband/Automatic-Position-Based-Clicker";
pub const MAX_BINARY: usize = 32 * 1024 * 1024;

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub fn unique_id() -> String {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        std::process::id(),
        COUNT.fetch_add(1, Ordering::Relaxed)
    )
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct Version(pub u32, pub u32, pub u32);

pub fn epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub fn startup_grace(confirmed_at: u64, now: u64) -> bool {
    confirmed_at > 0 && now >= confirmed_at && now - confirmed_at <= 60
}

impl Version {
    pub fn parse(text: &str) -> io::Result<Self> {
        let parts: Vec<_> = text.split('.').collect();
        if parts.len() != 3
            || parts.iter().any(|p| {
                p.is_empty()
                    || !p.bytes().all(|c| c.is_ascii_digit())
                    || (p.len() > 1 && p.starts_with('0'))
            })
        {
            return Err(invalid("Invalid release version"));
        }
        let parse = |s: &str| {
            s.parse::<u32>()
                .map_err(|_| invalid("Release version overflow"))
        };
        Ok(Self(parse(parts[0])?, parse(parts[1])?, parse(parts[2])?))
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Binary {
    pub version: Version,
    pub sha256: String,
    pub protocol: u32,
}

impl Binary {
    fn validate(&self) -> io::Result<()> {
        if self.sha256.len() != 64
            || !self.sha256.bytes().all(|c| c.is_ascii_hexdigit())
            || self.protocol > 1
        {
            return Err(invalid("Invalid installed executable"));
        }
        Ok(())
    }

    pub fn id(&self) -> String {
        format!("{}-{}", self.version, &self.sha256[..16])
    }

    pub fn path(&self, root: &Path) -> PathBuf {
        root.join("versions")
            .join(self.id())
            .join("multi-point-clicker.exe")
    }

    fn verify(&self, root: &Path) -> io::Result<()> {
        self.validate()?;
        let length = fs::metadata(self.path(root))?.len();
        if length == 0 || length > MAX_BINARY as u64 {
            return Err(invalid("Invalid installed executable size"));
        }
        let bytes = fs::read(self.path(root))?;
        if digest(&bytes) != self.sha256 {
            return Err(invalid("Installed executable checksum mismatch"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Installation {
    pub binary: Binary,
    pub data: String,
    #[serde(default)]
    pub settings_expected: bool,
}

impl Installation {
    pub fn data_root(&self, root: &Path) -> PathBuf {
        root.join("data").join(&self.data)
    }

    fn validate(&self) -> io::Result<()> {
        self.binary.validate()?;
        if self.data.is_empty()
            || self.data.len() > 100
            || !self
                .data
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        {
            return Err(invalid("Invalid application data identifier"));
        }
        Ok(())
    }

    fn verify(&self, root: &Path) -> io::Result<()> {
        self.binary.verify(root)?;
        let metadata = fs::symlink_metadata(self.data_root(root))?;
        if !metadata.is_dir() || is_link(&metadata) {
            return Err(invalid("Installed application data is missing"));
        }
        let app_data = self.data_root(root).join("MultiPointClicker");
        let metadata = fs::symlink_metadata(&app_data)?;
        if !metadata.is_dir()
            || is_link(&metadata)
            || (self.settings_expected && !app_data.join("settings.json").is_file())
        {
            return Err(invalid("Previously saved application settings are missing"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Pending {
    pub trial: Installation,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct State {
    pub format: u32,
    pub current: Installation,
    pub previous: Option<Installation>,
    pub staged: Option<Binary>,
    pub pending: Option<Pending>,
    pub rejected: Option<String>,
    #[serde(default)]
    pub confirmed_at: u64,
    #[serde(default)]
    pub timeout_candidate: Option<String>,
    #[serde(default)]
    pub timeout_failures: u32,
}

impl State {
    pub fn load(root: &Path) -> io::Result<Self> {
        let read = |path| fs::read(path).and_then(|bytes| Self::decode(&bytes));
        read(root.join("state.json"))
            .or_else(|original| read(root.join("state-backup.json")).map_err(|_| original))
    }

    fn decode(bytes: &[u8]) -> io::Result<Self> {
        let state: Self = serde_json::from_slice(bytes).map_err(|e| invalid(&e.to_string()))?;
        if state.format != 1 {
            return Err(invalid("Unsupported updater state"));
        }
        state.current.validate()?;
        if let Some(pair) = &state.previous {
            pair.validate()?;
        }
        if let Some(binary) = &state.staged {
            binary.validate()?;
        }
        if let Some(pending) = &state.pending {
            pending.trial.validate()?;
        }
        Ok(state)
    }

    pub fn save(&self, root: &Path) -> io::Result<()> {
        if let Ok(previous) = Self::load(root) {
            atomic_json(&root.join("state-backup.json"), &previous)?;
        }
        atomic_json(&root.join("state.json"), self)
    }
}

#[derive(Serialize, Deserialize)]
pub struct Bootstrap {
    pub seed: Binary,
    pub previous: Option<Binary>,
}

/// First-run import happens only after no application instance is running.
pub fn initialize(root: &Path, legacy_data: &Path) -> io::Result<State> {
    let setup: Bootstrap = serde_json::from_slice(&fs::read(root.join("bootstrap.json"))?)
        .map_err(|e| invalid(&e.to_string()))?;
    setup.seed.verify(root)?;
    if let Some(previous) = &setup.previous {
        previous.verify(root)?;
    }
    let current = Installation {
        binary: setup.previous.clone().unwrap_or_else(|| setup.seed.clone()),
        data: unique_id(),
        settings_expected: legacy_data.join("settings.json").is_file(),
    };
    let data_path = current.data_root(root).join("MultiPointClicker");
    copy_data(legacy_data, &data_path)?;
    let state = State {
        format: 1,
        current,
        previous: None,
        staged: setup.previous.map(|_| setup.seed),
        pending: None,
        rejected: None,
        confirmed_at: 0,
        timeout_candidate: None,
        timeout_failures: 0,
    };
    state.save(root)?;
    Ok(state)
}

pub fn stage_binary(root: &Path, binary: &Binary, bytes: &[u8]) -> io::Result<()> {
    binary.validate()?;
    if bytes.is_empty() || bytes.len() > MAX_BINARY || digest(bytes) != binary.sha256 {
        return Err(invalid("Downloaded executable checksum or size mismatch"));
    }
    let destination = binary.path(root);
    if destination.exists() && binary.verify(root).is_ok() {
        return atomic_json(&destination.with_file_name("binary.json"), binary);
    }
    fs::create_dir_all(
        destination
            .parent()
            .ok_or_else(|| invalid("Missing executable parent"))?,
    )?;
    atomic_bytes(&destination, bytes)?;
    atomic_json(&destination.with_file_name("binary.json"), binary)
}

/// Only remove our two known binary files; retain all application data and unknown files.
pub fn clean_versions(root: &Path, state: &State) {
    let mut keep = std::collections::BTreeSet::new();
    let mut add = |state: &State| {
        keep.insert(state.current.binary.id());
        if let Some(pair) = &state.previous {
            keep.insert(pair.binary.id());
        }
        if let Some(binary) = &state.staged {
            keep.insert(binary.id());
        }
        if let Some(pending) = &state.pending {
            keep.insert(pending.trial.binary.id());
        }
    };
    add(state);
    if let Ok(bytes) = fs::read(root.join("state-backup.json")) {
        if let Ok(backup) = State::decode(&bytes) {
            add(&backup);
        }
    }
    if let Ok(bytes) = fs::read(root.join("bootstrap.json")) {
        if let Ok(setup) = serde_json::from_slice::<Bootstrap>(&bytes) {
            if setup.seed.validate().is_ok() {
                keep.insert(setup.seed.id());
            }
            if let Some(previous) = setup.previous.filter(|b| b.validate().is_ok()) {
                keep.insert(previous.id());
            }
        }
    }
    let Ok(entries) = fs::read_dir(root.join("versions")) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if keep.contains(&name) {
            continue;
        }
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !metadata.is_dir() || is_link(&metadata) {
            continue;
        }
        let marker = path.join("binary.json");
        let Ok(metadata) = fs::symlink_metadata(&marker) else {
            continue;
        };
        if is_link(&metadata) {
            continue;
        }
        let Ok(bytes) = fs::read(&marker) else {
            continue;
        };
        let Ok(binary) = serde_json::from_slice::<Binary>(&bytes) else {
            continue;
        };
        if binary.validate().is_err() || binary.id() != name {
            continue;
        }
        if fs::remove_file(path.join("multi-point-clicker.exe")).is_err() {
            continue;
        }
        let _ = fs::remove_file(marker);
        let _ = fs::remove_dir(path);
    }
}

pub trait AppRuntime {
    type Session;
    /// A failure must leave no trial process running or writing application data.
    fn start(&mut self, pair: &Installation, root: &Path) -> io::Result<Self::Session>;
    fn stop(&mut self, session: Self::Session) -> io::Result<()>;
}

#[derive(Debug)]
pub struct CleanupUnconfirmed(pub io::Error);
impl std::fmt::Display for CleanupUnconfirmed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Application termination is unconfirmed: {}", self.0)
    }
}
impl std::error::Error for CleanupUnconfirmed {}
pub fn cleanup_error(error: io::Error) -> io::Error {
    io::Error::other(CleanupUnconfirmed(error))
}
fn is_cleanup_error(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|e| e.is::<CleanupUnconfirmed>())
}

/// Recover first, so a terminated launcher cannot repeatedly try the same broken update.
pub fn recover(root: &Path, state: &mut State) -> io::Result<bool> {
    let Some(pending) = state.pending.take() else {
        return Ok(false);
    };
    state.rejected = Some(pending.trial.binary.sha256);
    state.staged = None;
    state.save(root)?;
    Ok(true)
}

pub fn start<R: AppRuntime>(
    root: &Path,
    state: &mut State,
    runtime: &mut R,
) -> io::Result<R::Session> {
    if let Some(binary) = state.staged.clone().filter(|b| {
        b.version > state.current.binary.version && state.rejected.as_ref() != Some(&b.sha256)
    }) {
        let mut attempted_startup = false;
        let attempt = (|| {
            binary.verify(root)?;
            state.current.verify(root)?;
            let mut trial = Installation {
                binary,
                data: unique_id(),
                settings_expected: state.current.settings_expected,
            };
            copy_data(&state.current.data_root(root), &trial.data_root(root))?;
            state.pending = Some(Pending {
                trial: trial.clone(),
            });
            state.save(root)?;
            attempted_startup = true;
            let session = runtime.start(&trial, root)?;
            trial.settings_expected |= trial
                .data_root(root)
                .join("MultiPointClicker/settings.json")
                .is_file();
            if let Err(error) = trial.verify(root) {
                runtime.stop(session).map_err(cleanup_error)?;
                return Err(error);
            }
            let mut committed = state.clone();
            committed.previous = Some(state.current.clone());
            committed.current = trial;
            committed.staged = None;
            committed.pending = None;
            committed.confirmed_at = epoch_seconds();
            committed.timeout_candidate = None;
            committed.timeout_failures = 0;
            if let Err(error) = committed.save(root) {
                runtime.stop(session).map_err(cleanup_error)?;
                return Err(error);
            }
            *state = committed;
            Ok(session)
        })();
        match attempt {
            Ok(session) => return Ok(session),
            Err(error) if is_cleanup_error(&error) => return Err(error),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                state.pending = None;
                state.save(root)?;
                return Err(error);
            }
            Err(error) => {
                log(root, "rolled_back", &error.to_string());
                if attempted_startup {
                    let hash = state.staged.as_ref().map(|b| b.sha256.clone());
                    let retry_timeout = error.kind() == io::ErrorKind::TimedOut
                        && (state.timeout_candidate != hash || state.timeout_failures == 0);
                    if retry_timeout {
                        state.timeout_candidate = hash;
                        state.timeout_failures = 1;
                    } else {
                        state.rejected = hash;
                        state.staged = None;
                        state.timeout_failures = 2;
                    }
                }
                state.pending = None;
                // The saved current still names the old pair if this write fails.
                if let Err(error) = state.save(root) {
                    log(root, "state_write_failed", &error.to_string());
                }
            }
        }
    }
    let current_result = state
        .current
        .verify(root)
        .and_then(|_| runtime.start(&state.current, root));
    match current_result {
        Ok(session) => Ok(session),
        Err(error) if is_cleanup_error(&error) => Err(error),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Err(error),
        Err(error) => {
            let Some(previous) = state.previous.clone() else {
                return Err(error);
            };
            log(root, "rolled_back", &error.to_string());
            previous.verify(root)?;
            let session = runtime.start(&previous, root)?;
            let mut restored = state.clone();
            restored.rejected = Some(state.current.binary.sha256.clone());
            restored.current = previous;
            restored.previous = None;
            restored.staged = None;
            restored.pending = None;
            restored.confirmed_at = 0;
            if let Err(error) = restored.save(root) {
                // Never leave an untracked process alive if restoring the selector fails.
                runtime.stop(session).map_err(cleanup_error)?;
                return Err(error);
            }
            *state = restored;
            Ok(session)
        }
    }
}

pub fn atomic_json(path: &Path, value: &impl Serialize) -> io::Result<()> {
    atomic_bytes(
        path,
        &serde_json::to_vec_pretty(value).map_err(|e| invalid(&e.to_string()))?,
    )
}

fn atomic_bytes(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid("Missing destination parent"))?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".{}.tmp", unique_id()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            use windows_sys::Win32::Storage::FileSystem::{
                MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
            };
            let wide = |p: &Path| {
                p.as_os_str()
                    .encode_wide()
                    .chain(Some(0))
                    .collect::<Vec<_>>()
            };
            if unsafe {
                MoveFileExW(
                    wide(&temporary).as_ptr(),
                    wide(path).as_ptr(),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        #[cfg(not(windows))]
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    metadata.file_type().is_symlink()
}

fn copy_data(source: &Path, destination: &Path) -> io::Result<()> {
    let metadata = match fs::symlink_metadata(source) {
        Ok(value) => value,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(destination)?;
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    if is_link(&metadata) || !metadata.is_dir() {
        return Err(invalid("Application data is not a regular directory"));
    }
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if is_link(&metadata) {
            return Err(invalid("Application data contains a link"));
        }
        let target = destination.join(entry.file_name());
        if metadata.is_dir() {
            copy_data(&entry.path(), &target)?;
        } else if metadata.is_file() {
            fs::copy(entry.path(), &target)?;
            OpenOptions::new().write(true).open(target)?.sync_all()?;
        } else {
            return Err(invalid("Unsupported application data entry"));
        }
    }
    Ok(())
}

pub fn log(root: &Path, status: &str, detail: &str) {
    let _ = atomic_json(
        &root.join("update-status.json"),
        &serde_json::json!({"status":status,"detail":detail}),
    );
}

#[derive(Deserialize)]
pub struct Release {
    pub tag_name: String,
    pub draft: bool,
    pub prerelease: bool,
    pub assets: Vec<ReleaseAsset>,
}

#[derive(Deserialize)]
pub struct ReleaseAsset {
    pub name: String,
    pub size: usize,
    pub digest: Option<String>,
    pub browser_download_url: String,
}

impl Release {
    pub fn candidate(&self) -> io::Result<(Binary, &ReleaseAsset)> {
        if self.draft || self.prerelease {
            return Err(invalid("Not a stable release"));
        }
        let version = Version::parse(
            self.tag_name
                .strip_prefix('v')
                .ok_or_else(|| invalid("Invalid release tag"))?,
        )?;
        let asset = self
            .assets
            .iter()
            .find(|a| a.name == ASSET_NAME)
            .ok_or_else(|| invalid("Windows release asset missing"))?;
        let sha256 = asset
            .digest
            .as_deref()
            .and_then(|s| s.strip_prefix("sha256:"))
            .ok_or_else(|| invalid("Release checksum missing"))?
            .to_ascii_lowercase();
        let binary = Binary {
            version,
            sha256,
            protocol: 1,
        };
        binary.validate()?;
        if asset.size == 0
            || asset.size > MAX_BINARY
            || asset.browser_download_url
                != format!(
                    "https://github.com/{REPOSITORY}/releases/download/{}/{ASSET_NAME}",
                    self.tag_name
                )
        {
            return Err(invalid("Invalid release asset location or size"));
        }
        Ok((binary, asset))
    }
}

#[cfg(test)]
mod tests;
