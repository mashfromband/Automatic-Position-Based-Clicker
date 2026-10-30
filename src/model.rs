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

    pub fn validate(&self) -> Result<(), String> {
        if !(MIN_INTERVAL_MS..=MAX_INTERVAL_MS).contains(&self.interval_ms) {
            return Err("間隔は 0.001〜3600.000 秒で指定してください。".into());
        }
        if self.points.len() > MAX_POINTS {
            return Err("クリック位置は10か所までです。".into());
        }
        if self.delay_ms > 30_000 || self.repeat > 999_999 {
            return Err("待機時間または繰り返し回数が範囲外です。".into());
        }
        for p in &self.points {
            if p.interval_ms
                .is_some_and(|v| !(MIN_INTERVAL_MS..=MAX_INTERVAL_MS).contains(&v))
            {
                return Err("個別の間隔は 0.001〜3600.000 秒で指定してください。".into());
            }
        }
        if !matches!(self.lang.as_str(), "ja" | "en")
            || !matches!(self.theme.as_str(), "light" | "dark" | "system")
            || !matches!(self.button.as_str(), "left" | "right")
        {
            return Err("設定の値が不正です。".into());
        }
        Ok(())
    }

    pub fn load(path: &Path) -> Result<(Self, bool), String> {
        let raw = match fs::read(path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok((Self::default(), false));
            }
            Err(e) => return Err(format!("設定を読めません: {e}")),
        };
        let value: serde_json::Value =
            serde_json::from_slice(&raw).map_err(|e| format!("設定のJSONが不正です: {e}"))?;
        let migrated = value.get("version").is_none();
        let mut settings = if migrated {
            let mut s = Self::default();
            if let Some(v) = value.get("lang").and_then(|v| v.as_str()) {
                s.lang = v.into();
            }
            if let Some(v) = value.get("theme").and_then(|v| v.as_str()) {
                s.theme = v.into();
            }
            if let Some(v) = value.get("repeat").and_then(|v| v.as_u64()) {
                s.repeat = v.min(999_999) as u32;
            }
            if let Some(v) = value.get("delay").and_then(|v| v.as_u64()) {
                s.delay_ms = v.min(30) * 1000;
            }
            if let Some(v) = value.get("button").and_then(|v| v.as_str()) {
                s.button = v.into();
            }
            s.topmost = value
                .get("topmost")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if let Some(v) = value.get("hotkeys") {
                s.hotkeys = serde_json::from_value(v.clone()).map_err(|e| e.to_string())?;
            }
            if let Some(points) = value.get("points").and_then(|v| v.as_array()) {
                for p in points.iter().take(MAX_POINTS) {
                    let coords = p
                        .as_array()
                        .filter(|p| p.len() == 2)
                        .ok_or("クリック位置の形式が不正です。")?;
                    let x = coords[0]
                        .as_i64()
                        .and_then(|x| i32::try_from(x).ok())
                        .ok_or("X座標が不正です。")?;
                    let y = coords[1]
                        .as_i64()
                        .and_then(|y| i32::try_from(y).ok())
                        .ok_or("Y座標が不正です。")?;
                    s.points.push(Point {
                        x,
                        y,
                        interval_ms: None,
                    });
                }
            }
            // The owner requested 0.501 seconds for the first Rust launch, including migration.
            s
        } else {
            if value.get("version").and_then(|v| v.as_u64()) != Some(2) {
                return Err("未対応の設定ファイルの版です。".into());
            }
            serde_json::from_value(value).map_err(|e| format!("設定を解釈できません: {e}"))?
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
                        .map_err(|e| format!("旧設定を保全できません: {e}"))?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(format!("旧設定を保全できません: {e}")),
            }
        }
        Ok((settings, migrated))
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        use std::io::Write;
        self.validate()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let temp = path.with_extension(format!("{}.tmp", std::process::id()));
        let mut file = fs::File::create(&temp).map_err(|e| e.to_string())?;
        let contents = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        file.write_all(&contents)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
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
                return Err(format!(
                    "設定を保存できません: {}",
                    std::io::Error::last_os_error()
                ));
            }
        }
        #[cfg(not(windows))]
        fs::rename(temp, path).map_err(|e| e.to_string())?;
        Ok(())
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
pub fn parse_interval(text: &str) -> Result<u64, String> {
    let text = text.trim();
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || fraction.len() > 3
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        return Err("秒数は小数点以下3桁までで指定してください（例: 0.501）。".into());
    }
    let whole: u64 = whole.parse().map_err(|_| "秒数が範囲外です。")?;
    let fraction: u64 = format!("{fraction:0<3}")
        .parse()
        .map_err(|_| "秒数が不正です。")?;
    let value = whole
        .checked_mul(1000)
        .and_then(|w| w.checked_add(fraction))
        .ok_or("秒数が範囲外です。")?;
    if !(MIN_INTERVAL_MS..=MAX_INTERVAL_MS).contains(&value) {
        return Err("間隔は 0.001〜3600.000 秒で指定してください。".into());
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
}
