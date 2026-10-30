use crate::error::{ClickerError, ErrorCode};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

pub const MIN_INTERVAL_MS: u64 = 1;
pub const MAX_INTERVAL_MS: u64 = 3_600_000;
pub const DEFAULT_INTERVAL_MS: u64 = 501;
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
            interval_ms: None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Point {
    pub x: i32,
    pub y: i32,
    #[serde(default)]
    pub interval_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub version: u32,
    pub lang: String,
    pub theme: String,
    pub points: Vec<Point>,
    pub interval_ms: u64,
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
    pub fast_interval_ms: u64,
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
            version: 2,
            lang: "ja".into(),
            theme: "system".into(),
            points: vec![],
            interval_ms: DEFAULT_INTERVAL_MS,
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
            fast_interval_ms: 1,
        }
    }
}

impl Settings {
    pub fn interval_for(&self, index: usize) -> u64 {
        if self.per_point {
            self.points
                .get(index)
                .and_then(|p| p.interval_ms)
                .unwrap_or(self.interval_ms)
        } else {
            self.interval_ms
        }
    }

    pub fn validate(&self) -> Result<(), ClickerError> {
        if !(MIN_INTERVAL_MS..=MAX_INTERVAL_MS).contains(&self.fast_interval_ms) {
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
        if !(MIN_INTERVAL_MS..=MAX_INTERVAL_MS).contains(&self.interval_ms) {
            return Err(ErrorCode::IntervalRange.into());
        }
        if self.points.len() > MAX_POINTS {
            return Err(ErrorCode::PointLimit.into());
        }
        if self.delay_ms > 30_000 || self.repeat > 999_999 {
            return Err(ErrorCode::Limits.into());
        }
        for p in &self.points {
            if p.interval_ms
                .is_some_and(|v| !(MIN_INTERVAL_MS..=MAX_INTERVAL_MS).contains(&v))
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
        let value: serde_json::Value = serde_json::from_slice(&raw)
            .map_err(|e| ClickerError::with_detail(ErrorCode::SettingsJson, e.to_string()))?;
        if !value.is_object() {
            return Err(ErrorCode::Config.into());
        }
        let migrated = value.get("version").is_none();
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
                        interval_ms: None,
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
            if value.get("version").and_then(|v| v.as_u64()) != Some(2) {
                return Err(ErrorCode::SettingsVersion.into());
            }
            serde_json::from_value(value)
                .map_err(|e| ClickerError::with_detail(ErrorCode::SettingsJson, e.to_string()))?
        };
        for (key, value) in default_hotkeys() {
            settings.hotkeys.entry(key).or_insert(value);
        }
        settings.validate()?;
        if migrated {
            let backup = path.with_file_name("settings.python-backup.json");
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&backup)
            {
                Ok(mut file) => {
                    use std::io::Write;
                    file.write_all(&raw)
                        .and_then(|_| file.sync_all())
                        .map_err(|e| {
                            ClickerError::with_detail(ErrorCode::SettingsBackup, e.to_string())
                        })?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => {
                    return Err(ClickerError::with_detail(
                        ErrorCode::SettingsBackup,
                        e.to_string(),
                    ));
                }
            }
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

pub fn settings_path() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("MultiPointClicker")
        .join("settings.json")
}

/// Parse exact millisecond decimal seconds without floating point rounding.
pub fn parse_interval(text: &str) -> Result<u64, ClickerError> {
    let text = text.trim();
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || fraction.len() > 3
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(ErrorCode::IntervalFormat.into());
    }
    let whole: u64 = whole.parse().map_err(|_| ErrorCode::IntervalRange)?;
    let fraction: u64 = format!("{fraction:0<3}")
        .parse()
        .map_err(|_| ErrorCode::IntervalFormat)?;
    let value = whole
        .checked_mul(1000)
        .and_then(|w| w.checked_add(fraction))
        .ok_or(ErrorCode::IntervalRange)?;
    if !(MIN_INTERVAL_MS..=MAX_INTERVAL_MS).contains(&value) {
        return Err(ErrorCode::IntervalRange.into());
    }
    Ok(value)
}

pub fn format_interval(ms: u64) -> String {
    format!("{}.{:03}", ms / 1000, ms % 1000)
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let s: Settings =
            serde_json::from_str(r#"{"version":2,"points":[],"interval_ms":501}"#).unwrap();
        assert_eq!(s.mode, ClickMode::Recorded);
        assert_eq!(s.fast_interval_ms, 1);
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
            ("0.001", 1),
            ("0.501", 501),
            ("1", 1000),
            ("0.01", 10),
            ("3600.000", MAX_INTERVAL_MS),
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
            "0.0001",
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
                    interval_ms: Some(1),
                },
                Point {
                    x: 1,
                    y: 1,
                    interval_ms: Some(7),
                },
                Point {
                    x: 2,
                    y: 2,
                    interval_ms: None,
                },
            ],
            ..Settings::default()
        };
        assert_eq!(
            (s.interval_for(0), s.interval_for(1), s.interval_for(2)),
            (1, 7, 501)
        );
        s.points.remove(0);
        assert_eq!(s.interval_for(0), 7);
        s.per_point = false;
        assert_eq!(s.interval_for(0), 501);
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
        assert_eq!(s.interval_ms, 501);
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
            interval_ms: 1,
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
