use crate::error::{ClickerError, ErrorCode};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

pub const MIN_INTERVAL_TICKS: u64 = 1;
pub const TICKS_PER_SECOND: u64 = 10_000_000;
pub const TICKS_PER_MILLISECOND: u64 = 10_000;
pub const MAX_INTERVAL_TICKS: u64 = 3600 * TICKS_PER_SECOND;
pub const DEFAULT_INTERVAL_TICKS: u64 = 5_010_000;
pub const MAX_POINTS: usize = 10;
pub const ACTIONS: [&str; 4] = ["record_start", "record_stop", "click_start", "click_stop"];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClickMode {
    #[default]
    Recorded,
    Area,
    Pointer,
    Drag,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClickArea {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}
impl ClickArea {
    pub fn from_corners(a: (i32, i32), b: (i32, i32)) -> Self {
        Self {
            left: a.0.min(b.0),
            top: a.1.min(b.1),
            right: a.0.max(b.0),
            bottom: a.1.max(b.1),
        }
    }
    pub fn grid_size(&self, step: u32) -> Option<(u64, u64)> {
        if step == 0 || self.left > self.right || self.top > self.bottom {
            return None;
        }
        let axis = |a: i32, b: i32| {
            let span = (i64::from(b) - i64::from(a)) as u64;
            span.div_ceil(step as u64) + 1
        };
        let size = (axis(self.left, self.right), axis(self.top, self.bottom));
        size.0.checked_mul(size.1)?;
        Some(size)
    }
    /// Serpentine grid; every coordinate is visited once, including all four edges.
    pub fn point_at(&self, step: u32, index: u64) -> Option<Point> {
        let (columns, rows) = self.grid_size(step)?;
        let index = index % (columns * rows);
        let row = index / columns;
        let column = if row % 2 == 0 {
            index % columns
        } else {
            columns - 1 - index % columns
        };
        let x = (i64::from(self.left) + (column * step as u64) as i64).min(i64::from(self.right))
            as i32;
        let y =
            (i64::from(self.top) + (row * step as u64) as i64).min(i64::from(self.bottom)) as i32;
        Some(Point {
            x,
            y,
            interval_ticks: None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Point {
    pub x: i32,
    pub y: i32,
    #[serde(default)]
    pub interval_ticks: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub version: u32,
    pub lang: String,
    pub theme: String,
    pub points: Vec<Point>,
    pub interval_ticks: u64,
    pub repeat: u32,
    pub delay_ms: u64,
    pub button: String,
    pub topmost: bool,
    pub hotkeys: BTreeMap<String, String>,
    pub per_point: bool,
    pub details_open: bool,
    pub mode: ClickMode,
    pub area: Option<ClickArea>,
    pub area_step_px: u32,
    pub fast_interval_ticks: u64,
    pub key_clicks: crate::key_click::KeyClickSettings,
}

#[derive(Deserialize)]
#[serde(default)]
struct LegacySettings {
    lang: String,
    theme: String,
    points: Vec<(i32, i32)>,
    interval: f64,
    repeat: u32,
    delay: u64,
    button: String,
    topmost: bool,
    hotkeys: BTreeMap<String, String>,
}
impl Default for LegacySettings {
    fn default() -> Self {
        Self {
            lang: "ja".into(),
            theme: "system".into(),
            points: vec![],
            interval: 1.,
            repeat: 0,
            delay: 0,
            button: "left".into(),
            topmost: false,
            hotkeys: default_hotkeys(),
        }
    }
}

pub fn default_hotkeys() -> BTreeMap<String, String> {
    ACTIONS
        .into_iter()
        .zip(["ctrl+alt+r", "ctrl+alt+d", "ctrl+alt+s", "ctrl+alt+x"])
        .map(|(a, b)| (a.to_owned(), b.to_owned()))
        .collect()
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: 4,
            lang: "ja".into(),
            theme: "system".into(),
            points: vec![],
            interval_ticks: DEFAULT_INTERVAL_TICKS,
            repeat: 0,
            delay_ms: 0,
            button: "left".into(),
            topmost: false,
            hotkeys: default_hotkeys(),
            per_point: false,
            details_open: false,
            mode: ClickMode::Recorded,
            area: None,
            area_step_px: 30,
            fast_interval_ticks: TICKS_PER_MILLISECOND,
            key_clicks: crate::key_click::KeyClickSettings::default(),
        }
    }
}

impl Settings {
    pub fn interval_for(&self, index: usize) -> u64 {
        if self.per_point {
            self.points
                .get(index)
                .and_then(|p| p.interval_ticks)
                .unwrap_or(self.interval_ticks)
        } else {
            self.interval_ticks
        }
    }

    pub fn validate(&self) -> Result<(), ClickerError> {
        self.key_clicks.validate()?;
        if !(MIN_INTERVAL_TICKS..=MAX_INTERVAL_TICKS).contains(&self.fast_interval_ticks) {
            return Err(ErrorCode::IntervalRange.into());
        }
        if !(1..=4096).contains(&self.area_step_px) {
            return Err(ErrorCode::AreaInvalid.into());
        }
        if self
            .area
            .is_some_and(|a| a.grid_size(self.area_step_px).is_none())
        {
            return Err(ErrorCode::AreaInvalid.into());
        }
        if !(MIN_INTERVAL_TICKS..=MAX_INTERVAL_TICKS).contains(&self.interval_ticks) {
            return Err(ErrorCode::IntervalRange.into());
        }
        if self.points.len() > MAX_POINTS {
            return Err(ErrorCode::PointLimit.into());
        }
        if self.delay_ms > 30_000 || self.repeat > 999_999 {
            return Err(ErrorCode::Limits.into());
        }
        for p in &self.points {
            if p.interval_ticks
                .is_some_and(|v| !(MIN_INTERVAL_TICKS..=MAX_INTERVAL_TICKS).contains(&v))
            {
                return Err(ErrorCode::IntervalRange.into());
            }
        }
        if !matches!(self.lang.as_str(), "ja" | "en")
            || !matches!(self.theme.as_str(), "light" | "dark" | "system")
            || !matches!(self.button.as_str(), "left" | "right")
        {
            return Err(ErrorCode::Config.into());
        }
        Ok(())
    }

    pub fn load(path: &Path) -> Result<(Self, bool), ClickerError> {
        let raw = match fs::read(path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok((Self::default(), false));
            }
            Err(e) => {
                return Err(ClickerError::with_detail(
                    ErrorCode::SettingsRead,
                    e.to_string(),
                ));
            }
        };
        let mut value: serde_json::Value = serde_json::from_slice(&raw)
            .map_err(|e| ClickerError::with_detail(ErrorCode::SettingsJson, e.to_string()))?;
        if !value.is_object() {
            return Err(ErrorCode::Config.into());
        }
        let migrated = value.get("version").is_none();
        let rust_v2 = value.get("version").and_then(|v| v.as_u64()) == Some(2);
        let rust_v3 = value.get("version").and_then(|v| v.as_u64()) == Some(3);
        if rust_v2 {
            upgrade_millisecond_settings(&mut value)?;
        } else if rust_v3 {
            value["version"] = serde_json::Value::from(4);
        }
        let mut settings = if migrated {
            let old: LegacySettings = serde_json::from_value(value)
                .map_err(|e| ClickerError::with_detail(ErrorCode::SettingsJson, e.to_string()))?;
            if !old.interval.is_finite() || !(0.001..=3600.).contains(&old.interval) {
                return Err(ErrorCode::Config.into());
            }
            // Migration uses the new interval default and retains the other settings.
            Self {
                lang: old.lang,
                theme: old.theme,
                points: old
                    .points
                    .into_iter()
                    .map(|(x, y)| Point {
                        x,
                        y,
                        interval_ticks: None,
                    })
                    .collect(),
                repeat: old.repeat,
                delay_ms: old.delay.checked_mul(1000).ok_or(ErrorCode::Limits)?,
                button: old.button,
                topmost: old.topmost,
                hotkeys: old.hotkeys,
                ..Self::default()
            }
        } else {
            if value.get("version").and_then(|v| v.as_u64()) != Some(4) {
                return Err(ErrorCode::SettingsVersion.into());
            }
            serde_json::from_value(value)
                .map_err(|e| ClickerError::with_detail(ErrorCode::SettingsJson, e.to_string()))?
        };
        for (key, value) in default_hotkeys() {
            settings.hotkeys.entry(key).or_insert(value);
        }
        settings.validate()?;
        if migrated || rust_v2 || rust_v3 {
            let backup = path.with_file_name(if migrated {
                "settings.python-backup.json"
            } else if rust_v2 {
                "settings.v2-backup.json"
            } else {
                "settings.v3-backup.json"
            });
            preserve_settings_backup(&backup, &raw)?;
        }
        Ok((settings, migrated))
    }

    pub fn save(&self, path: &Path) -> Result<(), ClickerError> {
        use std::io::Write;
        self.validate()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| ClickerError::with_detail(ErrorCode::SettingsWrite, e.to_string()))?;
        }
        let temp = path.with_extension(format!("{}.tmp", std::process::id()));
        let mut created = false;
        let result = (|| -> Result<(), ClickerError> {
            let mut file = fs::File::create(&temp)
                .map_err(|e| ClickerError::with_detail(ErrorCode::SettingsWrite, e.to_string()))?;
            created = true;
            let contents = serde_json::to_vec_pretty(self)
                .map_err(|e| ClickerError::with_detail(ErrorCode::SettingsWrite, e.to_string()))?;
            file.write_all(&contents)
                .and_then(|_| file.sync_all())
                .map_err(|e| ClickerError::with_detail(ErrorCode::SettingsWrite, e.to_string()))?;
            drop(file);
            #[cfg(windows)]
            {
                use std::os::windows::ffi::OsStrExt;
                use windows_sys::Win32::Storage::FileSystem::{
                    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
                };
                let source: Vec<u16> = temp.as_os_str().encode_wide().chain(Some(0)).collect();
                let destination: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
                if unsafe {
                    MoveFileExW(
                        source.as_ptr(),
                        destination.as_ptr(),
                        MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                    )
                } == 0
                {
                    return Err(ClickerError::with_detail(
                        ErrorCode::SettingsWrite,
                        std::io::Error::last_os_error().to_string(),
                    ));
                }
            }
            #[cfg(not(windows))]
            fs::rename(&temp, path)
                .map_err(|e| ClickerError::with_detail(ErrorCode::SettingsWrite, e.to_string()))?;
            Ok(())
        })();
        if result.is_err() && created {
            let _ = fs::remove_file(&temp);
        }
        result
    }
}

fn preserve_settings_backup(backup: &Path, raw: &[u8]) -> Result<(), ClickerError> {
    use sha2::{Digest, Sha256};
    use std::io::Write;
    let write = |file: &mut fs::File| file.write_all(raw).and_then(|_| file.sync_all());
    if write_or_check_settings_backup(backup, raw, write)? {
        return Ok(());
    }
    // Keep earlier originals and give each subsequent raw file a stable name.
    let alternate = backup.with_extension(format!("{:x}.json", Sha256::digest(raw)));
    if !write_or_check_settings_backup(&alternate, raw, write)? {
        return Err(ClickerError::with_detail(
            ErrorCode::SettingsBackup,
            "The content-addressed settings backup contains different bytes.",
        ));
    }
    Ok(())
}

/// Returns false only for an existing regular file containing different bytes.
fn write_or_check_settings_backup(
    backup: &Path,
    raw: &[u8],
    write: impl FnOnce(&mut fs::File) -> std::io::Result<()>,
) -> Result<bool, ClickerError> {
    let backup_error = |error: std::io::Error| {
        ClickerError::with_detail(ErrorCode::SettingsBackup, error.to_string())
    };
    let check_existing = |metadata: fs::Metadata| -> Result<bool, ClickerError> {
        if !metadata.is_file() {
            return Err(ClickerError::with_detail(
                ErrorCode::SettingsBackup,
                "The settings backup path is not a regular file.",
            ));
        }
        let saved = fs::read(backup).map_err(backup_error)?;
        Ok(saved == raw)
    };
    match fs::symlink_metadata(backup) {
        Ok(metadata) => return check_existing(metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(backup_error(error)),
    }
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(backup)
    {
        Ok(mut file) => {
            let result = write(&mut file);
            drop(file);
            if let Err(error) = result {
                let _ = fs::remove_file(backup);
                return Err(backup_error(error));
            }
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            check_existing(fs::symlink_metadata(backup).map_err(backup_error)?)
        }
        Err(error) => Err(backup_error(error)),
    }
}

pub fn settings_path() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("MultiPointClicker")
        .join("settings.json")
}

/// Parse exact decimal seconds in 100ns ticks, without floating point rounding.
pub fn parse_interval(text: &str) -> Result<u64, ClickerError> {
    let text = text.trim();
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || fraction.len() > 7
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(ErrorCode::IntervalFormat.into());
    }
    let whole: u64 = whole.parse().map_err(|_| ErrorCode::IntervalRange)?;
    let fraction: u64 = format!("{fraction:0<7}")
        .parse()
        .map_err(|_| ErrorCode::IntervalFormat)?;
    let value = whole
        .checked_mul(TICKS_PER_SECOND)
        .and_then(|w| w.checked_add(fraction))
        .ok_or(ErrorCode::IntervalRange)?;
    if !(MIN_INTERVAL_TICKS..=MAX_INTERVAL_TICKS).contains(&value) {
        return Err(ErrorCode::IntervalRange.into());
    }
    Ok(value)
}

pub fn format_interval(ticks: u64) -> String {
    if ticks % TICKS_PER_MILLISECOND == 0 {
        format!(
            "{}.{:03}",
            ticks / TICKS_PER_SECOND,
            (ticks % TICKS_PER_SECOND) / TICKS_PER_MILLISECOND
        )
    } else {
        format!(
            "{}.{:07}",
            ticks / TICKS_PER_SECOND,
            ticks % TICKS_PER_SECOND
        )
    }
}

fn upgrade_millisecond_settings(value: &mut serde_json::Value) -> Result<(), ClickerError> {
    use serde_json::Value;
    let object = value.as_object_mut().ok_or(ErrorCode::Config)?;
    for (old, new, default_ms) in [
        ("interval_ms", "interval_ticks", 501),
        ("fast_interval_ms", "fast_interval_ticks", 1),
    ] {
        let ms = object
            .remove(old)
            .unwrap_or(Value::from(default_ms))
            .as_u64()
            .filter(|ms| (1..=3_600_000).contains(ms))
            .ok_or(ErrorCode::IntervalRange)?;
        object.insert(new.into(), Value::from(ms * TICKS_PER_MILLISECOND));
    }
    if let Some(points) = object.get_mut("points").and_then(Value::as_array_mut) {
        for point in points {
            let p = point.as_object_mut().ok_or(ErrorCode::PointData)?;
            let old = p.remove("interval_ms").unwrap_or(Value::Null);
            let ticks = if old.is_null() {
                Value::Null
            } else {
                Value::from(
                    old.as_u64()
                        .filter(|v| (1..=3_600_000).contains(v))
                        .ok_or(ErrorCode::IntervalRange)?
                        * TICKS_PER_MILLISECOND,
                )
            };
            p.insert("interval_ticks".into(), ticks);
        }
    }
    object.insert("version".into(), Value::from(4));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const IMPORT_CASES: [(&str, &[u8], bool); 3] = [
        (
            "settings.python-backup.json",
            br#"{"points":[[-150,42]],"interval":0.06,"repeat":500}"#,
            true,
        ),
        (
            "settings.v2-backup.json",
            br#"{"version":2,"interval_ms":125,"points":[{"x":-150,"y":42}]}"#,
            false,
        ),
        (
            "settings.v3-backup.json",
            br#"{"version":3,"interval_ticks":1250000,"points":[{"x":-150,"y":42}]}"#,
            false,
        ),
    ];

    fn expected_import_backup(backup: &Path, raw: &[u8]) -> PathBuf {
        use sha2::{Digest, Sha256};
        backup.with_extension(format!("{:x}.json", Sha256::digest(raw)))
    }

    struct SettingsTestDirectory(PathBuf);
    impl SettingsTestDirectory {
        fn new(name: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "clicker-{name}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn settings_path(&self) -> PathBuf {
            self.0.join("settings.json")
        }
    }
    impl Drop for SettingsTestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn v3_migration_preserves_every_setting_and_backs_up_the_original_bytes() {
        use crate::key_click::{InputMethod, MovementMode, PressBehavior};
        let dir = SettingsTestDirectory::new("v3-upgrade");
        let path = dir.settings_path();
        let raw = br#"{
            "version":3,"lang":"en","theme":"dark",
            "points":[{"x":-150,"y":42,"interval_ticks":9},{"x":10,"y":20}],
            "interval_ticks":9876543,"fast_interval_ticks":17,
            "repeat":123,"delay_ms":1500,"button":"right","topmost":true,
            "hotkeys":{"record_start":"ctrl+shift+r","record_stop":"ctrl+shift+d",
                "click_start":"ctrl+shift+s","click_stop":"ctrl+shift+x"},
            "per_point":true,"details_open":true,"mode":"area",
            "area":{"left":-80,"top":-40,"right":800,"bottom":600},"area_step_px":42,
            "key_clicks":{"enabled":true,
                "last_target":{"executable":"C:/Games/example.exe","class":"GameWindow","title":"Example"},
                "bindings":[{"key":87,"right":true,"position":{
                    "game":{"executable":"C:/Games/example.exe","class":"GameWindow","title":"Example"},
                    "x":320,"y":640,"width":1280,"height":900}},
                    {"key":32,"position":null,"right":false}]}
        }"#;
        fs::write(&path, raw).unwrap();
        let (settings, python_migrated) = Settings::load(&path).unwrap();
        assert!(!python_migrated);
        assert_eq!(settings.version, 4);
        assert_eq!(settings.interval_ticks, 9876543);
        assert_eq!(settings.fast_interval_ticks, 17);
        assert_eq!(settings.interval_for(0), 9);
        assert_eq!(settings.interval_for(1), 9876543);
        assert_eq!(settings.hotkeys["click_stop"], "ctrl+shift+x");
        assert_eq!(settings.key_clicks.input_method, InputMethod::Mouse);
        assert_eq!(settings.key_clicks.movement_mode, MovementMode::Buttons);
        assert!(settings.key_clicks.enabled);
        assert!(settings.key_clicks.bindings[0].right);
        assert_eq!(
            settings.key_clicks.bindings[0].behavior,
            PressBehavior::Click
        );
        assert_eq!(settings.key_clicks.bindings[0].label, "");
        assert_eq!(settings.key_clicks.bindings[1].position, None);
        let mut expected: Settings = serde_json::from_slice(raw).unwrap();
        expected.version = 4;
        assert_eq!(
            serde_json::to_value(&settings).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
        let backup = dir.0.join("settings.v3-backup.json");
        assert_eq!(fs::read(&backup).unwrap(), raw);
        assert_eq!(fs::read(&path).unwrap(), raw);
        assert!(!dir.0.join("settings.python-backup.json").exists());
        assert!(!dir.0.join("settings.v2-backup.json").exists());
        let (_, python_migrated) = Settings::load(&path).unwrap();
        assert!(!python_migrated);
        assert_eq!(fs::read(&backup).unwrap(), raw);
        settings.save(&path).unwrap();
        let (loaded, python_migrated) = Settings::load(&path).unwrap();
        assert!(!python_migrated);
        assert_eq!(loaded.version, 4);
        assert_eq!(loaded.key_clicks, settings.key_clicks);
        assert_eq!(loaded.interval_ticks, 9876543);
        assert_eq!(fs::read(&backup).unwrap(), raw);
    }

    #[test]
    fn reimports_preserve_both_originals_and_reuse_the_same_backup_for_the_same_raw() {
        for (filename, raw, python_import) in IMPORT_CASES {
            let dir = SettingsTestDirectory::new("backup-reimport");
            let path = dir.settings_path();
            let backup = dir.0.join(filename);
            let alternate = expected_import_backup(&backup, raw);
            let mut earlier = raw.to_vec();
            earlier.push(b'\n');
            fs::write(&path, raw).unwrap();
            fs::write(&backup, &earlier).unwrap();
            let (loaded, migrated) = Settings::load(&path).unwrap();
            assert_eq!(migrated, python_import);
            assert_eq!(loaded.version, 4);
            assert_eq!(loaded.points[0].x, -150);
            assert_eq!(fs::read(&backup).unwrap(), earlier);
            assert_eq!(fs::read(&alternate).unwrap(), raw);
            assert_eq!(fs::read(&path).unwrap(), raw);
            let saved_permissions = fs::metadata(&alternate).unwrap().permissions();
            let mut readonly = saved_permissions.clone();
            readonly.set_readonly(true);
            fs::set_permissions(&alternate, readonly).unwrap();
            let retry = Settings::load(&path);
            fs::set_permissions(&alternate, saved_permissions).unwrap();
            assert_eq!(retry.unwrap().1, python_import);
            assert_eq!(Settings::load(&path).unwrap().1, python_import);
            assert_eq!(fs::read(&backup).unwrap(), earlier);
            assert_eq!(fs::read(&alternate).unwrap(), raw);
            assert_eq!(fs::read(&path).unwrap(), raw);
            assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 3);
        }
    }

    #[test]
    fn invalid_backup_paths_and_corrupt_named_backups_leave_all_originals_untouched() {
        for (filename, raw, _) in IMPORT_CASES {
            let dir = SettingsTestDirectory::new("backup-invalid");
            let path = dir.settings_path();
            let backup = dir.0.join(filename);
            let alternate = expected_import_backup(&backup, raw);
            fs::write(&path, raw).unwrap();
            fs::create_dir(&backup).unwrap();
            assert_eq!(
                Settings::load(&path).unwrap_err().code,
                ErrorCode::SettingsBackup
            );
            assert_eq!(fs::read(&path).unwrap(), raw);
            assert!(backup.is_dir());
            assert!(!alternate.exists());
            fs::remove_dir(&backup).unwrap();
            let mut earlier = raw.to_vec();
            earlier.push(b'\n');
            fs::write(&backup, &earlier).unwrap();
            fs::create_dir(&alternate).unwrap();
            assert_eq!(
                Settings::load(&path).unwrap_err().code,
                ErrorCode::SettingsBackup
            );
            assert!(alternate.is_dir());
            assert_eq!(fs::read(&backup).unwrap(), earlier);
            assert_eq!(fs::read(&path).unwrap(), raw);
            fs::remove_dir(&alternate).unwrap();
            let corrupt = &raw[..raw.len() / 2];
            fs::write(&alternate, corrupt).unwrap();
            assert_eq!(
                Settings::load(&path).unwrap_err().code,
                ErrorCode::SettingsBackup
            );
            assert_eq!(fs::read(&alternate).unwrap(), corrupt);
            assert_eq!(fs::read(&backup).unwrap(), earlier);
            assert_eq!(fs::read(&path).unwrap(), raw);
            assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 3);
        }
    }

    #[test]
    fn backup_creation_and_partial_write_failures_keep_the_source_and_allow_a_clean_retry() {
        use std::io::Write;
        for (filename, raw, python_import) in IMPORT_CASES {
            let dir = SettingsTestDirectory::new("backup-write-failure");
            let path = dir.settings_path();
            let missing_parent = dir.0.join("missing");
            fs::write(&path, raw).unwrap();
            assert_eq!(
                preserve_settings_backup(&missing_parent.join(filename), raw)
                    .unwrap_err()
                    .code,
                ErrorCode::SettingsBackup
            );
            assert!(!missing_parent.exists());
            assert_eq!(fs::read(&path).unwrap(), raw);
            let backup = dir.0.join(filename);
            let alternate = expected_import_backup(&backup, raw);
            let mut earlier = raw.to_vec();
            earlier.push(b'\n');
            fs::write(&backup, &earlier).unwrap();
            let result = write_or_check_settings_backup(&alternate, raw, |file| {
                file.write_all(&raw[..raw.len() / 2])?;
                Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "Write failed",
                ))
            });
            assert_eq!(result.unwrap_err().code, ErrorCode::SettingsBackup);
            assert!(!alternate.exists());
            assert_eq!(fs::read(&backup).unwrap(), earlier);
            assert_eq!(fs::read(&path).unwrap(), raw);
            assert_eq!(Settings::load(&path).unwrap().1, python_import);
            assert_eq!(fs::read(&backup).unwrap(), earlier);
            assert_eq!(fs::read(&alternate).unwrap(), raw);
            assert_eq!(fs::read(&path).unwrap(), raw);
        }
    }

    #[test]
    fn invalid_v3_controls_do_not_overwrite_settings_or_create_a_backup() {
        let dir = SettingsTestDirectory::new("v3-invalid");
        let path = dir.settings_path();
        for raw in [
            r#"{"version":3,"key_clicks":{"bindings":[{"key":87},{"key":87}]}}"#,
            r#"{"version":3,"key_clicks":{"stick":{"radius_px":3}}}"#,
            r#"{"version":3,"key_clicks":{"selected_profile":999}}"#,
            r#"{"version":3,"interval_ticks":0}"#,
        ] {
            fs::write(&path, raw).unwrap();
            assert!(Settings::load(&path).is_err(), "{raw}");
            assert_eq!(fs::read_to_string(&path).unwrap(), raw);
            assert!(!dir.0.join("settings.v3-backup.json").exists());
        }
    }

    #[test]
    fn profiles_and_an_unnamed_incomplete_editor_round_trip_in_v4() {
        use crate::key_click::{InputMethod, KeyBinding, MovementMode, PressBehavior};
        let dir = SettingsTestDirectory::new("v4-profile-save");
        let path = dir.settings_path();
        let mut settings = Settings {
            interval_ticks: 3,
            ..Settings::default()
        };
        settings.key_clicks.bindings.push(KeyBinding {
            key: Some(b'W' as u32),
            right: true,
            ..KeyBinding::default()
        });
        let old = settings.key_clicks.save_profile("Existing").unwrap();
        settings.key_clicks.new_profile().unwrap();
        settings.key_clicks.bindings.push(KeyBinding {
            key: Some(0x10),
            label: "Run".into(),
            behavior: PressBehavior::Hold,
            ..KeyBinding::default()
        });
        settings.key_clicks.movement_mode = MovementMode::Stick;
        settings.key_clicks.stick.radius_px = 48;
        settings.key_clicks.sync_selected_profile().unwrap();
        assert_eq!(settings.key_clicks.selected_profile, None);
        assert!(!settings.key_clicks.has_configured_action());
        settings.save(&path).unwrap();
        let (mut loaded, migrated) = Settings::load(&path).unwrap();
        assert!(!migrated);
        assert_eq!(loaded.key_clicks, settings.key_clicks);
        assert_eq!(loaded.interval_ticks, 3);
        assert_eq!(loaded.key_clicks.input_method, InputMethod::Touch);
        loaded.key_clicks.select_profile(old).unwrap();
        assert_eq!(loaded.key_clicks.profiles.len(), 2);
        assert_eq!(loaded.key_clicks.bindings[0].behavior, PressBehavior::Click);
        assert!(loaded.key_clicks.bindings[0].right);
        let draft = loaded
            .key_clicks
            .profiles
            .iter()
            .find(|profile| profile.id != old)
            .unwrap()
            .id;
        loaded.key_clicks.select_profile(draft).unwrap();
        assert_eq!(loaded.key_clicks.bindings[0].key, Some(0x10));
        assert_eq!(loaded.key_clicks.bindings[0].label, "Run");
        assert_eq!(loaded.key_clicks.stick.center, None);
        assert_eq!(loaded.key_clicks.stick.radius_px, 48);
        assert!(!loaded.key_clicks.has_configured_action());
        assert!(!dir.0.join("settings.v3-backup.json").exists());
    }

    #[test]
    fn nanosecond_precision_and_millisecond_migration_preserve_exact_values() {
        let dir = std::env::temp_dir().join(format!("clicker-tick-upgrade-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let raw=br#"{"version":2,"interval_ms":501,"fast_interval_ms":1,"points":[{"x":5,"y":-6,"interval_ms":7}],"repeat":500,"mode":"drag"}"#;
        fs::write(&path, raw).unwrap();
        let (mut s, migrated) = Settings::load(&path).unwrap();
        assert!(!migrated);
        assert_eq!(s.version, 4);
        assert_eq!(s.interval_ticks, 5_010_000);
        assert_eq!(s.fast_interval_ticks, 10_000);
        assert_eq!(s.points[0].interval_ticks, Some(70_000));
        assert_eq!(s.points[0].y, -6);
        assert_eq!(s.repeat, 500);
        assert_eq!(s.mode, ClickMode::Drag);
        assert_eq!(fs::read(dir.join("settings.v2-backup.json")).unwrap(), raw);
        s.fast_interval_ticks = parse_interval("0.0000001").unwrap();
        s.points[0].interval_ticks = Some(parse_interval("0.0000009").unwrap());
        s.save(&path).unwrap();
        let (back, _) = Settings::load(&path).unwrap();
        assert_eq!(back.fast_interval_ticks, 1);
        assert_eq!(back.points[0].interval_ticks, Some(9));
        assert_eq!(format_interval(1), "0.0000001");
        assert_eq!(format_interval(5_010_000), "0.501");
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn grid_is_even_serpentine_and_includes_edges() {
        let a = ClickArea::from_corners((9, 8), (-2, -3));
        assert_eq!(a.grid_size(5), Some((4, 4)));
        let points: Vec<_> = (0..16)
            .map(|i| {
                let p = a.point_at(5, i).unwrap();
                assert!((-2..=9).contains(&p.x) && (-3..=8).contains(&p.y));
                (p.x, p.y)
            })
            .collect();
        assert_eq!(&points[..4], &[(-2, -3), (3, -3), (8, -3), (9, -3)]);
        assert_eq!(&points[4..8], &[(9, 2), (8, 2), (3, 2), (-2, 2)]);
        assert!(points.contains(&(9, 8)));
        assert_eq!(
            points
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            16
        );
        assert_eq!(a.point_at(5, 16), a.point_at(5, 0));
        assert!(a.grid_size(0).is_none());
    }
    #[test]
    fn grids_handle_single_pixels_large_extents_and_overflow() {
        let one = ClickArea::from_corners((-2, -8), (-2, -8));
        assert_eq!(one.grid_size(30), Some((1, 1)));
        assert_eq!(one.point_at(30, 999).unwrap().x, -2);
        let full = ClickArea::from_corners((i32::MIN, i32::MIN), (i32::MAX, i32::MAX));
        assert!(full.grid_size(1).is_none());
        assert!(full.grid_size(30).is_some());
        let a = ClickArea::from_corners((0, 0), (1, 1));
        assert_eq!(a.grid_size(4096), Some((2, 2)));
    }
    #[test]
    fn old_rust_settings_default_to_recorded_and_fast_modes_round_trip() {
        let mut value: serde_json::Value =
            serde_json::from_str(r#"{"version":2,"points":[],"interval_ms":501}"#).unwrap();
        upgrade_millisecond_settings(&mut value).unwrap();
        let s: Settings = serde_json::from_value(value).unwrap();
        assert_eq!(s.mode, ClickMode::Recorded);
        assert_eq!(s.fast_interval_ticks, TICKS_PER_MILLISECOND);
        let area = ClickArea::from_corners((50, 60), (5, 8));
        let changed = Settings {
            mode: ClickMode::Area,
            area: Some(area),
            ..s
        };
        let saved = serde_json::to_string(&changed).unwrap();
        let back: Settings = serde_json::from_str(&saved).unwrap();
        assert_eq!(back.area, Some(area));
        assert_eq!(back.mode, ClickMode::Area);
    }
    #[test]
    fn decimal_boundaries_are_exact() {
        for (input, ms) in [
            ("0.0000001", 1),
            ("0.0000009", 9),
            ("0.001", 10_000),
            ("0.501", 5_010_000),
            ("1", TICKS_PER_SECOND),
            ("0.01", 100_000),
            ("3600.000", MAX_INTERVAL_TICKS),
        ] {
            assert_eq!(parse_interval(input).unwrap(), ms);
            assert_eq!(parse_interval(&format_interval(ms)).unwrap(), ms);
        }
        for input in [
            "",
            ".5",
            "0",
            "0.000",
            "-0.001",
            "NaN",
            "inf",
            "1e-3",
            "0.00000001",
            "3600.001",
            "18446744073709551615",
        ] {
            assert!(parse_interval(input).is_err(), "{input}");
        }
    }
    #[test]
    fn individual_intervals_follow_the_point() {
        let mut s = Settings {
            per_point: true,
            points: vec![
                Point {
                    x: 0,
                    y: 0,
                    interval_ticks: Some(1),
                },
                Point {
                    x: 1,
                    y: 1,
                    interval_ticks: Some(7),
                },
                Point {
                    x: 2,
                    y: 2,
                    interval_ticks: None,
                },
            ],
            ..Settings::default()
        };
        assert_eq!(
            (s.interval_for(0), s.interval_for(1), s.interval_for(2)),
            (1, 7, DEFAULT_INTERVAL_TICKS)
        );
        s.points.remove(0);
        assert_eq!(s.interval_for(0), 7);
        s.per_point = false;
        assert_eq!(s.interval_for(0), DEFAULT_INTERVAL_TICKS);
    }
    #[test]
    fn migrate_preserves_positions_and_keeps_the_original() {
        let dir = std::env::temp_dir().join(format!("clicker-migrate-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let raw = br#"{"points":[[-150,42],[100,200]],"interval":0.06,"repeat":500,"lang":"ja"}"#;
        fs::write(&path, raw).unwrap();
        let (s, migrated) = Settings::load(&path).unwrap();
        assert!(migrated);
        assert_eq!(s.interval_ticks, DEFAULT_INTERVAL_TICKS);
        assert_eq!(s.repeat, 500);
        assert_eq!(s.points[0].x, -150);
        s.save(&path).unwrap();
        let (loaded, migrated) = Settings::load(&path).unwrap();
        assert!(!migrated);
        assert_eq!(loaded.points, s.points);
        assert_eq!(
            fs::read(dir.join("settings.python-backup.json")).unwrap(),
            raw
        );
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn broken_settings_are_not_treated_as_missing() {
        let dir = std::env::temp_dir().join(format!("clicker-invalid-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        fs::write(&path, b"{broken").unwrap();
        assert!(Settings::load(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"{broken");
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn malformed_legacy_values_never_overwrite_or_create_a_backup() {
        let dir =
            std::env::temp_dir().join(format!("clicker-legacy-invalid-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        for raw in [
            "null",
            "[]",
            r#"{"points":"invalid"}"#,
            r#"{"repeat":true}"#,
            r#"{"topmost":"false"}"#,
            r#"{"interval":"0.06"}"#,
            r#"{"points":[[1,"2"]]}"#,
        ] {
            fs::write(&path, raw).unwrap();
            assert!(Settings::load(&path).is_err(), "{raw}");
            assert_eq!(fs::read_to_string(&path).unwrap(), raw);
            assert!(!dir.join("settings.python-backup.json").exists());
        }
        fs::remove_dir_all(dir).unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn failed_atomic_replace_preserves_the_original_and_cleans_its_temp() {
        let dir = std::env::temp_dir().join(format!("clicker-readonly-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let original = Settings::default();
        original.save(&path).unwrap();
        let bytes = fs::read(&path).unwrap();
        let saved_permissions = fs::metadata(&path).unwrap().permissions();
        let mut readonly = saved_permissions.clone();
        readonly.set_readonly(true);
        fs::set_permissions(&path, readonly).unwrap();
        let changed = Settings {
            interval_ticks: 1,
            ..Settings::default()
        };
        let result = changed.save(&path);
        fs::set_permissions(&path, saved_permissions).unwrap();
        assert!(result.is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert!(
            !path
                .with_extension(format!("{}.tmp", std::process::id()))
                .exists()
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
