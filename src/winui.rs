use multi_point_clicker::{
    engine::{Progress, Runner},
    error::{ClickerError, ErrorCode},
    model::{self, ACTIONS, ClickArea, ClickMode, MAX_POINTS, Point, Settings},
};
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashSet},
    mem::{size_of, zeroed},
    path::PathBuf,
    ptr::{null, null_mut},
    sync::atomic::{AtomicU32, AtomicUsize, Ordering},
    time::{Duration, Instant},
};
use windows_sys::{
    Win32::{
        Foundation::*,
        Graphics::{Dwm::*, Gdi::*},
        System::{LibraryLoader::GetModuleHandleW, Registry::*, Threading::*},
        UI::{Controls::*, HiDpi::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
    },
    core::w,
};

const APP_RECORD: u32 = WM_APP + 1;
const APP_CAPTURE: u32 = WM_APP + 2;
const APP_INIT: u32 = WM_APP + 3;
const STATIC_LEFT: u32 = 0; // SS_LEFT (the default STATIC control alignment).
const TICK: usize = 1;
const PREVIEW_TIMER: usize = 2;
const NAV_MAIN: u16 = 10;
const NAV_SETTINGS: u16 = 11;
const LANG: u16 = 12;
const RECORD: u16 = 20;
const RECORD_STOP: u16 = 21;
const START: u16 = 22;
const STOP: u16 = 23;
const INTERVAL: u16 = 30;
const REPEAT: u16 = 31;
const DETAILS: u16 = 32;
const INDIVIDUAL: u16 = 33;
const PREVIEW: u16 = 34;
const CLEAR: u16 = 35;
const DEFAULT_INTERVAL: u16 = 36;
const DELAY: u16 = 40;
const BUTTON: u16 = 41;
const THEME: u16 = 42;
const TOPMOST: u16 = 43;
const RESET_KEYS: u16 = 44;
const MODE: u16 = 60;
const AREA_SELECT: u16 = 61;
const AREA_LEFT: u16 = 62;
const AREA_TOP: u16 = 63;
const AREA_RIGHT: u16 = 64;
const AREA_BOTTOM: u16 = 65;
const AREA_STEP: u16 = 66;
const FAST_INTERVAL: u16 = 67;
const LOCK: u16 = 68;
const LOCK_HOTKEY: i32 = 5;
const INTERVAL_POINT: u16 = 100;
const DELETE_POINT: u16 = 200;
const CAPTURE_KEY: u16 = 300;
const STATUS: u16 = 500;
const CLICKS: u16 = 501;
const CYCLES: u16 = 502;
const ELAPSED: u16 = 503;
const MESSAGE: u16 = 504;
static ROOT: AtomicUsize = AtomicUsize::new(0);
static CAPTURE: AtomicU32 = AtomicU32::new(0);
static DARK: AtomicU32 = AtomicU32::new(1);
static ACTIVE_TAB: AtomicU32 = AtomicU32::new(0);
thread_local! { static APP: RefCell<Option<App>> = const { RefCell::new(None) }; }

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
fn color(r: u8, g: u8, b: u8) -> u32 {
    r as u32 | ((g as u32) << 8) | ((b as u32) << 16)
}
fn bg() -> u32 {
    if DARK.load(Ordering::Relaxed) != 0 {
        color(31, 31, 31)
    } else {
        color(248, 248, 248)
    }
}
fn fg() -> u32 {
    if DARK.load(Ordering::Relaxed) != 0 {
        color(232, 237, 245)
    } else {
        color(24, 35, 54)
    }
}
fn muted() -> u32 {
    if DARK.load(Ordering::Relaxed) != 0 {
        color(161, 174, 194)
    } else {
        color(85, 102, 127)
    }
}
fn panel() -> u32 {
    if DARK.load(Ordering::Relaxed) != 0 {
        color(45, 45, 45)
    } else {
        color(237, 237, 237)
    }
}
fn text(hwnd: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(hwnd).max(0) as usize;
        let mut v = vec![0; len + 1];
        let n = GetWindowTextW(hwnd, v.as_mut_ptr(), v.len() as i32);
        String::from_utf16_lossy(&v[..n.max(0) as usize])
    }
}
fn set_text(hwnd: HWND, s: &str) {
    if !hwnd.is_null() && text(hwnd) != s {
        unsafe {
            SetWindowTextW(hwnd, wide(s).as_ptr());
        }
    }
}
fn error_dialog(hwnd: HWND, s: &str) {
    unsafe {
        MessageBoxW(
            hwnd,
            wide(s).as_ptr(),
            w!("Multi Point Clicker"),
            MB_OK | MB_ICONERROR,
        );
    }
}

fn error_text(error: &ClickerError, japanese: bool) -> String {
    use ErrorCode::*;
    let pair = match error.code {
        AreaMissing => (
            "先に範囲の2隅を指定してください。",
            "Select the two corners of a region first.",
        ),
        AreaInvalid => (
            "範囲の座標または配置間隔が不正です（1〜4096 px）。",
            "Invalid region coordinates or grid spacing (1–4096 px).",
        ),
        InputMonitor => (
            "ドラッグの入力監視を開始できません。",
            "Cannot start monitoring physical drag input.",
        ),
        IntervalRange => (
            "間隔は 0.001〜3600.000 秒で指定してください。",
            "Use an interval from 0.001 to 3600.000 seconds.",
        ),
        IntervalFormat => (
            "秒数は小数点以下3桁までで指定してください（例: 0.501）。",
            "Enter seconds with up to three decimal places (for example, 0.501).",
        ),
        PointLimit => (
            "クリック位置は10か所までです。",
            "You can save up to 10 click positions.",
        ),
        Limits => (
            "待機時間または繰り返し回数が範囲外です。",
            "Start delay or repeat count is out of range.",
        ),
        Config => ("設定の値が不正です。", "A settings value is invalid."),
        SettingsRead => ("設定を読めません。", "Cannot read settings."),
        SettingsJson => (
            "設定ファイルの内容が不正です。",
            "The settings file is invalid.",
        ),
        SettingsVersion => (
            "未対応の設定ファイルの版です。",
            "This settings version is not supported.",
        ),
        PointData => (
            "クリック位置の形式が不正です。",
            "A saved click position is invalid.",
        ),
        Coordinate => (
            "クリック位置の座標が不正です。",
            "A click coordinate is invalid.",
        ),
        SettingsBackup => (
            "旧設定を保全できません。",
            "Cannot back up the previous settings.",
        ),
        SettingsWrite => ("設定を保存できません。", "Cannot save settings."),
        EmptyPoints => (
            "先にクリック位置を記憶してください。",
            "Record click positions first.",
        ),
        SignalCreate => (
            "停止イベントを作成できません。",
            "Cannot create the stop signal.",
        ),
        TimerCreate => (
            "待機タイマーを作成できません。",
            "Cannot create the interval timer.",
        ),
        TimerSet => (
            "待機時間を設定できません。",
            "Cannot set the interval timer.",
        ),
        TimerWait => (
            "クリック間隔の待機に失敗しました。",
            "Waiting for the next click failed.",
        ),
        ThreadStart => (
            "クリック処理を開始できません。",
            "Cannot start the click worker.",
        ),
        WorkerPanic => (
            "クリック処理で予期しないエラーが発生しました。",
            "The click worker encountered an unexpected error.",
        ),
        ScreenUnavailable => (
            "画面の座標を取得できません。",
            "Cannot read the screen coordinates.",
        ),
        PointOutside => (
            "クリック位置が画面外です。再記録してください。",
            "A click position is outside the screen. Record it again.",
        ),
        ScreenChanged => (
            "画面構成が変わったため停止しました。位置を確認してください。",
            "Stopped because the monitor layout changed. Check the saved positions.",
        ),
        InputSend => (
            "マウス入力を送信できません。対象アプリの権限を確認してください。",
            "Cannot send mouse input. Check the target application's permissions.",
        ),
        BenchmarkRange => (
            "測定の範囲が不正です。",
            "The measurement range is invalid.",
        ),
        Other => (
            "処理を完了できません。",
            "The operation could not be completed.",
        ),
    };
    let base = if japanese { pair.0 } else { pair.1 };
    match &error.detail {
        Some(detail) => format!("{base}\n{detail}"),
        None => base.into(),
    }
}

#[derive(Clone, Copy)]
struct Control {
    hwnd: HWND,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}
struct App {
    hwnd: HWND,
    settings: Settings,
    path: PathBuf,
    controls: BTreeMap<u16, Control>,
    labels: Vec<Control>,
    font: HFONT,
    title_font: HFONT,
    scale: f64,
    scroll: i32,
    content_height: i32,
    tab: u8,
    recording: bool,
    recorded: Vec<Point>,
    runner: Option<Runner>,
    last: Progress,
    markers: Vec<HWND>,
    message: String,
    saved_error: bool,
    keyboard_hook: HHOOK,
    mouse_hook: HHOOK,
    capturing: Option<usize>,
    hotkeys_ready: bool,
    wheel_remainder: i32,
    main_offset: i32,
    selecting_area: bool,
}

impl App {
    fn t<'a>(&self, ja: &'a str, en: &'a str) -> &'a str {
        if self.settings.lang == "ja" { ja } else { en }
    }
    fn s(&self, v: i32) -> i32 {
        (v as f64 * self.scale).round() as i32
    }
    fn refresh_fonts(&mut self) {
        let (font, title) = make_fonts(self.scale);
        if font.is_null() || title.is_null() {
            unsafe {
                if !font.is_null() {
                    DeleteObject(font);
                }
                if !title.is_null() {
                    DeleteObject(title);
                }
            }
            return;
        }
        for control in self.controls.values().chain(self.labels.iter()) {
            unsafe {
                SendMessageW(control.hwnd, WM_SETFONT, font as usize, 1);
            }
        }
        if let Some(header) = self.labels.first() {
            unsafe {
                SendMessageW(header.hwnd, WM_SETFONT, title as usize, 1);
            }
        }
        unsafe {
            DeleteObject(self.font);
            DeleteObject(self.title_font);
        }
        self.font = font;
        self.title_font = title;
    }
    fn control(&self, id: u16) -> HWND {
        self.controls.get(&id).map_or(null_mut(), |c| c.hwnd)
    }
    fn busy(&self) -> bool {
        self.recording || self.runner.is_some()
    }
    fn create(
        &mut self,
        id: u16,
        class: *const u16,
        caption: &str,
        style: u32,
        bounds: [i32; 4],
    ) -> HWND {
        let [x, y, w, h] = bounds;
        let y = if self.tab == 0 && y >= 112 {
            y + self.main_offset
        } else {
            y
        };
        // All controls and font handles live on this UI thread and are destroyed before the fonts.
        let child = unsafe {
            CreateWindowExW(
                0,
                class,
                wide(caption).as_ptr(),
                WS_CHILD | WS_VISIBLE | style,
                self.s(x),
                self.s(y) - self.scroll,
                self.s(w),
                self.s(h),
                self.hwnd,
                id as usize as HMENU,
                GetModuleHandleW(null()),
                null(),
            )
        };
        unsafe {
            SendMessageW(child, WM_SETFONT, self.font as usize, 1);
        }
        let c = Control {
            hwnd: child,
            x,
            y,
            w,
            h,
        };
        if id != 0 {
            self.controls.insert(id, c);
        } else {
            self.labels.push(c);
        }
        child
    }
    fn label(&mut self, caption: &str, x: i32, y: i32, w: i32, h: i32) {
        self.create(0, w!("STATIC"), caption, STATIC_LEFT, [x, y, w, h]);
    }
    fn label_id(&mut self, id: u16, caption: &str, x: i32, y: i32, w: i32, h: i32) {
        self.create(id, w!("STATIC"), caption, STATIC_LEFT, [x, y, w, h]);
    }
    fn button(&mut self, id: u16, caption: &str, x: i32, y: i32, w: i32, h: i32) {
        if id == INDIVIDUAL || id == TOPMOST {
            let child = self.create(
                id,
                w!("BUTTON"),
                caption,
                WS_TABSTOP | BS_AUTOCHECKBOX as u32,
                [x, y, w, h],
            );
            let checked = if id == INDIVIDUAL {
                self.settings.per_point
            } else {
                self.settings.topmost
            };
            unsafe {
                SendMessageW(child, BM_SETCHECK, usize::from(checked), 0);
            }
            return;
        }
        self.create(
            id,
            w!("BUTTON"),
            caption,
            WS_TABSTOP | BS_OWNERDRAW as u32,
            [x, y, w, h],
        );
    }
    fn edit(&mut self, id: u16, caption: &str, x: i32, y: i32, w: i32, numeric: bool) {
        let h = self.create(
            id,
            w!("EDIT"),
            caption,
            WS_TABSTOP
                | WS_BORDER
                | ES_AUTOHSCROLL as u32
                | if numeric { ES_NUMBER as u32 } else { 0 },
            [x, y, w, 30],
        );
        unsafe {
            SendMessageW(h, EM_SETLIMITTEXT, 24, 0);
        }
    }
    fn enable(&self, id: u16, enabled: bool) {
        unsafe {
            EnableWindow(self.control(id), enabled as i32);
        }
    }
    fn save(&mut self) {
        if let Err(e) = self.settings.save(&self.path) {
            self.message = error_text(&e, self.settings.lang == "ja");
            self.saved_error = true;
        } else {
            self.saved_error = false;
        }
        set_text(self.control(MESSAGE), &self.message);
    }
    fn numbers(&mut self) -> Result<(), String> {
        if self.tab == 0 {
            let fast = model::parse_interval(&text(self.control(FAST_INTERVAL)))
                .map_err(|e| error_text(&e, self.settings.lang == "ja"))?;
            let step = text(self.control(AREA_STEP)).parse::<u32>().map_err(|_| {
                error_text(&ErrorCode::AreaInvalid.into(), self.settings.lang == "ja")
            })?;
            if !(1..=4096).contains(&step) {
                return Err(error_text(
                    &ErrorCode::AreaInvalid.into(),
                    self.settings.lang == "ja",
                ));
            }
            let mut corners = Vec::new();
            let mut present = 0;
            for id in [AREA_LEFT, AREA_TOP, AREA_RIGHT, AREA_BOTTOM] {
                let value = text(self.control(id));
                if !value.trim().is_empty() {
                    present += 1;
                }
                corners.push(value);
            }
            let region = if present == 0 {
                None
            } else if present == 4 {
                let values = corners
                    .iter()
                    .map(|s| s.trim().parse::<i32>())
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|_| {
                        error_text(&ErrorCode::AreaInvalid.into(), self.settings.lang == "ja")
                    })?;
                Some(ClickArea::from_corners(
                    (values[0], values[1]),
                    (values[2], values[3]),
                ))
            } else {
                return Err(error_text(
                    &ErrorCode::AreaInvalid.into(),
                    self.settings.lang == "ja",
                ));
            };
            let interval = model::parse_interval(&text(self.control(INTERVAL)))
                .map_err(|e| error_text(&e, self.settings.lang == "ja"))?;
            let repeat = text(self.control(REPEAT))
                .trim()
                .parse::<u32>()
                .map_err(|_| {
                    self.t("繰り返し回数は0〜999999です。", "Repeat must be 0–999999.")
                        .to_string()
                })?;
            if repeat > 999_999 {
                return Err(self
                    .t("繰り返し回数は0〜999999です。", "Repeat must be 0–999999.")
                    .into());
            }
            let mut overrides = vec![];
            if self.settings.details_open && self.settings.per_point && !self.recording {
                for i in 0..self.settings.points.len() {
                    overrides.push(
                        model::parse_interval(&text(self.control(INTERVAL_POINT + i as u16)))
                            .map_err(|e| error_text(&e, self.settings.lang == "ja"))?,
                    );
                }
            }
            self.settings.interval_ms = interval;
            self.settings.repeat = repeat;
            self.settings.fast_interval_ms = fast;
            self.settings.area_step_px = step;
            self.settings.area = region;
            for (p, ms) in self.settings.points.iter_mut().zip(overrides) {
                p.interval_ms = Some(ms);
            }
            set_text(self.control(INTERVAL), &model::format_interval(interval));
        } else {
            let seconds = text(self.control(DELAY))
                .trim()
                .parse::<u64>()
                .map_err(|_| {
                    self.t(
                        "開始待機は0〜30秒です。",
                        "Start delay must be 0–30 seconds.",
                    )
                    .to_string()
                })?;
            if seconds > 30 {
                return Err(self
                    .t(
                        "開始待機は0〜30秒です。",
                        "Start delay must be 0–30 seconds.",
                    )
                    .into());
            }
            self.settings.delay_ms = seconds * 1000;
        }
        Ok(())
    }
    fn commit(&mut self) -> bool {
        match self.numbers() {
            Ok(()) => {
                self.message.clear();
                self.save();
                !self.saved_error
            }
            Err(e) => {
                self.message = e;
                set_text(self.control(MESSAGE), &self.message);
                false
            }
        }
    }
    fn refresh_theme(&self) {
        let dark = if self.settings.theme == "system" {
            let mut light: u32 = 1;
            let mut len = size_of::<u32>() as u32;
            unsafe {
                RegGetValueW(
                    HKEY_CURRENT_USER,
                    w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
                    w!("AppsUseLightTheme"),
                    RRF_RT_REG_DWORD,
                    null_mut(),
                    (&mut light as *mut u32).cast(),
                    &mut len,
                );
            }
            light == 0
        } else {
            self.settings.theme == "dark"
        };
        DARK.store(dark as u32, Ordering::Relaxed);
        unsafe {
            let value = dark as i32;
            DwmSetWindowAttribute(self.hwnd, 20, (&value as *const i32).cast(), 4);
            InvalidateRect(self.hwnd, null(), 1);
        }
    }
    fn rebuild(&mut self) {
        ACTIVE_TAB.store(self.tab as u32, Ordering::Relaxed);
        for c in self.controls.values().chain(self.labels.iter()) {
            unsafe {
                DestroyWindow(c.hwnd);
            }
        }
        self.controls.clear();
        self.labels.clear();
        self.refresh_theme();
        self.label("Multi Point Clicker", 22, 17, 405, 35);
        if let Some(c) = self.labels.last() {
            unsafe {
                SendMessageW(c.hwnd, WM_SETFONT, self.title_font as usize, 1);
            }
        }
        let lang = self.t("日本語 → English", "English → 日本語").to_string();
        self.button(LANG, &lang, 440, 17, 185, 32);
        let main = self.t("メイン", "Main").to_string();
        let settings = self.t("設定", "Settings").to_string();
        self.button(NAV_MAIN, &main, 22, 63, 120, 34);
        self.button(NAV_SETTINGS, &settings, 148, 63, 120, 34);
        if self.tab == 0 {
            self.build_main();
        } else {
            self.build_settings();
        }
        self.position_controls();
        self.update_stats();
    }
    fn build_main(&mut self) {
        self.main_offset = 0;
        self.build_modes();
        self.main_offset = 190;
        self.label_id(STATUS, "", 35, 112, 580, 28);
        self.label_id(CLICKS, "", 35, 150, 180, 30);
        self.label_id(CYCLES, "", 233, 150, 180, 30);
        self.label_id(ELAPSED, "", 435, 150, 180, 30);
        let a = self.t("クリック数", "Clicks").to_string();
        let b = self.t("周回数", "Cycles").to_string();
        let c = self.t("経過時間", "Elapsed").to_string();
        self.label(&a, 35, 180, 180, 23);
        self.label(&b, 233, 180, 180, 23);
        self.label(&c, 435, 180, 180, 23);
        let captions = [
            self.t("記憶開始", "Start recording"),
            self.t("記憶終了", "Stop recording"),
            self.t("自動クリック開始", "Start clicking"),
            self.t("自動クリック終了", "Stop clicking"),
        ];
        let captions: Vec<_> = captions
            .iter()
            .enumerate()
            .map(|(i, s)| {
                format!(
                    "{s}\n{}",
                    format_hotkey(
                        self.settings
                            .hotkeys
                            .get(ACTIONS[i])
                            .map(String::as_str)
                            .unwrap_or("")
                    )
                )
            })
            .collect();
        self.button(RECORD, &captions[0], 22, 218, 294, 44);
        self.button(RECORD_STOP, &captions[1], 324, 218, 301, 44);
        self.button(START, &captions[2], 22, 270, 294, 44);
        self.button(STOP, &captions[3], 324, 270, 301, 44);
        let caption = self
            .t("クリック間隔（秒）", "Click interval (seconds)")
            .to_string();
        self.label(&caption, 30, 358, 240, 25);
        self.edit(
            INTERVAL,
            &model::format_interval(self.settings.interval_ms),
            320,
            353,
            160,
            false,
        );
        let reset = self.t("初期値に戻す", "Reset").to_string();
        self.button(DEFAULT_INTERVAL, &reset, 490, 353, 126, 30);
        let helper = self
            .t(
                "最小 0.001 秒・0.001 秒単位",
                "Minimum 0.001 s • step 0.001 s",
            )
            .to_string();
        self.label(&helper, 30, 389, 560, 23);
        let repeat = self
            .t(
                if matches!(self.settings.mode, ClickMode::Pointer | ClickMode::Drag) {
                    "連打は停止するまで継続"
                } else {
                    "繰り返し回数（0 = 無限）"
                },
                if matches!(self.settings.mode, ClickMode::Pointer | ClickMode::Drag) {
                    "Rapid click runs until stopped"
                } else {
                    "Repeat cycles (0 = forever)"
                },
            )
            .to_string();
        self.label(&repeat, 30, 425, 280, 25);
        self.edit(
            REPEAT,
            &self.settings.repeat.to_string(),
            320,
            419,
            160,
            true,
        );
        let details = format!(
            "{} {}",
            if self.settings.details_open {
                "▼"
            } else {
                "▶"
            },
            self.t("詳細設定", "Advanced")
        );
        self.button(DETAILS, &details, 22, 467, 603, 36);
        let mut y = 516;
        if self.settings.details_open {
            let mode = self
                .t(
                    "クリックごとに間隔を指定する",
                    "Use a separate interval for each click",
                )
                .to_string();
            self.button(INDIVIDUAL, &mode, 30, y, 585, 32);
            y += 42;
            let help = self
                .t(
                    "各位置をクリックした後、次の位置まで待つ秒数です。",
                    "Seconds from each click to the next (including the next cycle).",
                )
                .to_string();
            self.label(&help, 30, y, 580, 25);
            y += 34;
            let points = self.settings.points.clone();
            if points.is_empty() {
                let empty = self
                    .t(
                        "位置を記憶すると、ここで間隔を指定できます。",
                        "Record positions to configure their intervals here.",
                    )
                    .to_string();
                self.label(&empty, 35, y, 575, 28);
                y += 34;
            }
            for (i, p) in points.iter().enumerate() {
                let label = if i + 1 == points.len() {
                    format!("{} → 1 {}", i + 1, self.t("（次の周回）", "(next cycle)"))
                } else {
                    format!("{} → {}", i + 1, i + 2)
                };
                self.label(&label, 40, y + 4, 270, 25);
                self.edit(
                    INTERVAL_POINT + i as u16,
                    &model::format_interval(p.interval_ms.unwrap_or(self.settings.interval_ms)),
                    320,
                    y,
                    160,
                    false,
                );
                let sec = self.t("秒", "sec").to_string();
                self.label(&sec, 495, y + 5, 100, 25);
                self.enable(
                    INTERVAL_POINT + i as u16,
                    self.settings.per_point && !self.busy(),
                );
                y += 38;
            }
            y += 10;
        }
        let shown = if self.recording {
            self.recorded.clone()
        } else {
            self.settings.points.clone()
        };
        let title = format!(
            "{}  {}/10",
            self.t("クリック位置", "Click positions"),
            shown.len()
        );
        self.label(&title, 30, y + 6, 270, 28);
        let preview = self.t("位置を表示", "Show points").to_string();
        let clear = self.t("すべて削除", "Clear all").to_string();
        self.button(PREVIEW, &preview, 330, y, 135, 32);
        self.button(CLEAR, &clear, 477, y, 139, 32);
        y += 45;
        if shown.is_empty() {
            let s = self
                .t(
                    "「記憶開始」を押して、画面上をクリックしてください。",
                    "Start recording, then click the positions on your screen.",
                )
                .to_string();
            self.label(&s, 35, y, 575, 55);
            y += 65;
        }
        for (i, p) in shown.iter().enumerate() {
            self.label(
                &format!("{}     X {:>6}        Y {:>6}", i + 1, p.x, p.y),
                40,
                y + 5,
                455,
                28,
            );
            let delete = self.t("削除", "Delete").to_string();
            self.button(DELETE_POINT + i as u16, &delete, 515, y, 100, 30);
            self.enable(DELETE_POINT + i as u16, !self.busy());
            y += 39;
        }
        y += 13;
        let message = self.message.clone();
        self.label_id(MESSAGE, &message, 30, y, 585, 52);
        self.content_height = y + 70 + self.main_offset;
        self.enable(RECORD, !self.busy());
        self.enable(RECORD_STOP, self.recording);
        self.enable(START, !self.busy() && self.hotkeys_ready);
        self.enable(STOP, self.runner.is_some());
        for id in [INTERVAL, REPEAT, DEFAULT_INTERVAL, DETAILS, INDIVIDUAL] {
            self.enable(id, !self.busy());
        }
        self.enable(
            REPEAT,
            !self.busy() && !matches!(self.settings.mode, ClickMode::Pointer | ClickMode::Drag),
        );
        self.enable(
            INTERVAL,
            !self.busy() && self.settings.mode == ClickMode::Recorded,
        );
        self.enable(
            DEFAULT_INTERVAL,
            !self.busy() && self.settings.mode == ClickMode::Recorded,
        );
        self.enable(PREVIEW, !self.busy() && !self.settings.points.is_empty());
        self.enable(CLEAR, !self.busy() && !self.settings.points.is_empty());
    }
    fn build_modes(&mut self) {
        let label = self.t("動作", "Mode").to_string();
        self.label(&label, 30, 116, 100, 25);
        let combo = self.create(
            MODE,
            w!("COMBOBOX"),
            "",
            WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWNLIST as u32,
            [130, 112, 485, 180],
        );
        let modes = if self.settings.lang == "ja" {
            [
                "記憶した位置",
                "範囲内を均等にクリック",
                "ポインタ位置で連打",
                "ドラッグ中だけ連打",
            ]
        } else {
            [
                "Recorded positions",
                "Evenly cover a region",
                "Click at the pointer",
                "Click only while dragging",
            ]
        };
        for caption in modes {
            unsafe {
                SendMessageW(combo, CB_ADDSTRING, 0, wide(caption).as_ptr() as isize);
            }
        }
        let mode = match self.settings.mode {
            ClickMode::Recorded => 0,
            ClickMode::Area => 1,
            ClickMode::Pointer => 2,
            ClickMode::Drag => 3,
        };
        unsafe {
            SendMessageW(combo, CB_SETCURSEL, mode, 0);
        }
        // Combo drop-down height is larger than its collapsed layout rectangle.
        if let Some(control) = self.controls.get_mut(&MODE) {
            control.h = 180;
        }
        let select = self
            .t("範囲を指定（2か所）", "Select region (2 corners)")
            .to_string();
        self.button(AREA_SELECT, &select, 30, 151, 240, 30);
        let spacing = self.t("配置間隔 px", "Grid spacing px").to_string();
        self.label(&spacing, 290, 155, 160, 25);
        self.edit(
            AREA_STEP,
            &self.settings.area_step_px.to_string(),
            470,
            151,
            145,
            true,
        );
        let a = self.settings.area;
        for (id, title, x, value) in [
            (AREA_LEFT, "X1", 30, a.map(|a| a.left)),
            (AREA_TOP, "Y1", 180, a.map(|a| a.top)),
            (AREA_RIGHT, "X2", 330, a.map(|a| a.right)),
            (AREA_BOTTOM, "Y2", 480, a.map(|a| a.bottom)),
        ] {
            self.label(title, x, 193, 32, 25);
            self.edit(
                id,
                &value.map_or(String::new(), |v| v.to_string()),
                x + 32,
                187,
                100,
                false,
            );
        }
        let fast = self
            .t("連打間隔（秒）", "Rapid interval (seconds)")
            .to_string();
        self.label(&fast, 30, 232, 270, 25);
        self.edit(
            FAST_INTERVAL,
            &model::format_interval(self.settings.fast_interval_ms),
            320,
            226,
            160,
            false,
        );
        let locked = self.runner.as_ref().is_some_and(|r| r.snapshot().locked);
        let caption = if locked {
            self.t("ロック解除  Ctrl+Alt+L", "Unlock  Ctrl+Alt+L")
        } else {
            self.t("連打ロック  Ctrl+Alt+L", "Lock rapid click  Ctrl+Alt+L")
        }
        .to_string();
        self.button(LOCK, &caption, 30, 265, 285, 30);
        let hint = self
            .t(
                "停止: Ctrl+Alt+X（全モード共通）",
                "Stop: Ctrl+Alt+X (all modes)",
            )
            .to_string();
        self.label(&hint, 330, 270, 285, 25);
        for id in [
            MODE,
            AREA_SELECT,
            AREA_LEFT,
            AREA_TOP,
            AREA_RIGHT,
            AREA_BOTTOM,
            AREA_STEP,
            FAST_INTERVAL,
        ] {
            self.enable(id, !self.busy());
        }
        self.enable(
            LOCK,
            self.hotkeys_ready
                && !self.recording
                && (self.runner.is_none() || self.settings.mode == ClickMode::Drag),
        );
    }
    fn build_settings(&mut self) {
        let behavior = self.t("動作", "Behavior").to_string();
        self.label(&behavior, 30, 120, 565, 30);
        let delay = self
            .t(
                "開始までの待機時間（0〜30 秒）",
                "Start delay (0–30 seconds)",
            )
            .to_string();
        self.label(&delay, 35, 166, 350, 26);
        self.edit(
            DELAY,
            &(self.settings.delay_ms / 1000).to_string(),
            450,
            160,
            160,
            true,
        );
        let mouse = self.t("クリックするボタン", "Mouse button").to_string();
        self.label(&mouse, 35, 209, 350, 26);
        let value = if self.settings.button == "right" {
            self.t("右クリック", "Right")
        } else {
            self.t("左クリック", "Left")
        }
        .to_string();
        self.button(BUTTON, &value, 450, 203, 160, 32);
        let shortcuts = self.t("ショートカットキー", "Hotkeys").to_string();
        self.label(&shortcuts, 30, 266, 575, 29);
        let help = self
            .t(
                "ボタンを押してキーを入力。Escで取消。",
                "Click a binding and press your keys. Esc cancels.",
            )
            .to_string();
        self.label(&help, 35, 300, 575, 27);
        let names = [
            self.t("記憶開始", "Start recording"),
            self.t("記憶終了", "Stop recording"),
            self.t("自動クリック開始", "Start clicking"),
            self.t("自動クリック終了", "Stop clicking"),
        ];
        let names: Vec<_> = names.iter().map(|s| s.to_string()).collect();
        for i in 0..4 {
            let y = 340 + i as i32 * 43;
            self.label(&names[i], 35, y + 4, 280, 26);
            let v = if self.capturing == Some(i) {
                self.t("キーを押してください…", "Press keys…").to_string()
            } else {
                format_hotkey(&self.settings.hotkeys[ACTIONS[i]])
            };
            self.button(CAPTURE_KEY + i as u16, &v, 330, y, 280, 32);
            self.enable(CAPTURE_KEY + i as u16, !self.busy());
        }
        let reset = self
            .t("ショートカットを初期値に戻す", "Reset hotkeys")
            .to_string();
        self.button(RESET_KEYS, &reset, 35, 524, 575, 32);
        let appearance = self.t("表示", "Appearance").to_string();
        self.label(&appearance, 30, 587, 575, 28);
        let theme = self.t("テーマ", "Theme").to_string();
        self.label(&theme, 35, 628, 280, 26);
        let name = match self.settings.theme.as_str() {
            "dark" => self.t("ダーク", "Dark"),
            "light" => self.t("ライト", "Light"),
            _ => self.t("システム", "System"),
        }
        .to_string();
        self.button(THEME, &name, 330, 621, 280, 32);
        let topmost = self.t("常に手前に表示", "Always on top").to_string();
        self.button(TOPMOST, &topmost, 35, 672, 575, 32);
        let implementation = self
            .t(
                "クリック位置と間隔は自動で保存します。",
                "Positions and intervals are saved automatically.",
            )
            .to_string();
        self.label(&implementation, 35, 731, 575, 45);
        let message = self.message.clone();
        self.label_id(MESSAGE, &message, 35, 795, 575, 60);
        self.content_height = 875;
        for id in [DELAY, BUTTON, RESET_KEYS] {
            self.enable(id, !self.busy());
        }
    }
    fn position_controls(&mut self) {
        let mut rect: RECT = unsafe { zeroed() };
        unsafe {
            GetClientRect(self.hwnd, &mut rect);
        }
        let max = (self.s(self.content_height) - rect.bottom).max(0);
        self.scroll = self.scroll.clamp(0, max);
        for c in self.controls.values().chain(self.labels.iter()) {
            unsafe {
                SetWindowPos(
                    c.hwnd,
                    null_mut(),
                    self.s(c.x),
                    self.s(c.y) - self.scroll,
                    self.s(c.w),
                    self.s(c.h),
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }
        let info = SCROLLINFO {
            cbSize: size_of::<SCROLLINFO>() as u32,
            fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
            nMin: 0,
            nMax: self.s(self.content_height),
            nPage: rect.bottom.max(1) as u32,
            nPos: self.scroll,
            nTrackPos: 0,
        };
        unsafe {
            SetScrollInfo(self.hwnd, SB_VERT, &info, 1);
            InvalidateRect(self.hwnd, null(), 1);
        }
    }
    fn scroll_by(&mut self, delta: i32) {
        self.scroll = (self.scroll + delta).max(0);
        self.position_controls();
    }
    fn update_stats(&mut self) {
        if let Some(runner) = &self.runner {
            self.last = runner.snapshot();
        }
        if self.runner.as_ref().is_some_and(|r| r.finished()) {
            if let Some(mut runner) = self.runner.take() {
                runner.stop();
                self.last = runner.snapshot();
            }
            self.message = self
                .last
                .error
                .as_ref()
                .map(|e| error_text(e, self.settings.lang == "ja"))
                .unwrap_or_else(|| {
                    self.t("自動クリックが完了しました。", "Auto-clicking finished.")
                        .into()
                });
            self.rebuild();
            return;
        }
        let status = if self.recording {
            self.t(
                "記憶中：他の場所をクリックしてください",
                "Recording: click positions outside this window",
            )
            .to_string()
        } else if self.runner.is_some() && self.last.countdown_ms > 0 {
            format!(
                "{}  {:.1} {}",
                self.t("開始待機", "Starting in"),
                self.last.countdown_ms as f64 / 1000.,
                self.t("秒", "sec")
            )
        } else if self.runner.is_some() {
            if self.last.locked {
                self.t(
                    "連打ロック中（Ctrl+Alt+Lで解除）",
                    "Rapid click locked (Ctrl+Alt+L to unlock)",
                )
            } else if self.last.waiting_for_drag {
                self.t(
                    "ドラッグ待機中：左ボタンを押して動かしてください",
                    "Waiting: hold the left button and move to click",
                )
            } else {
                self.t("自動クリック中", "Auto-clicking")
            }
            .to_string()
        } else {
            self.t("待機中", "Idle").to_string()
        };
        set_text(self.control(STATUS), &status);
        set_text(self.control(CLICKS), &self.last.clicks.to_string());
        set_text(self.control(CYCLES), &self.last.cycles.to_string());
        let seconds = self.last.elapsed_ms / 1000;
        set_text(
            self.control(ELAPSED),
            &format!("{:02}:{:02}", seconds / 60, seconds % 60),
        );
        set_text(self.control(MESSAGE), &self.message);
        if self.runner.is_some() && self.settings.mode == ClickMode::Drag {
            let label = if self.last.locked {
                self.t("ロック解除  Ctrl+Alt+L", "Unlock  Ctrl+Alt+L")
            } else {
                self.t("連打ロック  Ctrl+Alt+L", "Lock rapid click  Ctrl+Alt+L")
            };
            set_text(self.control(LOCK), label);
        }
    }
    fn clear_markers(&mut self) {
        for hwnd in self.markers.drain(..) {
            unsafe {
                DestroyWindow(hwnd);
            }
        }
        unsafe {
            KillTimer(self.hwnd, PREVIEW_TIMER);
        }
    }
    fn show_markers(&mut self, points: &[Point]) {
        self.clear_markers();
        for (i, p) in points.iter().enumerate() {
            let size = self.s(32);
            let h = unsafe {
                CreateWindowExW(
                    WS_EX_TOPMOST
                        | WS_EX_TOOLWINDOW
                        | WS_EX_NOACTIVATE
                        | WS_EX_TRANSPARENT
                        | WS_EX_LAYERED,
                    w!("ClickerMarker"),
                    wide(&(i + 1).to_string()).as_ptr(),
                    WS_POPUP,
                    p.x - size / 2,
                    p.y - size / 2,
                    size,
                    size,
                    self.hwnd,
                    null_mut(),
                    GetModuleHandleW(null()),
                    null(),
                )
            };
            if !h.is_null() {
                unsafe {
                    let region = CreateEllipticRgn(0, 0, size, size);
                    if SetWindowRgn(h, region, 0) == 0 {
                        DeleteObject(region);
                    }
                    SetLayeredWindowAttributes(h, 0, 235, LWA_ALPHA);
                    ShowWindow(h, SW_SHOWNOACTIVATE);
                }
                self.markers.push(h);
            }
        }
    }
    fn record_point(&mut self, x: i32, y: i32) {
        if !self.recording || self.recorded.len() >= MAX_POINTS {
            return;
        }
        let mut r: RECT = unsafe { zeroed() };
        unsafe {
            GetWindowRect(self.hwnd, &mut r);
        }
        if x >= r.left && x < r.right && y >= r.top && y < r.bottom {
            return;
        }
        self.recorded.push(Point {
            x,
            y,
            interval_ms: None,
        });
        if self.selecting_area && self.recorded.len() == 2 {
            self.settings.area = Some(ClickArea::from_corners(
                (self.recorded[0].x, self.recorded[0].y),
                (x, y),
            ));
            self.recording = false;
            self.selecting_area = false;
            self.recorded.clear();
            if !self.mouse_hook.is_null() {
                unsafe {
                    UnhookWindowsHookEx(self.mouse_hook);
                }
                self.mouse_hook = null_mut();
            }
            self.clear_markers();
            self.message = self.t("範囲を保存しました。", "Region saved.").into();
            self.save();
            self.rebuild();
            return;
        }
        let points = self.recorded.clone();
        self.show_markers(&points);
        self.rebuild();
    }
    fn action(&mut self, id: u16) {
        if id == LOCK {
            if self.recording || !self.hotkeys_ready {
                return;
            }
            if let Some(runner) = &self.runner {
                if self.settings.mode == ClickMode::Drag {
                    runner.toggle_lock();
                    self.update_stats();
                }
                return;
            }
            if !self.commit() {
                return;
            }
            self.settings.mode = ClickMode::Drag;
            self.save();
            self.start_run(true);
            return;
        }
        if id == STOP {
            if let Some(mut r) = self.runner.take() {
                r.stop();
                self.last = r.snapshot();
                self.message = self
                    .t("自動クリックを停止しました。", "Auto-clicking stopped.")
                    .into();
                self.rebuild();
            }
            if self.recording {
                self.action(RECORD_STOP);
            }
            return;
        }
        if id == RECORD_STOP {
            if self.recording {
                self.recording = false;
                if !self.mouse_hook.is_null() {
                    unsafe {
                        UnhookWindowsHookEx(self.mouse_hook);
                    }
                    self.mouse_hook = null_mut();
                }
                self.clear_markers();
                if self.selecting_area {
                    self.selecting_area = false;
                    self.recorded.clear();
                    self.message = self
                        .t("範囲指定を取り消しました。", "Region selection cancelled.")
                        .into();
                } else if !self.recorded.is_empty() {
                    self.settings.points = std::mem::take(&mut self.recorded);
                    self.message = self
                        .t("クリック位置を保存しました。", "Positions saved.")
                        .into();
                    self.save();
                } else {
                    self.message = self
                        .t(
                            "記録がないため、以前の位置を保持しました。",
                            "No positions recorded; previous positions kept.",
                        )
                        .into();
                }
                self.rebuild();
            }
            return;
        }
        if id == LANG {
            if !self.commit() {
                return;
            }
            self.settings.lang = if self.settings.lang == "ja" {
                "en"
            } else {
                "ja"
            }
            .into();
            self.save();
            self.rebuild();
            return;
        }
        if id == NAV_MAIN || id == NAV_SETTINGS {
            if !self.busy() && !self.commit() {
                return;
            }
            self.cancel_capture();
            self.tab = if id == NAV_MAIN { 0 } else { 1 };
            self.scroll = 0;
            self.wheel_remainder = 0;
            self.rebuild();
            return;
        }
        if id == THEME {
            if !self.busy() && !self.commit() {
                self.sync_checks();
                return;
            }
            self.settings.theme = match self.settings.theme.as_str() {
                "system" => "dark",
                "dark" => "light",
                _ => "system",
            }
            .into();
            self.save();
            self.rebuild();
            return;
        }
        if id == TOPMOST {
            if !self.busy() && !self.commit() {
                self.sync_checks();
                return;
            }
            self.settings.topmost = !self.settings.topmost;
            unsafe {
                SetWindowPos(
                    self.hwnd,
                    if self.settings.topmost {
                        HWND_TOPMOST
                    } else {
                        HWND_NOTOPMOST
                    },
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
            self.save();
            self.rebuild();
            return;
        }
        if self.busy() {
            return;
        }
        if id == MODE {
            let selection = unsafe { SendMessageW(self.control(MODE), CB_GETCURSEL, 0, 0) };
            if !self.commit() {
                self.rebuild();
                return;
            }
            self.settings.mode = match selection {
                1 => ClickMode::Area,
                2 => ClickMode::Pointer,
                3 => ClickMode::Drag,
                _ => ClickMode::Recorded,
            };
            self.scroll = 0;
            self.save();
            self.rebuild();
            return;
        }
        if id == DEFAULT_INTERVAL {
            let previous_common = self.settings.interval_ms;
            self.settings.interval_ms = model::DEFAULT_INTERVAL_MS;
            set_text(self.control(INTERVAL), "0.501");
            for (i, point) in self.settings.points.iter().enumerate() {
                let editor = self.control(INTERVAL_POINT + i as u16);
                if follows_common_interval(point.interval_ms, &text(editor), previous_common) {
                    set_text(editor, "0.501");
                }
            }
            self.message.clear();
            self.save();
            return;
        }
        if !self.commit() {
            self.sync_checks();
            return;
        }
        self.cancel_capture();
        match id {
            RECORD | AREA_SELECT => {
                self.mouse_hook = unsafe {
                    SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), GetModuleHandleW(null()), 0)
                };
                if self.mouse_hook.is_null() {
                    self.message = self
                        .t(
                            "位置記録のマウス監視を開始できません。",
                            "Cannot start mouse recording.",
                        )
                        .into();
                    set_text(self.control(MESSAGE), &self.message);
                    return;
                }
                self.clear_markers();
                self.recorded.clear();
                self.selecting_area = id == AREA_SELECT;
                if self.selecting_area {
                    self.settings.mode = ClickMode::Area;
                    self.message = self
                        .t(
                            "範囲の2隅をクリックしてください。取消: Ctrl+Alt+X",
                            "Click two opposite corners. Cancel: Ctrl+Alt+X",
                        )
                        .into();
                }
                self.recording = true;
                if !self.selecting_area {
                    self.message.clear();
                }
                self.rebuild();
            }
            START => {
                if !self.hotkeys_ready {
                    self.message = self
                        .t(
                            "停止キーを含むショートカットを設定してください。",
                            "Configure working hotkeys, including Stop, before running.",
                        )
                        .into();
                    set_text(self.control(MESSAGE), &self.message);
                    return;
                }
                if self.settings.mode == ClickMode::Recorded && self.settings.points.is_empty() {
                    self.message = self
                        .t(
                            "先にクリック位置を記憶してください。",
                            "Record click positions first.",
                        )
                        .into();
                    set_text(self.control(MESSAGE), &self.message);
                    return;
                }
                self.start_run(false);
            }
            DETAILS => {
                self.settings.details_open = !self.settings.details_open;
                self.save();
                self.rebuild();
            }
            INDIVIDUAL => {
                self.settings.per_point = !self.settings.per_point;
                self.save();
                self.rebuild();
            }
            PREVIEW => {
                let points = self.settings.points.clone();
                self.show_markers(&points);
                unsafe {
                    SetTimer(self.hwnd, PREVIEW_TIMER, 2500, None);
                }
            }
            CLEAR => {
                self.settings.points.clear();
                self.clear_markers();
                self.save();
                self.rebuild();
            }
            BUTTON => {
                self.settings.button = if self.settings.button == "left" {
                    "right"
                } else {
                    "left"
                }
                .into();
                self.save();
                self.rebuild();
            }
            RESET_KEYS => {
                let old = self.settings.hotkeys.clone();
                self.settings.hotkeys = model::default_hotkeys();
                if let Err(e) = self.register_hotkeys() {
                    self.settings.hotkeys = old;
                    let _ = self.register_hotkeys();
                    self.message = e;
                }
                self.save();
                self.rebuild();
            }
            id if (DELETE_POINT..DELETE_POINT + MAX_POINTS as u16).contains(&id) => {
                let i = (id - DELETE_POINT) as usize;
                if i < self.settings.points.len() {
                    self.settings.points.remove(i);
                    self.clear_markers();
                    self.save();
                    self.rebuild();
                }
            }
            id if (CAPTURE_KEY..CAPTURE_KEY + 4).contains(&id) => {
                let i = (id - CAPTURE_KEY) as usize;
                self.keyboard_hook = unsafe {
                    SetWindowsHookExW(
                        WH_KEYBOARD_LL,
                        Some(keyboard_hook),
                        GetModuleHandleW(null()),
                        0,
                    )
                };
                if self.keyboard_hook.is_null() {
                    self.message = self
                        .t(
                            "キーの入力を監視できません。",
                            "Cannot capture keyboard input.",
                        )
                        .into();
                    set_text(self.control(MESSAGE), &self.message);
                    return;
                }
                self.capturing = Some(i);
                CAPTURE.store((i + 1) as u32, Ordering::Relaxed);
                self.rebuild();
            }
            _ => {}
        }
    }
    fn start_run(&mut self, locked: bool) {
        self.clear_markers();
        self.cancel_capture();
        match Runner::start(self.settings.clone()) {
            Ok(runner) => {
                if locked {
                    runner.toggle_lock();
                }
                self.runner = Some(runner);
                self.last = Progress::default();
                self.message.clear();
                self.rebuild();
            }
            Err(error) => {
                self.message = error_text(&error, self.settings.lang == "ja");
                self.rebuild();
            }
        }
    }
    fn register_hotkeys(&mut self) -> Result<(), String> {
        self.hotkeys_ready = false;
        for i in 0..5 {
            unsafe {
                UnregisterHotKey(self.hwnd, i + 1);
            }
        }
        if unsafe {
            RegisterHotKey(
                self.hwnd,
                LOCK_HOTKEY,
                MOD_CONTROL | MOD_ALT | MOD_NOREPEAT,
                b'L' as u32,
            )
        } == 0
        {
            return Err(self
                .t(
                    "ロックキーCtrl+Alt+Lを登録できません。",
                    "Cannot register Ctrl+Alt+L for rapid click lock.",
                )
                .into());
        }
        for (i, action) in ACTIONS.iter().enumerate() {
            let Some((mods, key)) = parse_hotkey(&self.settings.hotkeys[*action]) else {
                for id in 1..=5 {
                    unsafe {
                        UnregisterHotKey(self.hwnd, id);
                    }
                }
                return Err(format!(
                    "{}: {}",
                    self.t("ショートカットの形式が不正です", "Invalid hotkey"),
                    self.settings.hotkeys[*action]
                ));
            };
            if unsafe { RegisterHotKey(self.hwnd, i as i32 + 1, mods | MOD_NOREPEAT, key) } == 0 {
                for j in 0..5 {
                    unsafe {
                        UnregisterHotKey(self.hwnd, j + 1);
                    }
                }
                return Err(format!(
                    "{}: {}",
                    self.t(
                        "ショートカットを登録できません（他のアプリで使用中）",
                        "Cannot register hotkey (already in use)"
                    ),
                    format_hotkey(&self.settings.hotkeys[*action])
                ));
            }
        }
        self.hotkeys_ready = true;
        Ok(())
    }
    fn captured(&mut self, mods: u32, key: u32) {
        let Some(i) = self.capturing.take() else {
            return;
        };
        CAPTURE.store(0, Ordering::Relaxed);
        if !self.keyboard_hook.is_null() {
            unsafe {
                UnhookWindowsHookEx(self.keyboard_hook);
            }
            self.keyboard_hook = null_mut();
        }
        if key == VK_ESCAPE as u32 {
            self.rebuild();
            return;
        }
        let Some(combo) = combo_from_key(mods, key) else {
            self.rebuild();
            return;
        };
        if mods == MOD_CONTROL | MOD_ALT && key == b'L' as u32 {
            self.message = self
                .t(
                    "Ctrl+Alt+Lは連打ロック専用です。",
                    "Ctrl+Alt+L is reserved for rapid click lock.",
                )
                .into();
            self.rebuild();
            return;
        }
        if self
            .settings
            .hotkeys
            .iter()
            .any(|(a, k)| a != ACTIONS[i] && parse_hotkey(k) == Some((mods, key)))
        {
            self.message = self
                .t(
                    "そのキーは別の操作で使用中です。",
                    "That hotkey is already assigned.",
                )
                .into();
            self.rebuild();
            return;
        }
        let old = self.settings.hotkeys[ACTIONS[i]].clone();
        self.settings.hotkeys.insert(ACTIONS[i].into(), combo);
        if let Err(e) = self.register_hotkeys() {
            self.settings.hotkeys.insert(ACTIONS[i].into(), old);
            let _ = self.register_hotkeys();
            self.message = e;
        } else {
            self.message = self
                .t("ショートカットを更新しました。", "Hotkey updated.")
                .into();
            self.save();
        }
        self.rebuild();
    }
    fn cancel_capture(&mut self) {
        self.capturing = None;
        CAPTURE.store(0, Ordering::Relaxed);
        if !self.keyboard_hook.is_null() {
            unsafe {
                UnhookWindowsHookEx(self.keyboard_hook);
            }
            self.keyboard_hook = null_mut();
        }
    }
    fn sync_checks(&self) {
        unsafe {
            SendMessageW(
                self.control(INDIVIDUAL),
                BM_SETCHECK,
                usize::from(self.settings.per_point),
                0,
            );
            SendMessageW(
                self.control(TOPMOST),
                BM_SETCHECK,
                usize::from(self.settings.topmost),
                0,
            );
        }
    }
}

fn follows_common_interval(saved: Option<u64>, editor: &str, common: u64) -> bool {
    saved.is_none() && editor == model::format_interval(common)
}
fn wheel_steps(remainder: &mut i32, delta: i32) -> i32 {
    let total = *remainder + delta;
    *remainder = total % 120;
    total / 120
}
fn repair_hotkeys(settings: &mut Settings) -> bool {
    let mut used = HashSet::new();
    used.insert((MOD_CONTROL | MOD_ALT, b'L' as u32));
    let mut invalid = vec![];
    for action in ACTIONS {
        match settings.hotkeys.get(action).and_then(|v| parse_hotkey(v)) {
            Some(binding) if used.insert(binding) => {}
            _ => invalid.push(action),
        }
    }
    if invalid.is_empty() {
        return false;
    }
    let defaults = model::default_hotkeys();
    for action in invalid {
        let candidate = std::iter::once(&defaults[action])
            .chain(ACTIONS.iter().map(|a| &defaults[*a]))
            .find(|key| parse_hotkey(key).is_some_and(|binding| !used.contains(&binding)))
            .expect("four defaults suffice for four actions")
            .clone();
        used.insert(parse_hotkey(&candidate).unwrap());
        settings.hotkeys.insert(action.into(), candidate);
    }
    true
}
fn make_fonts(scale: f64) -> (HFONT, HFONT) {
    let create = |size: f64, weight: i32| unsafe {
        CreateFontW(
            -(size * scale).round() as i32,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            0,
            0,
            CLEARTYPE_QUALITY as u32,
            0,
            w!("Yu Gothic UI"),
        )
    };
    (
        create(15., FW_NORMAL as i32),
        create(18., FW_SEMIBOLD as i32),
    )
}
fn format_hotkey(s: &str) -> String {
    s.split('+')
        .map(|s| match s {
            "ctrl" => "Ctrl".into(),
            "alt" => "Alt".into(),
            "shift" => "Shift".into(),
            "win" => "Win".into(),
            _ => s.to_uppercase(),
        })
        .collect::<Vec<_>>()
        .join(" + ")
}
fn parse_hotkey(s: &str) -> Option<(u32, u32)> {
    let parts: Vec<_> = s.split('+').collect();
    let mut mods = 0;
    for part in &parts[..parts.len().saturating_sub(1)] {
        mods |= match *part {
            "ctrl" => MOD_CONTROL,
            "alt" => MOD_ALT,
            "shift" => MOD_SHIFT,
            "win" => MOD_WIN,
            _ => return None,
        };
    }
    let token = *parts.last()?;
    let key = match token {
        "space" => VK_SPACE as u32,
        "backspace" => VK_BACK as u32,
        "page_up" => VK_PRIOR as u32,
        "page_down" => VK_NEXT as u32,
        "pause" => VK_PAUSE as u32,
        "print_screen" => VK_SNAPSHOT as u32,
        "caps_lock" => VK_CAPITAL as u32,
        "num_lock" => VK_NUMLOCK as u32,
        "scroll_lock" => VK_SCROLL as u32,
        "menu" => VK_APPS as u32,
        "decimal" => VK_DECIMAL as u32,
        "add" => VK_ADD as u32,
        "subtract" => VK_SUBTRACT as u32,
        "multiply" => VK_MULTIPLY as u32,
        "divide" => VK_DIVIDE as u32,
        "enter" => VK_RETURN as u32,
        "esc" => VK_ESCAPE as u32,
        "tab" => VK_TAB as u32,
        "home" => VK_HOME as u32,
        "end" => VK_END as u32,
        "delete" => VK_DELETE as u32,
        "insert" => VK_INSERT as u32,
        "up" => VK_UP as u32,
        "down" => VK_DOWN as u32,
        "left" => VK_LEFT as u32,
        "right" => VK_RIGHT as u32,
        _ => {
            if token.len() == 1 && token.as_bytes()[0].is_ascii_alphanumeric() {
                token.as_bytes()[0].to_ascii_uppercase() as u32
            } else if let Some(n) = token
                .strip_prefix('f')
                .and_then(|x| x.parse::<u32>().ok())
                .filter(|n| (1..=24).contains(n))
            {
                VK_F1 as u32 + n - 1
            } else if let Some(n) = token
                .strip_prefix("num")
                .and_then(|x| x.parse::<u32>().ok())
                .filter(|n| *n <= 9)
            {
                VK_NUMPAD0 as u32 + n
            } else {
                if token.chars().count() == 1 {
                    let c = token.chars().next()? as u32;
                    if c <= u16::MAX as u32 {
                        let scan = unsafe { VkKeyScanW(c as u16) };
                        if scan != -1 {
                            let flags = (scan as u16 >> 8) as u32;
                            mods |= if flags & 1 != 0 { MOD_SHIFT } else { 0 };
                            mods |= if flags & 2 != 0 { MOD_CONTROL } else { 0 };
                            mods |= if flags & 4 != 0 { MOD_ALT } else { 0 };
                            return Some((mods, (scan as u16 & 0xff) as u32));
                        }
                    }
                }
                token
                    .strip_prefix("vk")
                    .and_then(|x| x.parse::<u32>().ok())
                    .filter(|n| (1..=254).contains(n))?
            }
        }
    };
    Some((mods, key))
}
fn combo_from_key(mods: u32, key: u32) -> Option<String> {
    let mut parts = vec![];
    for (flag, name) in [
        (MOD_CONTROL, "ctrl"),
        (MOD_ALT, "alt"),
        (MOD_SHIFT, "shift"),
        (MOD_WIN, "win"),
    ] {
        if mods & flag != 0 {
            parts.push(name.to_string());
        }
    }
    let token = if (0x30..=0x39).contains(&key) || (0x41..=0x5A).contains(&key) {
        char::from_u32(key)?.to_ascii_lowercase().to_string()
    } else if (VK_F1 as u32..=VK_F24 as u32).contains(&key) {
        format!("f{}", key - VK_F1 as u32 + 1)
    } else if (VK_NUMPAD0 as u32..=VK_NUMPAD9 as u32).contains(&key) {
        format!("num{}", key - VK_NUMPAD0 as u32)
    } else {
        format!("vk{key}")
    };
    parts.push(token);
    Some(parts.join("+"))
}

unsafe extern "system" fn mouse_hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 && wp == WM_LBUTTONDOWN as usize {
        // WH_MOUSE_LL supplies MSLLHOOKSTRUCT for the duration of this callback.
        let event = unsafe { &*(lp as *const MSLLHOOKSTRUCT) };
        // Recording and playback are mutually exclusive. Accessibility-generated
        // clicks are valid recording input too; no synthetic input is discarded.
        {
            let root = ROOT.load(Ordering::Relaxed) as HWND;
            if !root.is_null() {
                unsafe {
                    PostMessageW(root, APP_RECORD, event.pt.x as usize, event.pt.y as isize);
                }
            }
        }
    }
    unsafe { CallNextHookEx(null_mut(), code, wp, lp) }
}
unsafe extern "system" fn keyboard_hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0
        && CAPTURE.load(Ordering::Relaxed) != 0
        && (wp == WM_KEYDOWN as usize || wp == WM_SYSKEYDOWN as usize)
    {
        let event = unsafe { &*(lp as *const KBDLLHOOKSTRUCT) };
        let key = event.vkCode;
        if ![
            VK_LCONTROL,
            VK_RCONTROL,
            VK_CONTROL,
            VK_LMENU,
            VK_RMENU,
            VK_MENU,
            VK_LSHIFT,
            VK_RSHIFT,
            VK_SHIFT,
            VK_LWIN,
            VK_RWIN,
        ]
        .iter()
        .any(|k| *k as u32 == key)
        {
            let mut mods = 0;
            for (vk, flag) in [
                (VK_CONTROL, MOD_CONTROL),
                (VK_MENU, MOD_ALT),
                (VK_SHIFT, MOD_SHIFT),
                (VK_LWIN, MOD_WIN),
                (VK_RWIN, MOD_WIN),
            ] {
                if unsafe { GetAsyncKeyState(vk as i32) } < 0 {
                    mods |= flag;
                }
            }
            CAPTURE.store(0, Ordering::Relaxed);
            unsafe {
                PostMessageW(
                    ROOT.load(Ordering::Relaxed) as HWND,
                    APP_CAPTURE,
                    mods as usize,
                    key as isize,
                );
            }
            return 1;
        }
    }
    unsafe { CallNextHookEx(null_mut(), code, wp, lp) }
}

unsafe extern "system" fn marker_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_NCHITTEST {
        return HTTRANSPARENT as isize;
    }
    if msg == WM_PAINT {
        unsafe {
            let mut ps: PAINTSTRUCT = zeroed();
            let dc = BeginPaint(hwnd, &mut ps);
            let mut r: RECT = zeroed();
            GetClientRect(hwnd, &mut r);
            let brush = CreateSolidBrush(color(37, 99, 235));
            FillRect(dc, &r, brush);
            DeleteObject(brush);
            SetBkMode(dc, TRANSPARENT as i32);
            SetTextColor(dc, color(255, 255, 255));
            let content = wide(&text(hwnd));
            DrawTextW(
                dc,
                content.as_ptr(),
                content.len() as i32 - 1,
                &mut r,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
            EndPaint(hwnd, &ps);
        }
        return 0;
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_GETMINMAXINFO {
        let scale = unsafe { GetDpiForWindow(hwnd) }.max(96) as f64 / 96.;
        let limits = unsafe { &mut *(lp as *mut MINMAXINFO) };
        limits.ptMinTrackSize.x = (650. * scale) as i32;
        limits.ptMinTrackSize.y = (450. * scale) as i32;
        return 0;
    }
    // Painting can reenter during CreateWindow/DestroyWindow. It never borrows App.
    if msg == WM_DESTROY {
        ROOT.store(0, Ordering::Relaxed);
        CAPTURE.store(0, Ordering::Relaxed);
        unsafe {
            PostQuitMessage(0);
        }
        return 0;
    }
    if msg == WM_CTLCOLORSTATIC || msg == WM_CTLCOLOREDIT || msg == WM_CTLCOLORBTN {
        unsafe {
            let dc = wp as HDC;
            SetTextColor(dc, fg());
            SetBkColor(dc, bg());
            SetDCBrushColor(dc, bg());
            return GetStockObject(DC_BRUSH) as isize;
        }
    }
    if msg == WM_ERASEBKGND {
        unsafe {
            let mut r: RECT = zeroed();
            GetClientRect(hwnd, &mut r);
            let brush = CreateSolidBrush(bg());
            FillRect(wp as HDC, &r, brush);
            DeleteObject(brush);
        }
        return 1;
    }
    if msg == WM_PAINT {
        unsafe {
            let mut ps: PAINTSTRUCT = zeroed();
            let dc = BeginPaint(hwnd, &mut ps);
            let brush = CreateSolidBrush(bg());
            FillRect(dc, &ps.rcPaint, brush);
            DeleteObject(brush);
            EndPaint(hwnd, &ps);
        }
        return 0;
    }
    if msg == WM_DRAWITEM {
        unsafe {
            let d = &*(lp as *const DRAWITEMSTRUCT);
            let id = d.CtlID as u16;
            let disabled = d.itemState & ODS_DISABLED != 0;
            let pressed = d.itemState & ODS_SELECTED != 0;
            let mut fill = if disabled {
                panel()
            } else {
                match id {
                    START => color(0, 103, 192),
                    STOP => color(150, 55, 55),
                    _ => panel(),
                }
            };
            if pressed && !disabled {
                fill = color(47, 107, 187);
            }
            let brush = CreateSolidBrush(fill);
            let old = SelectObject(d.hDC, brush);
            let pen = CreatePen(
                PS_SOLID,
                1,
                if DARK.load(Ordering::Relaxed) != 0 {
                    color(78, 78, 78)
                } else {
                    color(180, 180, 180)
                },
            );
            let oldpen = SelectObject(d.hDC, pen);
            Rectangle(
                d.hDC,
                d.rcItem.left,
                d.rcItem.top,
                d.rcItem.right,
                d.rcItem.bottom,
            );
            SelectObject(d.hDC, old);
            SelectObject(d.hDC, oldpen);
            DeleteObject(brush);
            DeleteObject(pen);
            let control_font = SendMessageW(d.hwndItem, WM_GETFONT, 0, 0);
            if control_font != 0 {
                SelectObject(d.hDC, control_font as HGDIOBJ);
            }
            SetBkMode(d.hDC, TRANSPARENT as i32);
            SetTextColor(
                d.hDC,
                if disabled {
                    muted()
                } else if matches!(id, START | STOP) {
                    color(255, 255, 255)
                } else {
                    fg()
                },
            );
            let mut rect = d.rcItem;
            rect.left += 5;
            rect.right -= 5;
            let caption = wide(&text(d.hwndItem));
            DrawTextW(
                d.hDC,
                caption.as_ptr(),
                caption.len() as i32 - 1,
                &mut rect,
                DT_CENTER | DT_VCENTER | DT_WORDBREAK,
            );
            if (id == NAV_MAIN && ACTIVE_TAB.load(Ordering::Relaxed) == 0)
                || (id == NAV_SETTINGS && ACTIVE_TAB.load(Ordering::Relaxed) == 1)
            {
                let underline = RECT {
                    left: d.rcItem.left + 1,
                    top: d.rcItem.bottom - 3,
                    right: d.rcItem.right - 1,
                    bottom: d.rcItem.bottom - 1,
                };
                let accent = CreateSolidBrush(color(0, 120, 212));
                FillRect(d.hDC, &underline, accent);
                DeleteObject(accent);
            }
            if d.itemState & ODS_FOCUS != 0 {
                InflateRect(&mut rect, -3, -3);
                DrawFocusRect(d.hDC, &rect);
            }
            return 1;
        }
    }
    let handled = APP.with(|cell| {
        let Ok(mut guard) = cell.try_borrow_mut() else {
            return false;
        };
        let Some(app) = guard.as_mut() else {
            return false;
        };
        match msg {
            APP_INIT => {
                app.rebuild();
                true
            }
            APP_RECORD => {
                app.record_point(wp as i32, lp as i32);
                true
            }
            APP_CAPTURE => {
                app.captured(wp as u32, lp as u32);
                true
            }
            WM_COMMAND => {
                let id = (wp & 0xffff) as u16;
                let notification = ((wp >> 16) & 0xffff) as u32;
                if notification == BN_CLICKED || (id == MODE && notification == CBN_SELCHANGE) {
                    app.action(id);
                }
                true
            }
            WM_HOTKEY => {
                if CAPTURE.load(Ordering::Relaxed) == 0 {
                    if wp == LOCK_HOTKEY as usize {
                        app.action(LOCK);
                        return true;
                    }
                    if let Some(action) =
                        [RECORD, RECORD_STOP, START, STOP].get(wp.saturating_sub(1))
                    {
                        app.action(*action);
                    }
                }
                true
            }
            WM_TIMER => {
                if wp == TICK {
                    app.update_stats();
                } else if wp == PREVIEW_TIMER {
                    app.clear_markers();
                }
                true
            }
            WM_SIZE => {
                app.position_controls();
                true
            }
            WM_MOUSEWHEEL => {
                let steps = wheel_steps(&mut app.wheel_remainder, (wp >> 16) as u16 as i16 as i32);
                app.scroll_by(-steps * app.s(65));
                true
            }
            WM_VSCROLL => {
                let code = (wp & 0xffff) as i32;
                let mut info: SCROLLINFO = unsafe { zeroed() };
                info.cbSize = size_of::<SCROLLINFO>() as u32;
                info.fMask = SIF_ALL;
                unsafe {
                    GetScrollInfo(hwnd, SB_VERT, &mut info);
                }
                match code {
                    SB_LINEUP => app.scroll_by(-app.s(35)),
                    SB_LINEDOWN => app.scroll_by(app.s(35)),
                    SB_PAGEUP => app.scroll_by(-(info.nPage as i32)),
                    SB_PAGEDOWN => app.scroll_by(info.nPage as i32),
                    SB_THUMBTRACK | SB_THUMBPOSITION => {
                        app.scroll = info.nTrackPos;
                        app.position_controls();
                    }
                    _ => {}
                }
                true
            }
            WM_DPICHANGED => {
                app.scale = (wp & 0xffff) as f64 / 96.;
                app.refresh_fonts();
                let rect = unsafe { &*(lp as *const RECT) };
                unsafe {
                    SetWindowPos(
                        hwnd,
                        null_mut(),
                        rect.left,
                        rect.top,
                        rect.right - rect.left,
                        rect.bottom - rect.top,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
                app.position_controls();
                true
            }
            WM_CLOSE => {
                app.cancel_capture();
                if let Some(mut r) = app.runner.take() {
                    r.stop();
                }
                if !app.recording {
                    let _ = app.numbers();
                }
                app.save();
                if app.saved_error {
                    let prompt = format!(
                        "{}\n\n{}",
                        app.message,
                        app.t(
                            "変更を保存せずに終了しますか？",
                            "Exit without saving these changes?"
                        )
                    );
                    if unsafe {
                        MessageBoxW(
                            hwnd,
                            wide(&prompt).as_ptr(),
                            w!("Multi Point Clicker"),
                            MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
                        )
                    } != IDYES
                    {
                        app.rebuild();
                        return true;
                    }
                }
                unsafe {
                    DestroyWindow(hwnd);
                }
                true
            }
            _ => false,
        }
    });
    if handled {
        0
    } else {
        unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
    }
}

pub fn run() {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let instance = GetModuleHandleW(null());
        let mutex = CreateMutexW(null(), 0, w!("Local\\MultiPointClickerRust"));
        if mutex.is_null() {
            let japanese =
                windows_sys::Win32::Globalization::GetUserDefaultUILanguage() & 0x3ff == 0x11;
            error_dialog(
                null_mut(),
                if japanese {
                    "アプリの起動ロックを作成できません。"
                } else {
                    "Cannot create the startup lock."
                },
            );
            return;
        }
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let existing = FindWindowW(w!("MultiPointClickerRust"), null());
            if !existing.is_null() {
                ShowWindow(existing, SW_RESTORE);
                SetForegroundWindow(existing);
            }
            CloseHandle(mutex);
            return;
        }
        let path = model::settings_path();
        let new_settings = !path.exists();
        let (mut settings, migrated) = match Settings::load(&path) {
            Ok(s) => s,
            Err(e) => {
                error_dialog(
                    null_mut(),
                    &error_text(
                        &e,
                        windows_sys::Win32::Globalization::GetUserDefaultUILanguage() & 0x3ff
                            == 0x11,
                    ),
                );
                CloseHandle(mutex);
                return;
            }
        };
        if new_settings {
            settings.lang =
                if windows_sys::Win32::Globalization::GetUserDefaultUILanguage() & 0x3ff == 0x11 {
                    "ja"
                } else {
                    "en"
                }
                .into();
        }
        let cursor = LoadCursorW(null_mut(), IDC_ARROW);
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wnd_proc),
            hInstance: instance,
            lpszClassName: w!("MultiPointClickerRust"),
            hCursor: cursor,
            // MAKEINTRESOURCEW(1): Win32 interprets this value as an ordinal,
            // never as a dereferenceable Rust pointer.
            hIcon: LoadIconW(instance, std::ptr::without_provenance(1)),
            ..zeroed()
        };
        RegisterClassW(&class);
        let marker = WNDCLASSW {
            lpfnWndProc: Some(marker_proc),
            hInstance: instance,
            lpszClassName: w!("ClickerMarker"),
            hCursor: cursor,
            ..zeroed()
        };
        RegisterClassW(&marker);
        let dpi = GetDpiForSystem().max(96);
        let scale = dpi as f64 / 96.;
        let desktop_h = GetSystemMetrics(SM_CYSCREEN);
        let desktop_w = GetSystemMetrics(SM_CXSCREEN);
        let width = (672. * scale) as i32;
        let height = ((930. * scale) as i32).min(desktop_h - 80).max(450);
        let hwnd = CreateWindowExW(
            if settings.topmost { WS_EX_TOPMOST } else { 0 },
            w!("MultiPointClickerRust"),
            w!("Multi Point Clicker"),
            WS_OVERLAPPEDWINDOW | WS_VSCROLL | WS_CLIPCHILDREN,
            (desktop_w - width) / 2,
            (desktop_h - height) / 2,
            width,
            height,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if hwnd.is_null() {
            error_dialog(
                null_mut(),
                if settings.lang == "ja" {
                    "アプリのウィンドウを作成できません。"
                } else {
                    "Cannot create the application window."
                },
            );
            CloseHandle(mutex);
            return;
        }
        let small_icon = LoadImageW(
            instance,
            std::ptr::without_provenance(1),
            IMAGE_ICON,
            GetSystemMetrics(SM_CXSMICON),
            GetSystemMetrics(SM_CYSMICON),
            LR_SHARED,
        );
        if !small_icon.is_null() {
            SendMessageW(hwnd, WM_SETICON, ICON_SMALL as usize, small_icon as isize);
        }
        let scale = GetDpiForWindow(hwnd).max(96) as f64 / 96.;
        let (font, title_font) = make_fonts(scale);
        ROOT.store(hwnd as usize, Ordering::Relaxed);
        let mut app = App {
            hwnd,
            settings,
            path,
            controls: BTreeMap::new(),
            labels: vec![],
            font,
            title_font,
            scale,
            scroll: 0,
            content_height: 800,
            tab: 0,
            recording: false,
            recorded: vec![],
            runner: None,
            last: Progress::default(),
            markers: vec![],
            message: String::new(),
            saved_error: false,
            keyboard_hook: null_mut(),
            mouse_hook: null_mut(),
            capturing: None,
            hotkeys_ready: false,
            wheel_remainder: 0,
            main_offset: 0,
            selecting_area: false,
        };
        if migrated {
            app.message=app.t("旧設定とクリック位置を移行しました。間隔は0.501秒から開始します。","Previous positions and settings were imported. The interval starts at 0.501 seconds.").into();
            app.save();
        }
        if repair_hotkeys(&mut app.settings) {
            app.message=if migrated {app.t("クリック位置と旧設定を移行し、未対応または重複したキーを修正しました（間隔は0.501秒から開始します）。","Imported positions and settings, repaired unsupported or duplicate hotkeys, and set the interval to 0.501 seconds.")}else{app.t("未対応または重複した旧キーを利用可能な初期キーに変更しました。設定で確認できます。","Unsupported or duplicate legacy hotkeys were reset to available defaults. Check Settings.")}.into();
            app.save();
        }
        if let Err(e) = app.register_hotkeys() {
            app.message = e;
        }
        APP.with(|cell| *cell.borrow_mut() = Some(app));
        SendMessageW(hwnd, APP_INIT, 0, 0);
        ShowWindow(hwnd, SW_SHOW);
        UpdateWindow(hwnd);
        SetTimer(hwnd, TICK, 50, None);
        let mut msg: MSG = zeroed();
        loop {
            let result = GetMessageW(&mut msg, null_mut(), 0, 0);
            if result <= 0 {
                break;
            }
            if IsDialogMessageW(hwnd, &msg) == 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        APP.with(|cell| {
            if let Some(mut app) = cell.borrow_mut().take() {
                if let Some(mut runner) = app.runner.take() {
                    runner.stop();
                }
                app.clear_markers();
                if !app.mouse_hook.is_null() {
                    UnhookWindowsHookEx(app.mouse_hook);
                }
                if !app.keyboard_hook.is_null() {
                    UnhookWindowsHookEx(app.keyboard_hook);
                }
                DeleteObject(app.font);
                DeleteObject(app.title_font);
            }
        });
        CloseHandle(mutex);
    }
}

thread_local! { static PROBE: RefCell<Vec<(Instant,i32,i32,u32)>> = const { RefCell::new(Vec::new()) }; }
unsafe extern "system" fn probe_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_LBUTTONDOWN || msg == WM_RBUTTONDOWN {
        PROBE.with(|events| {
            events.borrow_mut().push((
                Instant::now(),
                (lp & 0xffff) as u16 as i16 as i32,
                ((lp >> 16) & 0xffff) as u16 as i16 as i32,
                msg,
            ))
        });
        let count = PROBE.with(|events| events.borrow().len());
        let label = unsafe { GetDlgItem(hwnd, 1) };
        if !label.is_null() {
            set_text(label, &format!("Received clicks: {count}"));
        }
        return 0;
    }
    if msg == WM_PAINT {
        unsafe {
            let mut ps: PAINTSTRUCT = zeroed();
            let dc = BeginPaint(hwnd, &mut ps);
            let mut r: RECT = zeroed();
            GetClientRect(hwnd, &mut r);
            let brush = CreateSolidBrush(color(230, 240, 250));
            FillRect(dc, &r, brush);
            DeleteObject(brush);
            SetBkMode(dc, TRANSPARENT as i32);
            SetTextColor(dc, color(20, 40, 60));
            DrawTextW(
                dc,
                w!("Multi Point Clicker: isolated click test"),
                -1,
                &mut r,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
            EndPaint(hwnd, &ps);
        }
        return 0;
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

/// Real SendInput acceptance test. All events are directed only at this dedicated window.
pub fn test_target(path: &str) {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let instance = GetModuleHandleW(null());
        let class = WNDCLASSW {
            lpfnWndProc: Some(probe_proc),
            hInstance: instance,
            lpszClassName: w!("MultiPointClickerProbe"),
            ..zeroed()
        };
        RegisterClassW(&class);
        let hwnd = CreateWindowExW(
            0,
            w!("MultiPointClickerProbe"),
            w!("Multi Point Clicker — test target"),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU,
            100,
            100,
            620,
            340,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if hwnd.is_null() {
            let _ = std::fs::write(path, b"{\"error\":\"target unavailable\"}");
            return;
        }
        CreateWindowExW(
            0,
            w!("STATIC"),
            w!("Received clicks: 0"),
            WS_CHILD | WS_VISIBLE,
            15,
            15,
            400,
            30,
            hwnd,
            1usize as HMENU,
            instance,
            null(),
        );
        ShowWindow(hwnd, SW_SHOW);
        UpdateWindow(hwnd);
        let mut message: MSG = zeroed();
        while IsWindow(hwnd) != 0 {
            if GetMessageW(&mut message, null_mut(), 0, 0) <= 0 {
                break;
            }
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        let received = PROBE.with(|events| events.borrow().len());
        let _ = std::fs::write(path, format!("{{\"received_clicks\":{received}}}"));
    }
}

pub fn input_probe(path: &str) {
    let result = (|| -> Result<serde_json::Value, String> {
        unsafe {
            SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            let instance = GetModuleHandleW(null());
            let class = WNDCLASSW {
                lpfnWndProc: Some(probe_proc),
                hInstance: instance,
                lpszClassName: w!("MultiPointClickerProbe"),
                ..zeroed()
            };
            RegisterClassW(&class);
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST,
                w!("MultiPointClickerProbe"),
                w!("Multi Point Clicker — input test"),
                WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU,
                100,
                100,
                600,
                300,
                null_mut(),
                null_mut(),
                instance,
                null(),
            );
            if hwnd.is_null() {
                return Err("Cannot create isolated click target".into());
            }
            ShowWindow(hwnd, SW_SHOW);
            SetForegroundWindow(hwnd);
            UpdateWindow(hwnd);
            let mut origin = POINT { x: 0, y: 0 };
            ClientToScreen(hwnd, &mut origin);
            let mut original_cursor: POINT = zeroed();
            GetCursorPos(&mut original_cursor);
            let pump = || {
                let mut message: MSG = zeroed();
                while PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) != 0 {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            };
            pump();
            let mut scenarios = vec![];
            for (name, intervals, repeat, right) in [
                ("1ms", vec![1], 400, false),
                ("per-point", vec![7, 13, 19], 10, false),
                ("default", vec![501], 4, false),
                ("right", vec![5], 10, true),
            ] {
                PROBE.with(|e| e.borrow_mut().clear());
                let points = intervals
                    .iter()
                    .enumerate()
                    .map(|(i, ms)| Point {
                        x: origin.x + 100 + i as i32 * 100,
                        y: origin.y + 160,
                        interval_ms: Some(*ms),
                    })
                    .collect();
                let settings = Settings {
                    points,
                    per_point: true,
                    repeat,
                    button: if right { "right" } else { "left" }.into(),
                    ..Settings::default()
                };
                let mut runner = Runner::start(settings).map_err(|e| e.to_string())?;
                let deadline = Instant::now() + Duration::from_secs(10);
                while !runner.finished() && Instant::now() < deadline {
                    pump();
                    std::thread::sleep(Duration::from_millis(1));
                }
                runner.stop();
                pump();
                let progress = runner.snapshot();
                let events = PROBE.with(|e| e.borrow().clone());
                let deltas: Vec<f64> = events
                    .windows(2)
                    .map(|p| p[1].0.duration_since(p[0].0).as_secs_f64() * 1000.)
                    .collect();
                let expected = repeat as usize * intervals.len();
                let positions_ok = events.iter().enumerate().all(|(i, e)| {
                    (e.1 - (100 + (i % intervals.len()) as i32 * 100)).abs() <= 1
                        && (e.2 - 160).abs() <= 1
                });
                let buttons_ok = events.iter().all(|e| {
                    e.3 == if right {
                        WM_RBUTTONDOWN
                    } else {
                        WM_LBUTTONDOWN
                    }
                });
                let mut sorted = deltas.clone();
                sorted.sort_by(|a, b| a.total_cmp(b));
                let average = if deltas.is_empty() {
                    0.
                } else {
                    deltas.iter().sum::<f64>() / deltas.len() as f64
                };
                scenarios.push(serde_json::json!({"scenario":name,"expected_clicks":expected,"received_clicks":events.len(),"count_ok":events.len()==expected,"positions_ok":positions_ok,"buttons_ok":buttons_ok,"reported_clicks":progress.clicks,"engine_elapsed_ms":progress.elapsed_ms,"mean_input_ms":progress.mean_input_ms,"error":progress.error,"mean_interval_ms":average,"p95_interval_ms":sorted.get(sorted.len()*95/100),"interval_sample_ms":deltas.iter().take(12).collect::<Vec<_>>()}));
            }
            PROBE.with(|e| e.borrow_mut().clear());
            let settings = Settings {
                points: vec![Point {
                    x: origin.x + 100,
                    y: origin.y + 160,
                    interval_ms: None,
                }],
                interval_ms: 30_000,
                ..Settings::default()
            };
            let mut runner = Runner::start(settings).map_err(|e| e.to_string())?;
            let deadline = Instant::now() + Duration::from_secs(1);
            while runner.snapshot().clicks == 0 && Instant::now() < deadline {
                pump();
                std::thread::sleep(Duration::from_millis(1));
            }
            let stop_start = Instant::now();
            runner.stop();
            let stop_ms = stop_start.elapsed().as_secs_f64() * 1000.;
            pump();
            let count_before = PROBE.with(|e| e.borrow().len());
            for _ in 0..50 {
                std::thread::sleep(Duration::from_millis(1));
                pump();
            }
            let count_after = PROBE.with(|e| e.borrow().len());
            DestroyWindow(hwnd);
            SetCursorPos(original_cursor.x, original_cursor.y);
            Ok(
                serde_json::json!({"scenarios":scenarios,"stop_during_30sec_gap_ms":stop_ms,"stop_count_before":count_before,"stop_count_after":count_after,"no_clicks_after_stop":count_before==count_after,"stop_ok":stop_ms<100.&&count_before==1&&count_after==1}),
            )
        }
    })();
    let output = match result {
        Ok(v) => v,
        Err(e) => serde_json::json!({"error":e}),
    };
    if let Ok(json) = serde_json::to_vec_pretty(&output) {
        let _ = std::fs::write(path, json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct TestApp {
        app: App,
        dir: PathBuf,
    }
    impl TestApp {
        fn new() -> Self {
            static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "clicker-ui-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let hwnd = unsafe {
                let instance = GetModuleHandleW(null());
                let class = WNDCLASSW {
                    lpfnWndProc: Some(DefWindowProcW),
                    hInstance: instance,
                    lpszClassName: w!("ClickerControlTest"),
                    ..zeroed()
                };
                RegisterClassW(&class);
                CreateWindowExW(
                    0,
                    w!("ClickerControlTest"),
                    w!("test"),
                    WS_OVERLAPPEDWINDOW,
                    0,
                    0,
                    800,
                    1000,
                    null_mut(),
                    null_mut(),
                    instance,
                    null(),
                )
            };
            assert!(!hwnd.is_null());
            let (font, title_font) = make_fonts(1.);
            let app = App {
                hwnd,
                settings: Settings::default(),
                path: dir.join("settings.json"),
                controls: BTreeMap::new(),
                labels: vec![],
                font,
                title_font,
                scale: 1.,
                scroll: 0,
                content_height: 1000,
                tab: 0,
                recording: false,
                recorded: vec![],
                runner: None,
                last: Progress::default(),
                markers: vec![],
                message: String::new(),
                saved_error: false,
                keyboard_hook: null_mut(),
                mouse_hook: null_mut(),
                capturing: None,
                hotkeys_ready: true,
                wheel_remainder: 0,
                main_offset: 0,
                selecting_area: false,
            };
            let mut fixture = Self { app, dir };
            fixture.app.rebuild();
            fixture
        }
    }
    impl Drop for TestApp {
        fn drop(&mut self) {
            unsafe {
                DestroyWindow(self.app.hwnd);
                DeleteObject(self.app.font);
                DeleteObject(self.app.title_font);
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
    #[test]
    fn native_mode_controls_parse_and_persist_without_desktop_input() {
        let mut f = TestApp::new();
        let a = &mut f.app;
        unsafe {
            SendMessageW(a.control(MODE), CB_SETCURSEL, 1, 0);
        }
        a.action(MODE);
        assert_eq!(a.settings.mode, ClickMode::Area);
        for (id, value) in [
            (AREA_LEFT, "90"),
            (AREA_TOP, "80"),
            (AREA_RIGHT, "10"),
            (AREA_BOTTOM, "20"),
            (AREA_STEP, "17"),
            (FAST_INTERVAL, "0.003"),
        ] {
            set_text(a.control(id), value);
        }
        assert!(a.commit());
        assert_eq!(
            a.settings.area,
            Some(ClickArea {
                left: 10,
                top: 20,
                right: 90,
                bottom: 80
            })
        );
        assert_eq!(a.settings.fast_interval_ms, 3);
        let (loaded, _) = Settings::load(&a.path).unwrap();
        assert_eq!(loaded.mode, ClickMode::Area);
        assert_eq!(loaded.area_step_px, 17);
        unsafe {
            SendMessageW(a.control(MODE), CB_SETCURSEL, 2, 0);
        }
        a.action(MODE);
        assert_eq!(a.settings.mode, ClickMode::Pointer);
        assert_eq!(unsafe { IsWindowEnabled(a.control(REPEAT)) }, 0);
        assert_eq!(text(a.control(FAST_INTERVAL)), "0.003");
        let current = a.settings.area;
        set_text(a.control(AREA_TOP), "bad");
        assert!(!a.commit());
        assert_eq!(a.settings.area, current);
    }
    #[test]
    fn pending_point_intervals_survive_new_mode_switches() {
        let mut f = TestApp::new();
        let a = &mut f.app;
        a.settings.points = vec![Point {
            x: 1,
            y: 2,
            interval_ms: None,
        }];
        a.settings.details_open = true;
        a.settings.per_point = true;
        a.rebuild();
        set_text(a.control(INTERVAL_POINT), "0.011");
        unsafe {
            SendMessageW(a.control(MODE), CB_SETCURSEL, 3, 0);
        }
        a.action(MODE);
        assert_eq!(a.settings.mode, ClickMode::Drag);
        assert_eq!(a.settings.points[0].interval_ms, Some(11));
        assert_eq!(a.settings.interval_ms, 501);
        assert_eq!(a.settings.fast_interval_ms, 1);
    }
    #[test]
    fn lock_key_is_reserved_when_migrating_previous_bindings() {
        let mut s = Settings::default();
        s.hotkeys.insert("click_start".into(), "ctrl+alt+l".into());
        assert!(repair_hotkeys(&mut s));
        assert_ne!(
            parse_hotkey(&s.hotkeys["click_start"]),
            Some((MOD_CONTROL | MOD_ALT, b'L' as u32))
        );
    }
    #[test]
    fn resetting_common_interval_preserves_pending_point_edits() {
        assert!(follows_common_interval(None, "0.001", 1));
        assert!(!follows_common_interval(None, "0.011", 1));
        assert!(!follows_common_interval(Some(1), "0.001", 1));
        assert!(!follows_common_interval(None, "invalid", 1));
    }
    #[test]
    fn high_resolution_wheel_accumulates_small_deltas() {
        let mut rest = 0;
        for _ in 0..3 {
            assert_eq!(wheel_steps(&mut rest, 30), 0);
        }
        assert_eq!(wheel_steps(&mut rest, 30), 1);
        assert_eq!(rest, 0);
        assert_eq!(wheel_steps(&mut rest, -60), 0);
        assert_eq!(wheel_steps(&mut rest, -60), -1);
    }
    #[test]
    fn english_errors_are_selected_by_category() {
        let error = model::parse_interval("0.000").unwrap_err();
        assert_eq!(error.code, ErrorCode::IntervalRange);
        let english = error_text(&error, false);
        assert!(english.contains("0.001"));
        assert!(!english.contains("指定"));
        assert!(error_text(&error, true).contains("指定"));
    }
    #[test]
    fn legacy_keys_and_invalid_bindings_are_migrated() {
        for value in ["ctrl+alt+page_up", "alt+pause", "ctrl+;", "shift+."] {
            assert!(parse_hotkey(value).is_some(), "{value}");
        }
        let mut s = Settings::default();
        s.hotkeys
            .insert("record_start".into(), "unsupported_key".into());
        let valid = s.hotkeys["click_stop"].clone();
        assert!(repair_hotkeys(&mut s));
        assert_eq!(s.hotkeys["click_stop"], valid);
        assert!(
            ACTIONS
                .iter()
                .all(|a| parse_hotkey(&s.hotkeys[*a]).is_some())
        );
    }
    #[test]
    fn fonts_follow_the_window_dpi() {
        unsafe {
            let (a, b) = make_fonts(1.);
            let (c, d) = make_fonts(1.5);
            let mut normal: LOGFONTW = zeroed();
            let mut scaled: LOGFONTW = zeroed();
            assert!(
                GetObjectW(
                    a,
                    size_of::<LOGFONTW>() as i32,
                    (&mut normal as *mut LOGFONTW).cast()
                ) > 0
            );
            assert!(
                GetObjectW(
                    c,
                    size_of::<LOGFONTW>() as i32,
                    (&mut scaled as *mut LOGFONTW).cast()
                ) > 0
            );
            assert!(scaled.lfHeight.abs() > normal.lfHeight.abs());
            for h in [a, b, c, d] {
                DeleteObject(h);
            }
        }
    }
    #[test]
    fn keyboard_bindings_round_trip_and_cover_no_numpad() {
        for combo in [
            "ctrl+alt+r",
            "ctrl+shift+f12",
            "alt+num0",
            "shift+vk186",
            "win+f24",
        ] {
            let (mods, key) = parse_hotkey(combo).unwrap();
            let encoded = combo_from_key(mods, key).unwrap();
            assert_eq!(parse_hotkey(&encoded), Some((mods, key)));
        }
        for bad in ["", "ctrl+", "cmd+r", "f25", "num10"] {
            assert!(parse_hotkey(bad).is_none());
        }
    }
}
