use multi_point_clicker::{
    engine::{Progress, Runner},
    error::{ClickerError, ErrorCode},
    input_language::InputLanguageGuard,
    key_click::{
        self, InputMethod, KeyBinding, MovementMode, PressBehavior,
        native::{Controller as KeyController, Target as KeyTarget},
    },
    model::{self, ACTIONS, ClickArea, ClickMode, MAX_POINTS, Point, Settings},
};
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashSet},
    mem::{size_of, zeroed},
    path::PathBuf,
    ptr::{null, null_mut},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use windows_sys::{
    Win32::{
        Foundation::*,
        Graphics::{Dwm::*, Gdi::*},
        System::{
            LibraryLoader::GetModuleHandleW,
            Registry::*,
            SystemServices::{SS_ETCHEDFRAME, SS_OWNERDRAW},
            Threading::*,
        },
        UI::{Controls::*, HiDpi::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
    },
    core::w,
};

const APP_RECORD: u32 = WM_APP + 1;
const APP_CAPTURE: u32 = WM_APP + 2;
const APP_INIT: u32 = WM_APP + 3;
const APP_READY: u32 = WM_APP + 4;
const APP_KEY_PICK: u32 = WM_APP + 5;
const APP_KEY_CANCEL: u32 = WM_APP + 6;
const STATIC_LEFT: u32 = 0; // SS_LEFT (the default STATIC control alignment).
const TICK: usize = 1;
const PREVIEW_TIMER: usize = 2;
const NAV_MAIN: u16 = 10;
const NAV_SETTINGS: u16 = 11;
const LANG: u16 = 12;
const NAV_KEYS: u16 = 13;
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
const KEY_ENABLE: u16 = 520;
const KEY_TARGET: u16 = 521;
const KEY_ADD: u16 = 522;
const KEY_WASD: u16 = 523;
const KEY_STATUS: u16 = 524;
const KEY_PROFILE: u16 = 525;
const KEY_PROFILE_SAVE: u16 = 526;
const KEY_PROFILE_NEW: u16 = 527;
const KEY_PROFILE_DELETE: u16 = 528;
const KEY_MOVE_BUTTONS: u16 = 529;
const KEY_MOVE_STICK: u16 = 530;
const KEY_INPUT: u16 = 531;
const KEY_STICK_CENTER: u16 = 532;
const KEY_STICK_RADIUS: u16 = 533;
const KEY_STICK_PREVIEW: u16 = 534;
const KEY_WARNING: u16 = 535;
const KEY_BIND: u16 = 600;
const KEY_POSITION: u16 = 650;
const KEY_BUTTON: u16 = 700;
const KEY_DELETE: u16 = 750;
const KEY_BEHAVIOR: u16 = 800;
const KEY_LABEL: u16 = 850;
const WASD_POSITION: u16 = 900;
const WASD_BEHAVIOR: u16 = 910;
const WASD_BUTTON: u16 = 920;
const WASD_KEYS: [u32; 4] = [b'W' as u32, b'A' as u32, b'S' as u32, b'D' as u32];
static KEY_PICKING: AtomicBool = AtomicBool::new(false);
thread_local! {static KEY_PICK_ORIGIN:RefCell<Option<POINT>>=const{RefCell::new(None)};}
thread_local! {static KEY_CAPTURE_STATE:RefCell<key_click::KeyCapture>=RefCell::new(key_click::KeyCapture::default());}
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
        KeyMapping => (
            "キーや登録位置が不正です。重複も確認してください。",
            "Invalid or duplicate key mapping. Check the keys and positions.",
        ),
        KeyTarget => (
            "対象のゲーム画面を選択し、登録位置を確認してください。",
            "Select the target game window and check the recorded position.",
        ),
        StickRange => (
            "スティックの移動幅は4〜2048 pxです。中心と移動範囲がゲーム画面内に収まるようにしてください。",
            "Use a stick radius of 4–2048 px. The center and movement range must fit inside the game window.",
        ),
        ProfileName => (
            "設定名を入力してください。制御文字や、ほかの設定と同じ名前は使えません。",
            "Enter a profile name without control characters or a duplicate name.",
        ),
        ProfileLimit => (
            "ゲームごとの設定は64件までです。不要な設定を削除してください。",
            "You can save up to 64 game profiles. Delete an unused profile first.",
        ),
        InputCombination => (
            "マウスでは別々の位置を同時に保持できません。ゲームが対応していればタッチ入力を選んでください。",
            "A mouse cannot hold separate positions at once. Choose Touch if the game supports touch input.",
        ),
        InputRightButton => (
            "タッチ入力では右クリックできません。入力方式にマウスを選んでください。",
            "Touch input cannot right-click. Choose Mouse as the input method.",
        ),
        InputMouseMoved => (
            "手動のマウス操作を優先しました。割り当てたキーを離して、押し直してください。",
            "Manual mouse movement took priority. Release the assigned key and press it again.",
        ),
        InputMouseBusy => (
            "物理マウスのボタンが押されています。マウスのボタンと割り当てたキーを離してから、キーを押し直してください。",
            "A physical mouse button is held. Release the mouse button and the assigned key, then press the key again.",
        ),
        InputInitialize => (
            "選択した入力方式を準備できません。ゲームの対応や実行権限を確認してください。",
            "Cannot initialize the selected input method. Check game support and application permissions.",
        ),
        AreaMissing => (
            "先に範囲の2隅を指定してください。",
            "Select the two corners of a region first.",
        ),
        AreaInvalid => (
            "範囲の座標または配置間隔が不正です（1〜4096 px）。",
            "Invalid region coordinates or grid spacing (1–4096 px).",
        ),
        InputMonitor => (
            "入力の監視を開始できません。",
            "Cannot start monitoring input.",
        ),
        IntervalRange => (
            "間隔は 0.0000001〜3600.0000000 秒で指定してください。",
            "Use an interval from 0.0000001 to 3600.0000000 seconds.",
        ),
        IntervalFormat => (
            "秒数は小数点以下7桁までで指定してください（例: 0.0000001）。",
            "Enter seconds with up to seven decimal places (for example, 0.0000001).",
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
            "入力を送信できません。対象アプリの権限を確認してください。",
            "Cannot send input. Check the target application's permissions.",
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
    if matches!(
        error.code,
        ErrorCode::InputCombination
            | ErrorCode::InputRightButton
            | ErrorCode::InputMouseMoved
            | ErrorCode::InputMouseBusy
    ) {
        return base.into();
    }
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
    lock_hotkey_ready: bool,
    wheel_remainder: i32,
    main_offset: i32,
    selecting_area: bool,
    key_controller: Option<KeyController>,
    key_target: Option<Arc<KeyTarget>>,
    key_pick: Option<KeyPick>,
    profile_draft_name: Option<String>,
}
#[derive(Clone, Copy)]
enum KeyPick {
    Game,
    Position(usize),
    StickCenter,
}
#[derive(Clone, Copy)]
enum ProfileChange {
    Select(u64),
    New,
    Delete(u64),
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
        if id == INDIVIDUAL || id == TOPMOST || id == KEY_ENABLE {
            let child = self.create(
                id,
                w!("BUTTON"),
                caption,
                WS_TABSTOP | BS_AUTOCHECKBOX as u32,
                [x, y, w, h],
            );
            let checked = if id == INDIVIDUAL {
                self.settings.per_point
            } else if id == TOPMOST {
                self.settings.topmost
            } else {
                self.settings.key_clicks.enabled
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
        let result = self
            .settings
            .key_clicks
            .sync_selected_profile()
            .and_then(|()| self.settings.save(&self.path));
        if let Err(e) = result {
            self.message = error_text(&e, self.settings.lang == "ja");
            self.saved_error = true;
        } else {
            self.saved_error = false;
        }
        self.sync_key_runtime(!self.saved_error);
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
            self.settings.interval_ticks = interval;
            self.settings.repeat = repeat;
            self.settings.fast_interval_ticks = fast;
            self.settings.area_step_px = step;
            self.settings.area = region;
            for (p, ms) in self.settings.points.iter_mut().zip(overrides) {
                p.interval_ticks = Some(ms);
            }
            set_text(self.control(INTERVAL), &model::format_interval(interval));
        } else if self.tab == 1 {
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
        } else if self.tab == 2 {
            let profile_name = text(self.control(KEY_PROFILE));
            let selected_name = usize::try_from(combo_selection(self.control(KEY_PROFILE)))
                .ok()
                .and_then(|i| self.settings.key_clicks.profiles.get(i))
                .map(|p| p.name.as_str());
            if selected_name != Some(profile_name.as_str()) {
                self.profile_draft_name = Some(profile_name);
            }
            let mut candidate = self.settings.key_clicks.clone();
            if !self.control(KEY_STICK_RADIUS).is_null() {
                let radius = text(self.control(KEY_STICK_RADIUS))
                    .trim()
                    .parse::<u32>()
                    .ok()
                    .filter(|r| (4..=2048).contains(r))
                    .ok_or_else(|| {
                        error_text(&ErrorCode::StickRange.into(), self.settings.lang == "ja")
                    })?;
                candidate.stick.radius_px = radius;
            }
            for (index, binding) in candidate.bindings.iter_mut().enumerate() {
                let index = index as u16;
                let label = self.control(KEY_LABEL + index);
                if !label.is_null() {
                    binding.label = text(label);
                }
                let behavior = self.control(KEY_BEHAVIOR + index);
                if !behavior.is_null() {
                    binding.behavior = behavior_selection(behavior);
                }
                let button = self.control(KEY_BUTTON + index);
                if !button.is_null() {
                    binding.right = combo_selection(button) == 1;
                }
                if let Some(row) = WASD_KEYS.iter().position(|key| binding.key == Some(*key)) {
                    let behavior = self.control(WASD_BEHAVIOR + row as u16);
                    if !behavior.is_null() {
                        binding.behavior = behavior_selection(behavior);
                        binding.right =
                            combo_selection(self.control(WASD_BUTTON + row as u16)) == 1;
                    }
                }
            }
            candidate
                .validate()
                .map_err(|e| error_text(&e, self.settings.lang == "ja"))?;
            self.settings.key_clicks = candidate;
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
        let keys = self.t("キーでクリック", "Key clicks").to_string();
        self.button(NAV_MAIN, &main, 22, 63, 120, 34);
        self.button(NAV_KEYS, &keys, 148, 63, 190, 34);
        self.button(NAV_SETTINGS, &settings, 344, 63, 120, 34);
        if self.tab == 0 {
            self.build_main();
        } else if self.tab == 1 {
            self.build_settings();
        } else {
            self.build_key_clicks();
        }
        self.position_controls();
        self.update_stats();
    }
    fn key_combo(&mut self, id: u16, items: &[&str], selected: usize, bounds: [i32; 4]) {
        let combo = self.create(
            id,
            w!("COMBOBOX"),
            "",
            WS_TABSTOP | CBS_DROPDOWNLIST as u32,
            bounds,
        );
        unsafe {
            for item in items {
                SendMessageW(combo, CB_ADDSTRING, 0, wide(item).as_ptr() as isize);
            }
            SendMessageW(combo, CB_SETCURSEL, selected, 0);
        }
        self.enable(id, !self.busy());
    }
    fn behavior_combo(&mut self, id: u16, behavior: PressBehavior, bounds: [i32; 4]) {
        let items = [
            self.t("1回クリック", "Click once"),
            self.t("押している間", "Hold"),
        ];
        self.key_combo(
            id,
            &items,
            usize::from(behavior == PressBehavior::Hold),
            bounds,
        );
    }
    fn position_caption(&self, point: Option<&key_click::ClientPoint>) -> String {
        point
            .map(|p| {
                if self
                    .key_target
                    .as_ref()
                    .and_then(|target| target.client_size().ok().map(|size| (target, size)))
                    .is_some_and(|(target, (w, h))| !p.fits(&target.identity, w, h))
                {
                    self.t("位置を取り直す", "Record again").into()
                } else {
                    format!("X {}   Y {}", p.x, p.y)
                }
            })
            .unwrap_or_else(|| self.t("位置を記憶", "Record position").into())
    }
    fn wasd_index(&self, row: usize) -> Option<usize> {
        let key = *WASD_KEYS.get(row)?;
        self.settings
            .key_clicks
            .bindings
            .iter()
            .position(|b| b.key == Some(key))
    }
    fn add_wasd(&mut self) -> bool {
        let mut candidate = self.settings.key_clicks.clone();
        if !candidate.add_wasd() {
            self.message = self
                .t(
                    "キーの登録は16個までです。",
                    "You can assign up to 16 keys.",
                )
                .into();
            false
        } else if candidate
            .bindings
            .iter()
            .filter_map(|b| b.key)
            .any(|key| self.key_conflict(key))
        {
            self.message = self
                .t(
                    "WASDのキーが操作用ショートカットで使用中です。",
                    "A WASD key is already used by an operation hotkey.",
                )
                .into();
            false
        } else {
            self.settings.key_clicks = candidate;
            true
        }
    }
    fn build_key_clicks(&mut self) {
        self.main_offset = 0;
        let heading = self.t("ゲームごとの設定", "Game profiles").to_string();
        self.label(&heading, 30, 124, 130, 26);
        let profile = self.create(
            KEY_PROFILE,
            w!("COMBOBOX"),
            "",
            WS_TABSTOP | CBS_DROPDOWN as u32 | CBS_AUTOHSCROLL as u32,
            [165, 118, 215, 240],
        );
        for saved in &self.settings.key_clicks.profiles {
            unsafe {
                SendMessageW(
                    profile,
                    CB_ADDSTRING,
                    0,
                    wide(&saved.name).as_ptr() as isize,
                );
            }
        }
        if let Some(index) = self
            .settings
            .key_clicks
            .profiles
            .iter()
            .position(|p| Some(p.id) == self.settings.key_clicks.selected_profile)
        {
            unsafe {
                SendMessageW(profile, CB_SETCURSEL, index, 0);
            }
        }
        let name = self.profile_draft_name.clone().unwrap_or_else(|| {
            self.settings
                .key_clicks
                .selected_profile_name()
                .unwrap_or("")
                .into()
        });
        set_text(profile, &name);
        unsafe {
            SendMessageW(profile, CB_LIMITTEXT, 128, 0);
            SendMessageW(
                profile,
                CB_SETCUEBANNER,
                0,
                wide(self.t("設定名を入力 / 選択", "Type or select a name")).as_ptr() as isize,
            );
        }
        let captions = [
            self.t("保存", "Save"),
            self.t("新規", "New"),
            self.t("削除", "Delete"),
        ]
        .map(str::to_string);
        for (id, caption, x, width) in [
            (KEY_PROFILE_SAVE, &captions[0], 388, 69),
            (KEY_PROFILE_NEW, &captions[1], 465, 68),
            (KEY_PROFILE_DELETE, &captions[2], 541, 74),
        ] {
            self.button(id, caption, x, 118, width, 32);
            self.enable(id, !self.busy());
        }
        self.enable(KEY_PROFILE, !self.busy());
        self.enable(
            KEY_PROFILE_NEW,
            !self.busy() && self.settings.key_clicks.profiles.len() < key_click::MAX_PROFILES,
        );
        self.enable(
            KEY_PROFILE_DELETE,
            !self.busy() && !self.settings.key_clicks.profiles.is_empty(),
        );
        let name = self
            .key_target
            .as_ref()
            .map(|t| t.identity.title.as_str())
            .or_else(|| {
                self.settings
                    .key_clicks
                    .last_target
                    .as_ref()
                    .map(|t| t.title.as_str())
            })
            .unwrap_or(self.t("未選択", "Not selected"));
        let short: String = name.chars().take(25).collect();
        self.label(
            &format!("{}: {}", self.t("対象ゲーム", "Target game"), short),
            30,
            170,
            345,
            34,
        );
        let choose = self.t("ゲーム画面を選ぶ", "Choose game window").to_string();
        self.button(KEY_TARGET, &choose, 385, 164, 230, 36);
        let enable = self
            .t("キー操作を有効にする", "Enable key controls")
            .to_string();
        self.button(KEY_ENABLE, &enable, 30, 209, 585, 30);
        let hint = self
            .t(
                "選んだ画面が前面のときだけ有効です。",
                "Active only while the selected game window is in front.",
            )
            .to_string();
        self.label(&hint, 30, 246, 585, 28);
        self.create(0, w!("STATIC"), "", SS_ETCHEDFRAME, [22, 282, 602, 319]);
        let movement = self.t("移動方式", "Movement").to_string();
        self.label(&movement, 35, 297, 88, 27);
        for (id, caption, mode, x, width) in [
            (
                KEY_MOVE_BUTTONS,
                self.t("方向ボタン", "Buttons"),
                MovementMode::Buttons,
                130,
                132,
            ),
            (
                KEY_MOVE_STICK,
                self.t("スティック", "Stick"),
                MovementMode::Stick,
                270,
                116,
            ),
        ] {
            let radio = self.create(
                id,
                w!("BUTTON"),
                caption,
                WS_TABSTOP
                    | BS_AUTORADIOBUTTON as u32
                    | if id == KEY_MOVE_BUTTONS { WS_GROUP } else { 0 },
                [x, 292, width, 30],
            );
            unsafe {
                SendMessageW(
                    radio,
                    BM_SETCHECK,
                    usize::from(self.settings.key_clicks.movement_mode == mode),
                    0,
                );
            }
            self.enable(id, !self.busy());
        }
        let input = self.t("入力", "Input").to_string();
        self.label(&input, 405, 298, 48, 26);
        let methods = [self.t("マウス", "Mouse"), self.t("タッチ", "Touch")];
        self.key_combo(
            KEY_INPUT,
            &methods,
            usize::from(self.settings.key_clicks.input_method == InputMethod::Touch),
            [455, 292, 150, 120],
        );
        if self.settings.key_clicks.movement_mode == MovementMode::Buttons {
            let title = self.t("移動のキー設定", "Movement keys").to_string();
            self.label(&title, 35, 338, 570, 26);
            let columns = [
                (self.t("キー", "Key"), 35, 43),
                (self.t("方向", "Direction"), 85, 68),
                (self.t("画面の位置", "Position"), 160, 160),
                (self.t("動作", "Behavior"), 330, 178),
                (self.t("左右", "Button"), 518, 87),
            ];
            for (caption, x, width) in columns {
                self.label(caption, x, 371, width, 25);
            }
            for (row, direction) in [
                self.t("上", "Up"),
                self.t("左", "Left"),
                self.t("下", "Down"),
                self.t("右", "Right"),
            ]
            .into_iter()
            .enumerate()
            {
                let y = 400 + row as i32 * 37;
                let binding = self
                    .wasd_index(row)
                    .map(|index| self.settings.key_clicks.bindings[index].clone());
                self.label(&key_name(WASD_KEYS[row]), 40, y + 5, 38, 27);
                self.label(direction, 85, y + 5, 68, 27);
                let caption =
                    self.position_caption(binding.as_ref().and_then(|b| b.position.as_ref()));
                self.button(WASD_POSITION + row as u16, &caption, 160, y, 160, 32);
                self.behavior_combo(
                    WASD_BEHAVIOR + row as u16,
                    binding.as_ref().map_or(PressBehavior::Hold, |b| b.behavior),
                    [330, y, 178, 110],
                );
                let buttons = [self.t("左", "Left"), self.t("右", "Right")];
                self.key_combo(
                    WASD_BUTTON + row as u16,
                    &buttons,
                    usize::from(binding.as_ref().is_some_and(|b| b.right)),
                    [518, y, 87, 110],
                );
                self.enable(WASD_POSITION + row as u16, !self.busy());
            }
            let hint = self
                .t(
                    "各方向の位置と動作を設定します。同時押しにはゲームのタッチ対応が必要です。",
                    "Set each direction's position and behavior. Simultaneous buttons require game touch support.",
                )
                .to_string();
            self.label(&hint, 35, 552, 570, 42);
        } else {
            let title = self.t("スティックの設定", "Stick settings").to_string();
            self.label(&title, 35, 341, 325, 28);
            let caption = self
                .settings
                .key_clicks
                .stick
                .center
                .as_ref()
                .map(|p| {
                    format!(
                        "{}  {}",
                        self.t("中心", "Center"),
                        self.position_caption(Some(p))
                    )
                })
                .unwrap_or_else(|| self.t("中心を記憶", "Record center").into());
            self.button(KEY_STICK_CENTER, &caption, 35, 391, 325, 39);
            let radius = self.t("移動幅", "Radius").to_string();
            self.label(&radius, 35, 454, 100, 27);
            self.edit(
                KEY_STICK_RADIUS,
                &self.settings.key_clicks.stick.radius_px.to_string(),
                140,
                448,
                130,
                true,
            );
            self.label("px", 280, 454, 55, 27);
            let hint = self
                .t(
                    "中心からの距離（4〜2048 px）\nWASDを離すと操作を解除します。",
                    "Distance from center (4–2048 px).\nRelease WASD to release the stick.",
                )
                .to_string();
            self.label(&hint, 35, 506, 330, 74);
            let preview = self
                .t("W+D：右上へ（操作イメージ）", "W+D: up-right (preview)")
                .to_string();
            self.create(
                KEY_STICK_PREVIEW,
                w!("STATIC"),
                &preview,
                SS_OWNERDRAW,
                [385, 333, 220, 253],
            );
            self.enable(KEY_STICK_CENTER, !self.busy());
            self.enable(KEY_STICK_RADIUS, !self.busy());
        }
        let mut y = 621;
        let extra = self.t("追加の操作", "Additional actions").to_string();
        self.label(&extra, 30, y, 585, 30);
        y += 37;
        for (caption, x, width) in [
            (self.t("キー", "Key"), 30, 78),
            (self.t("操作名", "Label"), 115, 110),
            (self.t("画面の位置", "Position"), 232, 142),
            (self.t("動作", "Behavior"), 381, 122),
            (self.t("左右", "Button"), 510, 60),
        ] {
            self.label(caption, x, y, width, 26);
        }
        y += 32;
        let mut extra_count = 0;
        for (i, binding) in self.settings.key_clicks.bindings.clone().iter().enumerate() {
            if binding.key.is_some_and(|key| WASD_KEYS.contains(&key)) {
                continue;
            }
            extra_count += 1;
            let index = i as u16;
            let key = if self.capturing == Some(ACTIONS.len() + i) {
                self.t("入力待ち", "Press key").into()
            } else {
                binding
                    .key
                    .map(key_name)
                    .unwrap_or_else(|| self.t("キー選択", "Set key").into())
            };
            self.button(KEY_BIND + index, &key, 30, y, 78, 32);
            self.edit(KEY_LABEL + index, &binding.label, 115, y, 110, false);
            unsafe {
                SendMessageW(self.control(KEY_LABEL + index), EM_SETLIMITTEXT, 128, 0);
            }
            self.button(
                KEY_POSITION + index,
                &self.position_caption(binding.position.as_ref()),
                232,
                y,
                142,
                32,
            );
            self.behavior_combo(KEY_BEHAVIOR + index, binding.behavior, [381, y, 122, 110]);
            let buttons = [self.t("左", "L"), self.t("右", "R")];
            self.key_combo(
                KEY_BUTTON + index,
                &buttons,
                usize::from(binding.right),
                [510, y, 48, 110],
            );
            let delete = self.t("削除", "Delete").to_string();
            self.button(KEY_DELETE + index, &delete, 565, y, 50, 32);
            for base in [KEY_BIND, KEY_LABEL, KEY_POSITION, KEY_DELETE] {
                self.enable(base + index, !self.busy());
            }
            y += 42;
        }
        if extra_count == 0 {
            let empty = self
                .t(
                    "追加キーは未登録です。操作名は任意で入力できます。",
                    "No additional keys. Action labels are optional.",
                )
                .to_string();
            self.label(&empty, 30, y, 585, 30);
            y += 38;
        }
        let add = self.t("＋ 追加", "+ Add").to_string();
        self.button(KEY_ADD, &add, 30, y, 125, 32);
        self.label(
            &format!(
                "{} / {}",
                self.settings.key_clicks.bindings.len(),
                key_click::MAX_BINDINGS
            ),
            475,
            y + 5,
            140,
            27,
        );
        self.enable(
            KEY_ADD,
            !self.busy() && self.settings.key_clicks.bindings.len() < key_click::MAX_BINDINGS,
        );
        self.enable(
            KEY_ENABLE,
            !self.busy() && (self.hotkeys_ready || self.settings.key_clicks.enabled),
        );
        self.enable(KEY_TARGET, !self.busy());
        y += 45;
        let compatibility = match self.settings.key_clicks.input_method {
            InputMethod::Touch => self.t(
                "タッチ入力を受け付けるゲームで使えます。",
                "Requires a game that accepts touch input.",
            ),
            InputMethod::Mouse => self.t(
                "マウスでは別々の位置を同時に保持できません。",
                "A mouse cannot hold separate positions at once.",
            ),
        }
        .to_string();
        self.label(&compatibility, 30, y, 585, 40);
        y += 45;
        self.label_id(KEY_STATUS, "", 30, y, 585, 60);
        y += 67;
        self.label_id(KEY_WARNING, "", 30, y, 585, 65);
        y += 70;
        self.label_id(MESSAGE, &self.message.clone(), 30, y, 585, 95);
        self.content_height = y + 115;
    }
    fn restore_profile_selection(&self) {
        let index = self
            .settings
            .key_clicks
            .profiles
            .iter()
            .position(|p| Some(p.id) == self.settings.key_clicks.selected_profile);
        unsafe {
            SendMessageW(
                self.control(KEY_PROFILE),
                CB_SETCURSEL,
                index.unwrap_or(usize::MAX),
                0,
            );
        }
        if let Some(name) = &self.profile_draft_name {
            set_text(self.control(KEY_PROFILE), name);
        }
    }
    fn change_key_profile(&mut self, change: ProfileChange) {
        self.change_key_profile_using(change, Self::stop_key_profile_runtime);
    }
    fn stop_key_profile_runtime(&mut self) {
        self.cancel_capture();
        if let Some(mut controller) = self.key_controller.take() {
            controller.stop();
        }
        // A saved identity never authorizes reuse of the previous live window.
        self.key_target = None;
        self.settings.key_clicks.enabled = false;
    }
    fn change_key_profile_using(
        &mut self,
        change: ProfileChange,
        stop_current: impl FnOnce(&mut Self),
    ) {
        let keeps_current = matches!(change, ProfileChange::Delete(id)
            if Some(id) != self.settings.key_clicks.selected_profile);
        if !self.commit() {
            self.restore_profile_selection();
            return;
        }
        stop_current(self);
        let result = match change {
            ProfileChange::Select(id) => self.settings.key_clicks.select_profile(id),
            ProfileChange::New => self.settings.key_clicks.new_profile(),
            ProfileChange::Delete(id) => self.settings.key_clicks.delete_profile(id),
        };
        match result {
            Ok(()) => {
                if keeps_current {
                    self.message = self
                        .t(
                            "保存済みの設定を削除しました。編集中の設定は保持しています。",
                            "Saved profile deleted. Your current edits have been kept.",
                        )
                        .into();
                } else {
                    self.profile_draft_name = None;
                    self.message = self
                        .t(
                            "設定を切り替えました。ゲーム画面を選び直してください。",
                            "Profile changed. Choose the game window again.",
                        )
                        .into();
                }
                self.save();
            }
            Err(error) => self.message = error_text(&error, self.settings.lang == "ja"),
        }
        self.rebuild();
    }
    fn choose_key_profile_to_delete(&self) -> Option<u64> {
        let candidates = profile_deletion_candidates(&self.settings.key_clicks);
        if let Some(id) = self.settings.key_clicks.selected_profile {
            return candidates
                .iter()
                .any(|(saved, _)| *saved == id)
                .then_some(id);
        }
        if candidates.is_empty() {
            return None;
        }
        unsafe {
            let menu = CreatePopupMenu();
            if menu.is_null() {
                return None;
            }
            for (index, (_, name)) in candidates.iter().enumerate() {
                if AppendMenuW(
                    menu,
                    MF_STRING,
                    index + 1,
                    wide(&name.replace('&', "&&")).as_ptr(),
                ) == 0
                {
                    DestroyMenu(menu);
                    return None;
                }
            }
            let mut anchor: RECT = zeroed();
            if GetWindowRect(self.control(KEY_PROFILE_DELETE), &mut anchor) == 0 {
                DestroyMenu(menu);
                return None;
            }
            let command = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
                anchor.left,
                anchor.bottom,
                0,
                self.hwnd,
                null(),
            );
            DestroyMenu(menu);
            usize::try_from(command)
                .ok()
                .and_then(|id| id.checked_sub(1))
                .and_then(|index| candidates.get(index))
                .map(|(id, _)| *id)
        }
    }
    fn delete_key_profile_with_confirmation(
        &mut self,
        id: u64,
        confirm: impl FnOnce(&str) -> bool,
    ) {
        let Some((_, name)) = profile_deletion_candidates(&self.settings.key_clicks)
            .into_iter()
            .find(|(saved, _)| *saved == id)
        else {
            return;
        };
        let prompt = format!(
            "{}\n{}",
            self.t(
                "このゲームの設定を削除しますか？",
                "Delete this game profile?"
            ),
            name
        );
        if confirm(&prompt) {
            self.change_key_profile(ProfileChange::Delete(id));
        }
    }
    fn sync_key_runtime(&mut self, allow_start: bool) {
        let active = self.settings.key_clicks.enabled
            && self.hotkeys_ready
            && !self.key_bindings_conflict()
            && !self.busy()
            && self.capturing.is_none()
            && self.key_pick.is_none()
            && !self.saved_error
            && self.key_target.is_some();
        if self.key_controller.is_none()
            && active
            && allow_start
            && self.settings.key_clicks.has_configured_action()
        {
            match KeyController::start() {
                Ok(controller) => self.key_controller = Some(controller),
                Err(error) => self.handle_key_error(error),
            }
        }
        let hotkeys =
            reserved_key_hotkeys(&self.settings, self.hotkeys_ready, self.lock_hotkey_ready);
        if let Some(controller) = self.key_controller.as_mut() {
            controller.configure_with_hotkeys(
                self.key_target.clone(),
                &self.settings.key_clicks,
                &hotkeys,
            );
            controller.set_active(active);
        }
    }
    fn handle_key_error(&mut self, error: ClickerError) {
        let detail = error_text(&error, self.settings.lang == "ja");
        if fatal_key_error(&error.code) {
            self.settings.key_clicks.enabled = false;
            if let Some(mut controller) = self.key_controller.take() {
                controller.set_active(false);
                controller.stop();
            }
            unsafe {
                SendMessageW(self.control(KEY_ENABLE), BM_SETCHECK, 0, 0);
            }
            let message = format!(
                "{}\n{}",
                self.t(
                    "エラーのためキー操作を停止しました。",
                    "Key controls stopped because of an error."
                ),
                detail
            );
            self.message = message.clone();
            self.save();
            if self.saved_error {
                self.message = format!("{message}\n{}", self.message);
            }
        } else {
            self.message = detail;
        }
        set_text(self.control(MESSAGE), &self.message);
    }
    fn key_conflict(&self, key: u32) -> bool {
        self.settings
            .hotkeys
            .values()
            .filter_map(|value| parse_hotkey(value))
            .any(|(mods, registered)| mods == 0 && registered == key)
    }
    fn key_bindings_conflict(&self) -> bool {
        (self.settings.key_clicks.movement_mode == MovementMode::Stick
            && WASD_KEYS.iter().any(|key| self.key_conflict(*key)))
            || self
                .settings
                .key_clicks
                .bindings
                .iter()
                .filter_map(|b| b.key)
                .any(|key| self.key_conflict(key))
    }
    fn key_positions_match_target(&self) -> bool {
        let Some(target) = &self.key_target else {
            return false;
        };
        let Ok((width, height)) = target.client_size() else {
            return false;
        };
        let keys = &self.settings.key_clicks;
        let stick_fits = keys.movement_mode == MovementMode::Stick
            && keys.stick.center.as_ref().is_some_and(|p| {
                let radius = i64::from(keys.stick.radius_px);
                p.fits(&target.identity, width, height)
                    && i64::from(p.x) >= radius
                    && i64::from(p.y) >= radius
                    && i64::from(p.x) + radius < i64::from(width)
                    && i64::from(p.y) + radius < i64::from(height)
            });
        stick_fits
            || keys.bindings.iter().any(|binding| {
                binding.key.is_some_and(|key| {
                    keys.movement_mode == MovementMode::Buttons || !WASD_KEYS.contains(&key)
                }) && binding
                    .position
                    .as_ref()
                    .is_some_and(|p| p.fits(&target.identity, width, height))
            })
    }
    fn begin_key_pick(&mut self, mode: KeyPick) {
        if matches!(mode, KeyPick::Position(_) | KeyPick::StickCenter) && self.key_target.is_none()
        {
            self.message = self
                .t(
                    "先にゲーム画面を選んでください。",
                    "Choose the game window first.",
                )
                .into();
            self.rebuild();
            return;
        }
        self.cancel_capture();
        if let Some(controller) = &self.key_controller {
            controller.set_active(false);
        }
        self.key_pick = Some(mode);
        KEY_PICKING.store(true, Ordering::Release);
        KEY_PICK_ORIGIN.with(|point| *point.borrow_mut() = None);
        self.mouse_hook = unsafe {
            SetWindowsHookExW(
                WH_MOUSE_LL,
                Some(key_pick_mouse_hook),
                GetModuleHandleW(null()),
                0,
            )
        };
        self.keyboard_hook = unsafe {
            SetWindowsHookExW(
                WH_KEYBOARD_LL,
                Some(keyboard_hook),
                GetModuleHandleW(null()),
                0,
            )
        };
        if self.mouse_hook.is_null() || self.keyboard_hook.is_null() {
            self.cancel_key_pick();
            self.message = self
                .t(
                    "位置を記憶するための入力監視を開始できません。",
                    "Cannot start input monitoring for position capture.",
                )
                .into();
            self.rebuild();
            return;
        }
        self.message = self
            .t(
                "ゲーム画面の位置をクリックしてください。Escで取消。",
                "Click the position in the game. Esc cancels.",
            )
            .into();
        unsafe {
            ShowWindow(self.hwnd, SW_HIDE);
        }
    }
    fn cancel_key_pick(&mut self) {
        if self.key_pick.take().is_none() {
            return;
        }
        KEY_PICKING.store(false, Ordering::Release);
        KEY_PICK_ORIGIN.with(|point| *point.borrow_mut() = None);
        unsafe {
            if !self.mouse_hook.is_null() {
                UnhookWindowsHookEx(self.mouse_hook);
                self.mouse_hook = null_mut();
            }
            if !self.keyboard_hook.is_null() {
                UnhookWindowsHookEx(self.keyboard_hook);
                self.keyboard_hook = null_mut();
            }
            ShowWindow(self.hwnd, SW_SHOW);
            SetForegroundWindow(self.hwnd);
        }
    }
    fn key_pick_finished(&mut self, x: i32, y: i32) {
        let Some(mode) = self.key_pick else {
            return;
        };
        let screen = POINT { x, y };
        let result = match mode {
            KeyPick::Game => KeyTarget::capture_at(screen).map(|target| {
                self.settings.key_clicks.last_target = Some(target.identity.clone());
                self.key_target = Some(target);
            }),
            KeyPick::Position(index) => self
                .key_target
                .as_ref()
                .ok_or_else(|| ErrorCode::KeyTarget.into())
                .and_then(|target| target.record_point(screen))
                .and_then(|point| {
                    let row = self
                        .settings
                        .key_clicks
                        .bindings
                        .get_mut(index)
                        .ok_or(ErrorCode::KeyMapping)?;
                    row.position = Some(point);
                    Ok(())
                }),
            KeyPick::StickCenter => self
                .key_target
                .as_ref()
                .ok_or_else(|| ErrorCode::KeyTarget.into())
                .and_then(|target| target.record_point(screen))
                .map(|point| self.settings.key_clicks.stick.center = Some(point)),
        };
        self.cancel_key_pick();
        match result {
            Ok(()) => {
                self.message = self
                    .t(
                        "ゲーム画面と登録位置を保存しました。",
                        "Game window and position saved.",
                    )
                    .into();
                self.save();
            }
            Err(error) => self.message = error_text(&error, self.settings.lang == "ja"),
        }
        self.rebuild();
    }
    fn start_key_capture(&mut self, index: usize) {
        self.cancel_capture();
        if let Some(controller) = &self.key_controller {
            controller.set_active(false);
        }
        KEY_CAPTURE_STATE.with(|state| *state.borrow_mut() = key_click::KeyCapture::default());
        self.capturing = Some(index);
        CAPTURE.store((index + 1) as u32, Ordering::Release);
        self.keyboard_hook = unsafe {
            SetWindowsHookExW(
                WH_KEYBOARD_LL,
                Some(keyboard_hook),
                GetModuleHandleW(null()),
                0,
            )
        };
        if self.keyboard_hook.is_null() {
            self.capturing = None;
            CAPTURE.store(0, Ordering::Release);
            self.message = self
                .t(
                    "キーの入力を監視できません。",
                    "Cannot capture keyboard input.",
                )
                .into();
        } else {
            self.message = self
                .t(
                    "割り当てるキーを押して離してください（Escで取消）。",
                    "Press and release the key to assign (Esc cancels).",
                )
                .into();
        }
        self.rebuild();
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
            &model::format_interval(self.settings.interval_ticks),
            320,
            353,
            160,
            false,
        );
        let reset = self.t("初期値に戻す", "Reset").to_string();
        self.button(DEFAULT_INTERVAL, &reset, 490, 353, 126, 30);
        let helper = self
            .t(
                "最小 0.0000001 秒・小数点以下7桁まで",
                "Minimum 0.0000001 s • up to 7 decimal places",
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
                    &model::format_interval(
                        p.interval_ticks.unwrap_or(self.settings.interval_ticks),
                    ),
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
            &model::format_interval(self.settings.fast_interval_ticks),
            320,
            226,
            160,
            false,
        );
        let locked = self.runner.as_ref().is_some_and(|r| r.snapshot().locked);
        let caption = self.lock_caption(locked).to_string();
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
    fn lock_caption(&self, locked: bool) -> &str {
        match (self.lock_hotkey_ready, locked) {
            (true, true) => self.t("ロック解除  Ctrl+Alt+L", "Unlock  Ctrl+Alt+L"),
            (true, false) => self.t("連打ロック  Ctrl+Alt+L", "Lock rapid click  Ctrl+Alt+L"),
            (false, true) => self.t("ロック解除（ボタンのみ）", "Unlock (button only)"),
            (false, false) => self.t("連打ロック（ボタンのみ）", "Lock rapid click (button only)"),
        }
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
                if self.lock_hotkey_ready {
                    self.t(
                        "連打ロック中（Ctrl+Alt+Lで解除）",
                        "Rapid click locked (Ctrl+Alt+L to unlock)",
                    )
                } else {
                    self.t(
                        "連打ロック中（ボタンで解除）",
                        "Rapid click locked (use the button to unlock)",
                    )
                }
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
            let label = self.lock_caption(self.last.locked);
            set_text(self.control(LOCK), label);
        }
        if let Some(error) = self
            .key_controller
            .as_ref()
            .and_then(|controller| controller.take_error())
        {
            self.handle_key_error(error);
        }
        self.sync_key_runtime(false);
        if self.tab == 2 {
            let status = if !self.hotkeys_ready {
                self.t(
                    "停止キーを含むショートカットを設定し直してください。",
                    "Configure working operation hotkeys, including Stop.",
                )
            } else if self.key_bindings_conflict() {
                self.t(
                    "操作用ショートカットと同じキーが登録されています。",
                    "A key conflicts with an operation hotkey.",
                )
            } else if !self.settings.key_clicks.enabled {
                self.t("停止中", "Disabled")
            } else if self.busy() {
                self.t(
                    "連打・位置の記録中は一時停止します。",
                    "Paused during auto-clicking or position recording.",
                )
            } else if self
                .key_target
                .as_ref()
                .is_none_or(|target| !target.alive())
            {
                self.t("ゲーム画面を選択してください。", "Choose the game window.")
            } else if self
                .key_target
                .as_ref()
                .is_some_and(|target| !target.context_matches())
            {
                self.t(
                    "ゲーム画面が切り替わりました。選び直してください。",
                    "The game window changed. Select it again.",
                )
            } else if !self.key_positions_match_target() {
                self.t(
                    "キーと位置を記憶してください。画面サイズが変わった場合も取り直してください。",
                    "Assign keys and positions. Record again after resizing the game.",
                )
            } else if self.key_controller.is_none() {
                self.t(
                    "キー操作を開始できません。設定を確認してください。",
                    "Key clicks could not start. Check the settings.",
                )
            } else {
                self.t(
                    "選択したゲーム画面が前面のとき操作できます。",
                    "Ready while the selected game window is in front.",
                )
            };
            let count = self
                .key_controller
                .as_ref()
                .map_or(0, KeyController::clicks);
            set_text(
                self.control(KEY_STATUS),
                &format!(
                    "{}\n{}: {}   {}: {}",
                    status,
                    self.t("クリック数", "Clicks"),
                    count,
                    self.t("停止", "Stop"),
                    format_hotkey(
                        self.settings
                            .hotkeys
                            .get("click_stop")
                            .map(String::as_str)
                            .unwrap_or("")
                    )
                ),
            );
            let warning = self
                .key_controller
                .as_ref()
                .and_then(|controller| controller.warning())
                .map(|error| {
                    format!(
                        "{}\n{}",
                        self.t("入力方式の注意", "Input warning"),
                        error_text(&error, self.settings.lang == "ja")
                    )
                })
                .unwrap_or_default();
            set_text(self.control(KEY_WARNING), &warning);
            set_text(self.control(MESSAGE), &self.message);
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
            interval_ticks: None,
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
            if self.tab == 2 {
                let _ = self.numbers();
            }
            self.cancel_capture();
            if let Some(controller) = &self.key_controller {
                controller.set_active(false);
            }
            if self.settings.key_clicks.enabled {
                self.settings.key_clicks.enabled = false;
                self.save();
                self.rebuild();
            }
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
        if id == NAV_MAIN || id == NAV_SETTINGS || id == NAV_KEYS {
            if !self.busy() && !self.commit() {
                return;
            }
            self.cancel_capture();
            self.tab = if id == NAV_MAIN {
                0
            } else if id == NAV_SETTINGS {
                1
            } else {
                2
            };
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
        if id == KEY_PROFILE {
            let selection = combo_selection(self.control(KEY_PROFILE));
            if let Ok(index) = usize::try_from(selection) {
                if let Some(profile) = self.settings.key_clicks.profiles.get(index) {
                    let selected = profile.id;
                    if Some(selected) != self.settings.key_clicks.selected_profile {
                        self.change_key_profile(ProfileChange::Select(selected));
                    }
                }
            }
            return;
        }
        if id == KEY_PROFILE_NEW {
            self.change_key_profile(ProfileChange::New);
            return;
        }
        if id == KEY_PROFILE_SAVE {
            self.cancel_capture();
            let name = text(self.control(KEY_PROFILE));
            self.profile_draft_name = Some(name.clone());
            if !self.commit() {
                return;
            }
            match self.settings.key_clicks.save_profile(&name) {
                Ok(_) => {
                    self.profile_draft_name = None;
                    self.message = self
                        .t("ゲームごとの設定を保存しました。", "Game profile saved.")
                        .into();
                    self.save();
                }
                Err(error) => self.message = error_text(&error, self.settings.lang == "ja"),
            }
            self.rebuild();
            return;
        }
        if id == KEY_PROFILE_DELETE {
            if let Some(id) = self.choose_key_profile_to_delete() {
                let hwnd = self.hwnd;
                self.delete_key_profile_with_confirmation(id, |prompt| unsafe {
                    MessageBoxW(
                        hwnd,
                        wide(prompt).as_ptr(),
                        w!("Multi Point Clicker"),
                        MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
                    ) == IDYES
                });
            }
            return;
        }
        if id == KEY_MOVE_BUTTONS || id == KEY_MOVE_STICK || id == KEY_INPUT {
            self.cancel_capture();
            let input = combo_selection(self.control(KEY_INPUT));
            if !self.commit() {
                unsafe {
                    SendMessageW(
                        self.control(KEY_MOVE_BUTTONS),
                        BM_SETCHECK,
                        usize::from(
                            self.settings.key_clicks.movement_mode == MovementMode::Buttons,
                        ),
                        0,
                    );
                    SendMessageW(
                        self.control(KEY_MOVE_STICK),
                        BM_SETCHECK,
                        usize::from(self.settings.key_clicks.movement_mode == MovementMode::Stick),
                        0,
                    );
                    SendMessageW(
                        self.control(KEY_INPUT),
                        CB_SETCURSEL,
                        usize::from(self.settings.key_clicks.input_method == InputMethod::Touch),
                        0,
                    );
                }
                return;
            }
            if id == KEY_INPUT {
                self.settings.key_clicks.input_method = if input == 1 {
                    InputMethod::Touch
                } else {
                    InputMethod::Mouse
                };
            } else {
                self.settings.key_clicks.movement_mode = if id == KEY_MOVE_STICK {
                    MovementMode::Stick
                } else {
                    MovementMode::Buttons
                };
            }
            self.save();
            self.rebuild();
            return;
        }
        if id == KEY_TARGET || id == KEY_STICK_CENTER {
            if self.commit() {
                self.begin_key_pick(if id == KEY_TARGET {
                    KeyPick::Game
                } else {
                    KeyPick::StickCenter
                });
            }
            return;
        }
        if (WASD_POSITION..WASD_POSITION + 4).contains(&id) {
            let row = (id - WASD_POSITION) as usize;
            if !self.commit() {
                return;
            }
            if self.key_target.is_none() {
                self.message = self
                    .t(
                        "先にゲーム画面を選んでください。",
                        "Choose the game window first.",
                    )
                    .into();
                self.rebuild();
                return;
            }
            if self.wasd_index(row).is_none() && !self.add_wasd() {
                self.rebuild();
                return;
            }
            if let Some(index) = self.wasd_index(row) {
                self.begin_key_pick(KeyPick::Position(index));
            }
            return;
        }
        if (WASD_BEHAVIOR..WASD_BEHAVIOR + 4).contains(&id)
            || (WASD_BUTTON..WASD_BUTTON + 4).contains(&id)
        {
            self.cancel_capture();
            let row = if id < WASD_BUTTON {
                id - WASD_BEHAVIOR
            } else {
                id - WASD_BUTTON
            };
            let behavior = behavior_selection(self.control(WASD_BEHAVIOR + row));
            let right = combo_selection(self.control(WASD_BUTTON + row)) == 1;
            if !self.commit() {
                return;
            }
            if self.wasd_index(row as usize).is_none() && !self.add_wasd() {
                self.rebuild();
                return;
            }
            if let Some(index) = self.wasd_index(row as usize) {
                let binding = &mut self.settings.key_clicks.bindings[index];
                binding.behavior = behavior;
                binding.right = right;
            }
            self.save();
            self.rebuild();
            return;
        }
        if (KEY_POSITION..KEY_POSITION + key_click::MAX_BINDINGS as u16).contains(&id) {
            let index = (id - KEY_POSITION) as usize;
            if index < self.settings.key_clicks.bindings.len() && self.commit() {
                self.begin_key_pick(KeyPick::Position(index));
            }
            return;
        }
        if (KEY_BIND..KEY_BIND + key_click::MAX_BINDINGS as u16).contains(&id) {
            let index = (id - KEY_BIND) as usize;
            if !self.control(id).is_null()
                && index < self.settings.key_clicks.bindings.len()
                && self.commit()
            {
                self.start_key_capture(ACTIONS.len() + index);
            }
            return;
        }
        if id == KEY_ENABLE
            || id == KEY_ADD
            || id == KEY_WASD
            || (KEY_DELETE..KEY_DELETE + key_click::MAX_BINDINGS as u16).contains(&id)
            || (KEY_BUTTON..KEY_BUTTON + key_click::MAX_BINDINGS as u16).contains(&id)
            || (KEY_BEHAVIOR..KEY_BEHAVIOR + key_click::MAX_BINDINGS as u16).contains(&id)
        {
            self.cancel_capture();
            if !self.commit() {
                return;
            }
            if id == KEY_ENABLE
                && !self.settings.key_clicks.enabled
                && (!self.hotkeys_ready || self.key_bindings_conflict())
            {
                self.message = self
                    .t(
                        "停止キーを含むショートカットと、キーの重複を確認してください。",
                        "Check the Stop hotkey and resolve conflicting keys first.",
                    )
                    .into();
                self.rebuild();
                return;
            }
            match id {
                KEY_ENABLE => self.settings.key_clicks.enabled = !self.settings.key_clicks.enabled,
                KEY_ADD if self.settings.key_clicks.bindings.len() < key_click::MAX_BINDINGS => {
                    self.settings
                        .key_clicks
                        .bindings
                        .push(KeyBinding::default());
                }
                KEY_WASD => {
                    self.add_wasd();
                }
                _ if (KEY_DELETE..KEY_DELETE + key_click::MAX_BINDINGS as u16).contains(&id) => {
                    let index = (id - KEY_DELETE) as usize;
                    if index < self.settings.key_clicks.bindings.len() {
                        self.settings.key_clicks.bindings.remove(index);
                    }
                }
                _ => {} // The editors have already been read by commit().
            }
            self.save();
            self.rebuild();
            return;
        }
        if id == MODE {
            let selection = unsafe { SendMessageW(self.control(MODE), CB_GETCURSEL, 0, 0) };
            if !self.commit() {
                let previous = match self.settings.mode {
                    ClickMode::Recorded => 0,
                    ClickMode::Area => 1,
                    ClickMode::Pointer => 2,
                    ClickMode::Drag => 3,
                };
                unsafe {
                    SendMessageW(self.control(MODE), CB_SETCURSEL, previous, 0);
                }
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
            let previous_common = self.settings.interval_ticks;
            self.settings.interval_ticks = model::DEFAULT_INTERVAL_TICKS;
            set_text(self.control(INTERVAL), "0.501");
            for (i, point) in self.settings.points.iter().enumerate() {
                let editor = self.control(INTERVAL_POINT + i as u16);
                if follows_common_interval(point.interval_ticks, &text(editor), previous_common) {
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
                if let Some(controller) = &self.key_controller {
                    controller.set_active(false);
                }
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
                self.start_key_capture(i);
            }
            _ => {}
        }
    }
    fn start_run(&mut self, locked: bool) {
        self.clear_markers();
        self.cancel_capture();
        if let Some(controller) = &self.key_controller {
            controller.set_active(false);
        }
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
        for i in 0..5 {
            unsafe {
                UnregisterHotKey(self.hwnd, i + 1);
            }
        }
        let hwnd = self.hwnd;
        self.register_hotkeys_using(|id, mods, key| unsafe {
            RegisterHotKey(hwnd, id, mods, key) != 0
        })
    }
    fn register_hotkeys_using(
        &mut self,
        mut register: impl FnMut(i32, u32, u32) -> bool,
    ) -> Result<(), String> {
        self.hotkeys_ready = false;
        self.lock_hotkey_ready = register(
            LOCK_HOTKEY,
            MOD_CONTROL | MOD_ALT | MOD_NOREPEAT,
            b'L' as u32,
        );
        for (i, action) in ACTIONS.iter().enumerate() {
            let Some((mods, key)) = parse_hotkey(&self.settings.hotkeys[*action]) else {
                self.lock_hotkey_ready = false;
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
            if !register(i as i32 + 1, mods | MOD_NOREPEAT, key) {
                self.lock_hotkey_ready = false;
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
        if i >= ACTIONS.len() {
            let index = i - ACTIONS.len();
            let key = if matches!(key, 0xa0 | 0xa1) {
                VK_SHIFT as u32
            } else {
                key
            };
            let mods = if key == VK_SHIFT as u32 {
                mods & !MOD_SHIFT
            } else {
                mods
            };
            if !key_click::supported_key(key) || mods != 0 {
                self.message = self
                    .t(
                        "SpaceやShiftなど、単独のキーを選んでください。",
                        "Choose a single key, such as Space or Shift.",
                    )
                    .into();
            } else if WASD_KEYS.contains(&key)
                && self
                    .settings
                    .key_clicks
                    .bindings
                    .get(index)
                    .is_none_or(|b| b.key != Some(key))
            {
                self.message = self
                    .t(
                        "WASDは移動用のキーです。",
                        "WASD keys are reserved for movement.",
                    )
                    .into();
            } else if self
                .settings
                .key_clicks
                .bindings
                .iter()
                .enumerate()
                .any(|(other, b)| other != index && b.key == Some(key))
                || self.key_conflict(key)
            {
                self.message = self
                    .t(
                        "そのキーは別の操作で使用中です。",
                        "That key is already assigned.",
                    )
                    .into();
            } else if let Some(binding) = self.settings.key_clicks.bindings.get_mut(index) {
                binding.key = Some(key);
                self.message = self.t("キーを保存しました。", "Key saved.").into();
                self.save();
            }
            self.rebuild();
            return;
        }
        if mods == 0
            && ((self.settings.key_clicks.movement_mode == MovementMode::Stick
                && WASD_KEYS.contains(&key))
                || self
                    .settings
                    .key_clicks
                    .bindings
                    .iter()
                    .any(|b| b.key == Some(key)))
        {
            self.message = self
                .t(
                    "そのキーは位置のクリックに割り当てられています。",
                    "That key is assigned to a position click.",
                )
                .into();
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
        self.cancel_key_pick();
        self.capturing = None;
        CAPTURE.store(0, Ordering::Relaxed);
        KEY_CAPTURE_STATE.with(|state| *state.borrow_mut() = key_click::KeyCapture::default());
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

fn reserved_key_hotkeys(
    settings: &Settings,
    hotkeys_ready: bool,
    lock_hotkey_ready: bool,
) -> Vec<(u32, u32)> {
    let mut reserved = Vec::new();
    if hotkeys_ready {
        for action in ACTIONS {
            if let Some(hotkey) = settings
                .hotkeys
                .get(action)
                .and_then(|value| parse_hotkey(value))
            {
                if !reserved.contains(&hotkey) {
                    reserved.push(hotkey);
                }
            }
        }
    }
    let lock = (MOD_CONTROL | MOD_ALT, b'L' as u32);
    if lock_hotkey_ready && !reserved.contains(&lock) {
        reserved.push(lock);
    }
    reserved
}
fn profile_deletion_candidates(settings: &key_click::KeyClickSettings) -> Vec<(u64, String)> {
    settings
        .profiles
        .iter()
        .map(|profile| (profile.id, profile.name.clone()))
        .collect()
}
fn fatal_key_error(code: &ErrorCode) -> bool {
    !matches!(
        code,
        ErrorCode::KeyTarget
            | ErrorCode::PointOutside
            | ErrorCode::ScreenChanged
            | ErrorCode::StickRange
    )
}
fn combo_selection(hwnd: HWND) -> isize {
    unsafe { SendMessageW(hwnd, CB_GETCURSEL, 0, 0) }
}
fn behavior_selection(hwnd: HWND) -> PressBehavior {
    if combo_selection(hwnd) == 1 {
        PressBehavior::Hold
    } else {
        PressBehavior::Click
    }
}
fn capture_key_event(
    state: &mut key_click::KeyCapture,
    for_binding: bool,
    key: u32,
    down: bool,
    mods: u32,
) -> (bool, Option<(u32, u32)>) {
    if for_binding {
        state.event_for_binding(key, down, mods)
    } else {
        state.event(key, down, mods)
    }
}
fn valid_hotkey_primary(key: u32) -> bool {
    (1..=254).contains(&key) && !matches!(key, 0x10..=0x12 | 0x5b | 0x5c | 0xa0..=0xa5)
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
fn key_name(key: u32) -> String {
    if key == VK_SHIFT as u32 {
        return "Shift".into();
    }
    if !matches!(key,0x30..=0x39|0x41..=0x5a|0x60..=0x69|0x70..=0x87) {
        let scan = unsafe { MapVirtualKeyW(key, MAPVK_VK_TO_VSC) };
        let mut name = [0u16; 64];
        let length =
            unsafe { GetKeyNameTextW((scan << 16) as i32, name.as_mut_ptr(), name.len() as i32) };
        if length > 0 && !matches!(key, 0x08 | 0x09 | 0x0d | 0x20..=0x28 | 0x2d | 0x2e) {
            return String::from_utf16_lossy(&name[..length as usize]);
        }
    }
    match key {
        0x08 => "Backspace".into(),
        0x09 => "Tab".into(),
        0x0d => "Enter".into(),
        0x20 => "Space".into(),
        0x21 => "Page Up".into(),
        0x22 => "Page Down".into(),
        0x23 => "End".into(),
        0x24 => "Home".into(),
        0x25 => "←".into(),
        0x26 => "↑".into(),
        0x27 => "→".into(),
        0x28 => "↓".into(),
        0x2d => "Insert".into(),
        0x2e => "Delete".into(),
        _ => format_hotkey(&combo_from_key(0, key).unwrap_or_else(|| format!("vk{key}"))),
    }
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
                            let primary = (scan as u16 & 0xff) as u32;
                            return valid_hotkey_primary(primary).then_some((mods, primary));
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
    valid_hotkey_primary(key).then_some((mods, key))
}
fn combo_from_key(mods: u32, key: u32) -> Option<String> {
    if !valid_hotkey_primary(key) || mods & !(MOD_CONTROL | MOD_ALT | MOD_SHIFT | MOD_WIN) != 0 {
        return None;
    }
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
        if event.flags & LLMHF_INJECTED != 0
            && event.dwExtraInfo == multi_point_clicker::engine::KEY_CLICK_TAG
        {
            return unsafe { CallNextHookEx(null_mut(), code, wp, lp) };
        }
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
unsafe extern "system" fn key_pick_mouse_hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 && KEY_PICKING.load(Ordering::Acquire) {
        let event = unsafe { &*(lp as *const MSLLHOOKSTRUCT) };
        if event.flags & LLMHF_INJECTED == 0 {
            if wp as u32 == WM_LBUTTONDOWN {
                KEY_PICK_ORIGIN.with(|point| *point.borrow_mut() = Some(event.pt));
                return 1;
            }
            if wp as u32 == WM_LBUTTONUP
                && KEY_PICK_ORIGIN
                    .with(|point| point.borrow_mut().take())
                    .is_some()
            {
                KEY_PICKING.store(false, Ordering::Release);
                unsafe {
                    PostMessageW(
                        ROOT.load(Ordering::Relaxed) as HWND,
                        APP_KEY_PICK,
                        event.pt.x as usize,
                        event.pt.y as isize,
                    );
                }
                return 1;
            }
        }
    }
    unsafe { CallNextHookEx(null_mut(), code, wp, lp) }
}
unsafe extern "system" fn keyboard_hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0
        && KEY_PICKING.load(Ordering::Acquire)
        && matches!(wp as u32, WM_KEYDOWN | WM_SYSKEYDOWN)
    {
        let event = unsafe { &*(lp as *const KBDLLHOOKSTRUCT) };
        if event.vkCode == VK_ESCAPE as u32 {
            unsafe {
                PostMessageW(ROOT.load(Ordering::Relaxed) as HWND, APP_KEY_CANCEL, 0, 0);
            }
            return 1;
        }
    }
    if code >= 0
        && CAPTURE.load(Ordering::Acquire) != 0
        && matches!(
            wp as u32,
            WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP
        )
    {
        let event = unsafe { &*(lp as *const KBDLLHOOKSTRUCT) };
        let mut mods = 0;
        for (key, flag) in [
            (VK_CONTROL, MOD_CONTROL),
            (VK_MENU, MOD_ALT),
            (VK_SHIFT, MOD_SHIFT),
            (VK_LWIN, MOD_WIN),
            (VK_RWIN, MOD_WIN),
        ] {
            if unsafe { GetAsyncKeyState(key as i32) } < 0 {
                mods |= flag;
            }
        }
        let for_binding = CAPTURE.load(Ordering::Acquire) > ACTIONS.len() as u32;
        let (suppress, captured) = KEY_CAPTURE_STATE.with(|state| {
            capture_key_event(
                &mut state.borrow_mut(),
                for_binding,
                event.vkCode,
                matches!(wp as u32, WM_KEYDOWN | WM_SYSKEYDOWN),
                mods,
            )
        });
        if let Some((mods, key)) = captured {
            CAPTURE.store(0, Ordering::Release);
            unsafe {
                PostMessageW(
                    ROOT.load(Ordering::Relaxed) as HWND,
                    APP_CAPTURE,
                    mods as usize,
                    key as isize,
                );
            }
        }
        if suppress {
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

fn draw_stick_preview(item: &DRAWITEMSTRUCT) {
    unsafe {
        let dc = item.hDC;
        let rect = item.rcItem;
        let scale = GetDpiForWindow(item.hwndItem).max(96) as f64 / 96.;
        let gap = (24. * scale).round() as i32;
        let cx = (rect.left + rect.right) / 2;
        let cy = (rect.top + rect.bottom) / 2 - gap / 2;
        let radius = ((rect.right - rect.left) / 2 - gap)
            .min((rect.bottom - rect.top - gap * 3) / 2)
            .max(1);
        let background = CreateSolidBrush(bg());
        FillRect(dc, &rect, background);
        DeleteObject(background);
        let pen = CreatePen(PS_SOLID, scale.round().max(1.) as i32, muted());
        let old_pen = SelectObject(dc, pen);
        let old_brush = SelectObject(dc, GetStockObject(HOLLOW_BRUSH));
        Ellipse(dc, cx - radius, cy - radius, cx + radius, cy + radius);
        MoveToEx(dc, cx - radius, cy, null_mut());
        LineTo(dc, cx + radius, cy);
        MoveToEx(dc, cx, cy - radius, null_mut());
        LineTo(dc, cx, cy + radius);
        let accent = CreatePen(
            PS_SOLID,
            (2. * scale).round().max(1.) as i32,
            color(0, 120, 212),
        );
        SelectObject(dc, accent);
        let offset = (radius as f64 / std::f64::consts::SQRT_2).round() as i32;
        MoveToEx(dc, cx, cy, null_mut());
        LineTo(dc, cx + offset, cy - offset);
        let fill = CreateSolidBrush(color(0, 120, 212));
        SelectObject(dc, fill);
        let dot = (6. * scale).round().max(2.) as i32;
        Ellipse(
            dc,
            cx + offset - dot,
            cy - offset - dot,
            cx + offset + dot,
            cy - offset + dot,
        );
        SelectObject(dc, pen);
        let center = CreateSolidBrush(muted());
        SelectObject(dc, center);
        let dot = (4. * scale).round().max(2.) as i32;
        Ellipse(dc, cx - dot, cy - dot, cx + dot, cy + dot);
        SelectObject(dc, old_brush);
        SelectObject(dc, old_pen);
        DeleteObject(pen);
        DeleteObject(accent);
        DeleteObject(fill);
        DeleteObject(center);
        let font = SendMessageW(item.hwndItem, WM_GETFONT, 0, 0) as HGDIOBJ;
        let old_font = if !font.is_null() {
            SelectObject(dc, font)
        } else {
            null_mut()
        };
        SetBkMode(dc, TRANSPARENT as i32);
        SetTextColor(dc, fg());
        for (caption, x, y) in [
            ("W", cx, cy - radius - gap / 2),
            ("A", cx - radius - gap / 2, cy),
            ("S", cx, cy + radius + gap / 2),
            ("D", cx + radius + gap / 2, cy),
        ] {
            let mut label = RECT {
                left: x - gap / 2,
                top: y - gap / 2,
                right: x + gap / 2,
                bottom: y + gap / 2,
            };
            DrawTextW(
                dc,
                wide(caption).as_ptr(),
                -1,
                &mut label,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
        }
        let mut label = RECT {
            left: rect.left,
            top: cy + radius + gap,
            right: rect.right,
            bottom: rect.bottom,
        };
        DrawTextW(
            dc,
            wide(&text(item.hwndItem)).as_ptr(),
            -1,
            &mut label,
            DT_CENTER | DT_WORDBREAK,
        );
        if !old_font.is_null() {
            SelectObject(dc, old_font);
        }
    }
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
            if id == KEY_STICK_PREVIEW {
                draw_stick_preview(d);
                return 1;
            }
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
                || (id == NAV_KEYS && ACTIVE_TAB.load(Ordering::Relaxed) == 2)
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
            APP_KEY_PICK => {
                app.key_pick_finished(wp as i32, lp as i32);
                true
            }
            APP_KEY_CANCEL => {
                app.cancel_key_pick();
                app.message = app
                    .t(
                        "位置の記憶を取り消しました。",
                        "Position capture cancelled.",
                    )
                    .into();
                app.rebuild();
                true
            }
            WM_COMMAND => {
                let id = (wp & 0xffff) as u16;
                let notification = ((wp >> 16) & 0xffff) as u32;
                if id == KEY_PROFILE && notification == CBN_EDITCHANGE {
                    app.profile_draft_name = Some(text(app.control(KEY_PROFILE)));
                }
                if app.tab == 2
                    && notification == EN_KILLFOCUS
                    && (id == KEY_STICK_RADIUS
                        || (KEY_LABEL..KEY_LABEL + key_click::MAX_BINDINGS as u16).contains(&id))
                {
                    app.commit();
                }
                if notification == BN_CLICKED
                    || ((id == MODE
                        || id == KEY_PROFILE
                        || id == KEY_INPUT
                        || (KEY_BUTTON..KEY_BUTTON + key_click::MAX_BINDINGS as u16).contains(&id)
                        || (KEY_BEHAVIOR..KEY_BEHAVIOR + key_click::MAX_BINDINGS as u16)
                            .contains(&id)
                        || (WASD_BEHAVIOR..WASD_BEHAVIOR + 4).contains(&id)
                        || (WASD_BUTTON..WASD_BUTTON + 4).contains(&id))
                        && notification == CBN_SELCHANGE)
                {
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
            APP_READY => {
                if app.saved_error
                    || app
                        .controls
                        .get(&START)
                        .is_none_or(|h| unsafe { IsWindow(h.hwnd) } == 0)
                    || multi_point_clicker::startup::notify_ready().is_err()
                {
                    unsafe {
                        PostQuitMessage(1);
                    }
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

fn startup_error(message: &str) {
    if !multi_point_clicker::startup::managed() {
        error_dialog(null_mut(), message);
    }
}

pub fn run() -> i32 {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let instance = GetModuleHandleW(null());
        let mutex = CreateMutexW(null(), 0, w!("Local\\MultiPointClickerRust"));
        if mutex.is_null() {
            let japanese =
                windows_sys::Win32::Globalization::GetUserDefaultUILanguage() & 0x3ff == 0x11;
            startup_error(if japanese {
                "アプリの起動ロックを作成できません。"
            } else {
                "Cannot create the startup lock."
            });
            return 1;
        }
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let existing = FindWindowW(w!("MultiPointClickerRust"), null());
            if !existing.is_null() {
                ShowWindow(existing, SW_RESTORE);
                SetForegroundWindow(existing);
            }
            CloseHandle(mutex);
            return 10;
        }
        let path = model::settings_path();
        let new_settings = !path.exists();
        let (mut settings, migrated) = match Settings::load(&path) {
            Ok(s) => s,
            Err(e) => {
                startup_error(&error_text(
                    &e,
                    windows_sys::Win32::Globalization::GetUserDefaultUILanguage() & 0x3ff == 0x11,
                ));
                CloseHandle(mutex);
                return 1;
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
        let (input_language_guard, input_language_error) = match InputLanguageGuard::register() {
            Ok(guard) => (Some(guard), None),
            Err(error) => (None, Some(error)),
        };
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
            startup_error(if settings.lang == "ja" {
                "アプリのウィンドウを作成できません。"
            } else {
                "Cannot create the application window."
            });
            drop(input_language_guard);
            CloseHandle(mutex);
            return 1;
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
            lock_hotkey_ready: false,
            wheel_remainder: 0,
            main_offset: 0,
            selecting_area: false,
            key_controller: None,
            key_target: None,
            key_pick: None,
            profile_draft_name: None,
        };
        if migrated {
            app.message=app.t("旧設定とクリック位置を移行しました。間隔は0.501秒から開始します。","Previous positions and settings were imported. The interval starts at 0.501 seconds.").into();
            app.save();
        }
        if repair_hotkeys(&mut app.settings) {
            app.message=if migrated {app.t("クリック位置と旧設定を移行し、未対応または重複したキーを修正しました（間隔は0.501秒から開始します）。","Imported positions and settings, repaired unsupported or duplicate hotkeys, and set the interval to 0.501 seconds.")}else{app.t("未対応または重複した旧キーを利用可能な初期キーに変更しました。設定で確認できます。","Unsupported or duplicate legacy hotkeys were reset to available defaults. Check Settings.")}.into();
            app.save();
        }
        if new_settings && multi_point_clicker::startup::managed() {
            app.save();
        }
        if let Err(e) = app.register_hotkeys() {
            app.message = e;
        }
        if let Some(error) = input_language_error {
            let warning = app.t(
                "Alt+ShiftによるWindowsの入力言語切り替えを抑止できません。",
                "Cannot suppress Windows input-language switching with Alt+Shift.",
            );
            let message = match error.raw_os_error() {
                Some(code) => format!("{warning} ({code})"),
                None => warning.to_string(),
            };
            if !app.message.is_empty() {
                app.message.push('\n');
            }
            app.message.push_str(&message);
        }
        APP.with(|cell| *cell.borrow_mut() = Some(app));
        SendMessageW(hwnd, APP_INIT, 0, 0);
        ShowWindow(hwnd, SW_SHOW);
        UpdateWindow(hwnd);
        let timer_ok = SetTimer(hwnd, TICK, 50, None) != 0;
        let posted = PostMessageW(hwnd, APP_READY, 0, 0) != 0;
        let mut exit_code = 1;
        let mut msg: MSG = zeroed();
        if timer_ok && posted {
            loop {
                let result = GetMessageW(&mut msg, null_mut(), 0, 0);
                if result <= 0 {
                    exit_code = if result == 0 { msg.wParam as i32 } else { 1 };
                    break;
                }
                if msg.hwnd.is_null()
                    && msg.message == WM_HOTKEY
                    && input_language_guard
                        .as_ref()
                        .is_some_and(|guard| guard.owns_hotkey(msg.wParam))
                {
                    continue;
                }
                if IsDialogMessageW(hwnd, &msg) == 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
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
                if let Some(mut controller) = app.key_controller.take() {
                    controller.stop();
                }
                DeleteObject(app.font);
                DeleteObject(app.title_font);
            }
        });
        if IsWindow(hwnd) != 0 {
            DestroyWindow(hwnd);
        }
        drop(input_language_guard);
        CloseHandle(mutex);
        exit_code
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
                        interval_ticks: Some(*ms * model::TICKS_PER_MILLISECOND),
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
                scenarios.push(serde_json::json!({"scenario":name,"expected_clicks":expected,"received_clicks":events.len(),"count_ok":events.len()==expected,"positions_ok":positions_ok,"buttons_ok":buttons_ok,"reported_clicks":progress.clicks,"engine_elapsed_ms":progress.elapsed_ms,"mean_input_ms":progress.mean_input_ms,"error":progress.error,"mean_interval_ticks":average,"p95_interval_ticks":sorted.get(sorted.len()*95/100),"interval_sample_ms":deltas.iter().take(12).collect::<Vec<_>>()}));
            }
            PROBE.with(|e| e.borrow_mut().clear());
            let settings = Settings {
                points: vec![Point {
                    x: origin.x + 100,
                    y: origin.y + 160,
                    interval_ticks: None,
                }],
                interval_ticks: 300_000_000,
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
    #[test]
    fn key_clicks_refuse_enable_without_a_registered_stop_and_show_custom_stop() {
        let mut fixture = TestApp::new();
        let app = &mut fixture.app;
        app.action(NAV_KEYS);
        app.action(KEY_WASD);
        assert!(app.register_hotkeys_using(|_, _, _| false).is_err());
        app.rebuild();
        app.action(KEY_ENABLE);
        assert!(!app.settings.key_clicks.enabled);
        assert!(app.key_controller.is_none());
        assert_eq!(unsafe { IsWindowEnabled(app.control(KEY_ENABLE)) }, 0);
        app.hotkeys_ready = true;
        app.settings
            .hotkeys
            .insert("click_stop".into(), "f8".into());
        app.rebuild();
        assert!(text(app.control(KEY_STATUS)).contains("F8"));
        app.action(KEY_ENABLE);
        assert!(app.settings.key_clicks.enabled);
    }
    #[test]
    fn wasd_preset_keeps_the_existing_map_when_an_operation_key_conflicts() {
        let mut fixture = TestApp::new();
        let app = &mut fixture.app;
        app.settings
            .hotkeys
            .insert("click_start".into(), "w".into());
        app.action(NAV_KEYS);
        app.action(KEY_WASD);
        assert!(app.settings.key_clicks.bindings.is_empty());
        assert_eq!(app.settings.hotkeys["click_start"], "w");
        app.action(KEY_ADD);
        app.capturing = Some(ACTIONS.len());
        app.captured(0, b'W' as u32);
        assert_eq!(app.settings.key_clicks.bindings[0].key, None);
        app.settings.key_clicks.bindings[0].key = Some(b'W' as u32);
        app.action(KEY_ENABLE);
        assert!(!app.settings.key_clicks.enabled);
        assert!(app.key_bindings_conflict());
    }
    #[test]
    fn key_click_controls_save_independent_wasd_and_preserve_other_settings() {
        let mut fixture = TestApp::new();
        let app = &mut fixture.app;
        app.settings.delay_ms = 7000;
        app.settings.points.push(Point {
            x: 31,
            y: 42,
            interval_ticks: Some(1),
        });
        app.action(NAV_KEYS);
        assert_eq!(app.tab, 2);
        app.action(KEY_WASD);
        assert_eq!(app.settings.key_clicks.bindings.len(), 4);
        let game = key_click::GameIdentity {
            executable: "C:/Games/example.exe".into(),
            class: "ExampleWindow".into(),
            title: "Example".into(),
        };
        app.settings.key_clicks.last_target = Some(game.clone());
        for (binding, (x, y)) in app.settings.key_clicks.bindings.iter_mut().zip([
            (180, 690),
            (140, 730),
            (180, 770),
            (220, 730),
        ]) {
            binding.position = Some(key_click::ClientPoint {
                game: game.clone(),
                x,
                y,
                width: 1280,
                height: 900,
            });
        }
        app.capturing = Some(ACTIONS.len() + 1);
        app.captured(0, b'W' as u32);
        assert_eq!(app.settings.key_clicks.bindings[1].key, Some(b'A' as u32));
        app.capturing = Some(ACTIONS.len() + 1);
        app.captured(0, b'E' as u32);
        assert_eq!(app.settings.key_clicks.bindings[1].key, Some(b'E' as u32));
        unsafe {
            SendMessageW(app.control(WASD_BUTTON), CB_SETCURSEL, 1, 0);
        }
        app.action(WASD_BUTTON);
        assert!(app.settings.key_clicks.bindings[0].right);
        app.action(KEY_ENABLE);
        assert!(app.settings.key_clicks.enabled);
        assert!(app.key_controller.is_none());
        let (loaded, _) = Settings::load(&app.path).unwrap();
        assert_eq!(loaded.key_clicks, app.settings.key_clicks);
        assert_eq!(loaded.delay_ms, 7000);
        assert_eq!(loaded.points, app.settings.points);
        app.action(KEY_DELETE + 1);
        assert_eq!(app.settings.key_clicks.bindings.len(), 3);
        app.action(NAV_SETTINGS);
        assert_eq!(text(app.control(DELAY)), "7");
        app.action(NAV_MAIN);
        assert_eq!(app.settings.points[0].interval_ticks, Some(1));
    }
    fn select_key_combo(app: &mut App, id: u16, selection: usize) {
        assert!(!app.control(id).is_null());
        unsafe {
            SendMessageW(app.control(id), CB_SETCURSEL, selection, 0);
        }
        app.action(id);
    }
    fn saved_game_point() -> key_click::ClientPoint {
        key_click::ClientPoint {
            game: key_click::GameIdentity {
                executable: "C:/Games/example.exe".into(),
                class: "ExampleWindow".into(),
                title: "Example".into(),
            },
            x: 300,
            y: 350,
            width: 1280,
            height: 900,
        }
    }
    #[test]
    fn movement_placeholders_do_not_create_or_save_example_actions() {
        let mut fixture = TestApp::new();
        let app = &mut fixture.app;
        let hotkeys = app.settings.hotkeys.clone();
        app.action(NAV_KEYS);
        assert_eq!(unsafe { IsWindowVisible(app.hwnd) }, 0);
        for row in 0..4 {
            assert!(!app.control(WASD_POSITION + row).is_null());
            assert_eq!(combo_selection(app.control(WASD_BEHAVIOR + row)), 1);
        }
        assert!(app.settings.key_clicks.bindings.is_empty());
        assert!(app.settings.key_clicks.profiles.is_empty());
        assert_eq!(text(app.control(KEY_PROFILE)), "");
        assert!(app.commit());
        app.action(KEY_MOVE_STICK);
        assert!(!app.control(KEY_STICK_PREVIEW).is_null());
        assert!(app.settings.key_clicks.stick.center.is_none());
        app.action(NAV_SETTINGS);
        app.action(NAV_MAIN);
        let (loaded, _) = Settings::load(&app.path).unwrap();
        assert!(loaded.key_clicks.bindings.is_empty());
        assert!(loaded.key_clicks.profiles.is_empty());
        assert!(loaded.key_clicks.last_target.is_none());
        assert_eq!(loaded.hotkeys, hotkeys);
    }
    #[test]
    fn fixed_wasd_rows_keep_legacy_clicks_and_only_add_bindings_on_edit() {
        let mut fixture = TestApp::new();
        let app = &mut fixture.app;
        let legacy: KeyBinding = serde_json::from_value(serde_json::json!({
            "key": b'W' as u32, "position": saved_game_point(), "right": true,
        }))
        .unwrap();
        assert_eq!(legacy.behavior, PressBehavior::Click);
        app.settings.key_clicks.bindings = vec![
            legacy.clone(),
            KeyBinding {
                key: Some(b'E' as u32),
                ..KeyBinding::default()
            },
        ];
        app.settings.key_clicks.last_target = Some(saved_game_point().game);
        app.action(NAV_KEYS);
        assert_eq!(combo_selection(app.control(WASD_BEHAVIOR)), 0);
        assert_eq!(combo_selection(app.control(WASD_BUTTON)), 1);
        assert!(app.control(KEY_BIND).is_null());
        assert!(!app.control(KEY_BIND + 1).is_null());
        assert!(text(app.control(WASD_POSITION)).contains("300"));
        app.action(KEY_MOVE_STICK);
        select_key_combo(app, KEY_INPUT, 1);
        app.action(KEY_MOVE_BUTTONS);
        assert_eq!(
            app.settings.key_clicks.bindings,
            vec![
                legacy.clone(),
                KeyBinding {
                    key: Some(b'E' as u32),
                    ..KeyBinding::default()
                }
            ]
        );
        select_key_combo(app, WASD_BEHAVIOR + 1, 0);
        assert_eq!(app.settings.key_clicks.bindings.len(), 5);
        assert_eq!(app.settings.key_clicks.bindings[0], legacy);
        let left = &app.settings.key_clicks.bindings[app.wasd_index(1).unwrap()];
        assert_eq!(left.behavior, PressBehavior::Click);
        assert!(left.position.is_none());
        for row in [2, 3] {
            let binding = &app.settings.key_clicks.bindings[app.wasd_index(row).unwrap()];
            assert_eq!(binding.behavior, PressBehavior::Hold);
            assert!(binding.position.is_none());
        }
        let (loaded, _) = Settings::load(&app.path).unwrap();
        assert_eq!(loaded.key_clicks.bindings[0], legacy);
    }
    #[test]
    fn profiles_keep_pending_edits_and_switch_without_reusing_a_live_target() {
        let mut fixture = TestApp::new();
        let app = &mut fixture.app;
        let hotkeys = app.settings.hotkeys.clone();
        app.settings.delay_ms = 7000;
        app.settings.points = vec![Point {
            x: 31,
            y: 42,
            interval_ticks: Some(1),
        }];
        app.action(NAV_KEYS);
        app.action(KEY_ADD);
        app.capturing = Some(ACTIONS.len());
        app.captured(0, b'E' as u32);
        app.settings.key_clicks.bindings[0].position = Some(saved_game_point());
        app.settings.key_clicks.last_target = Some(saved_game_point().game);
        app.settings.key_clicks.stick.center = Some(saved_game_point());
        app.rebuild();
        set_text(app.control(KEY_LABEL), "Action label");
        select_key_combo(app, KEY_BEHAVIOR, 1);
        select_key_combo(app, KEY_BUTTON, 1);
        app.action(KEY_MOVE_STICK);
        select_key_combo(app, KEY_INPUT, 1);
        set_text(app.control(KEY_STICK_RADIUS), "120");
        set_text(app.control(KEY_PROFILE), "First");
        app.action(KEY_PROFILE_SAVE);
        let first = app.settings.key_clicks.selected_profile.unwrap();
        assert_eq!(
            app.settings.key_clicks.selected_profile_name(),
            Some("First")
        );
        set_text(app.control(KEY_STICK_RADIUS), "140");
        set_text(app.control(KEY_LABEL), "Pending first edit");
        app.action(KEY_PROFILE_NEW);
        assert!(!app.settings.key_clicks.enabled);
        assert!(app.key_target.is_none());
        assert_eq!(app.settings.key_clicks.input_method, InputMethod::Touch);
        let saved_first = app
            .settings
            .key_clicks
            .profiles
            .iter()
            .find(|p| p.id == first)
            .unwrap();
        assert_eq!(saved_first.stick.radius_px, 140);
        assert_eq!(saved_first.bindings[0].label, "Pending first edit");
        app.action(KEY_ADD);
        app.capturing = Some(ACTIONS.len());
        app.captured(0, VK_SHIFT as u32);
        set_text(app.control(KEY_PROFILE), "Second");
        app.action(KEY_PROFILE_SAVE);
        let second = app.settings.key_clicks.selected_profile.unwrap();
        assert_ne!(first, second);
        set_text(app.control(KEY_LABEL), "Pending second edit");
        app.settings.key_clicks.enabled = true;
        let index = app
            .settings
            .key_clicks
            .profiles
            .iter()
            .position(|p| p.id == first)
            .unwrap();
        select_key_combo(app, KEY_PROFILE, index);
        assert_eq!(app.settings.key_clicks.selected_profile, Some(first));
        assert_eq!(app.settings.key_clicks.movement_mode, MovementMode::Stick);
        assert_eq!(app.settings.key_clicks.input_method, InputMethod::Touch);
        assert_eq!(
            app.settings.key_clicks.stick.center,
            Some(saved_game_point())
        );
        assert_eq!(text(app.control(KEY_STICK_RADIUS)), "140");
        assert_eq!(text(app.control(KEY_LABEL)), "Pending first edit");
        assert_eq!(
            app.settings.key_clicks.bindings[0].behavior,
            PressBehavior::Hold
        );
        assert!(app.settings.key_clicks.bindings[0].right);
        assert!(!app.settings.key_clicks.enabled);
        assert!(app.key_controller.is_none());
        assert!(app.key_target.is_none());
        let saved_second = app
            .settings
            .key_clicks
            .profiles
            .iter()
            .find(|p| p.id == second)
            .unwrap();
        assert_eq!(saved_second.bindings[0].label, "Pending second edit");
        let (loaded, _) = Settings::load(&app.path).unwrap();
        assert_eq!(loaded.key_clicks, app.settings.key_clicks);
        assert_eq!(loaded.hotkeys, hotkeys);
        assert_eq!(loaded.delay_ms, 7000);
        assert_eq!(loaded.points, app.settings.points);
        app.action(NAV_SETTINGS);
        assert_eq!(text(app.control(DELAY)), "7");
        app.action(NAV_MAIN);
        assert_eq!(app.settings.points[0].interval_ticks, Some(1));
    }
    #[test]
    fn invalid_stick_radius_preserves_the_editor_and_blocks_profile_changes() {
        let mut fixture = TestApp::new();
        let app = &mut fixture.app;
        app.action(NAV_KEYS);
        set_text(app.control(KEY_PROFILE), "First");
        app.action(KEY_PROFILE_SAVE);
        let first = app.settings.key_clicks.selected_profile.unwrap();
        app.action(KEY_MOVE_STICK);
        let previous_radius = app.settings.key_clicks.stick.radius_px;
        for value in ["3", "2049", "4294967296", "invalid", ""] {
            set_text(app.control(KEY_STICK_RADIUS), value);
            assert!(!app.commit(), "{value}");
            assert_eq!(app.settings.key_clicks.stick.radius_px, previous_radius);
            app.action(KEY_MOVE_BUTTONS);
            assert_eq!(app.settings.key_clicks.movement_mode, MovementMode::Stick);
            assert_eq!(text(app.control(KEY_STICK_RADIUS)), value);
            app.change_key_profile(ProfileChange::New);
            assert_eq!(app.settings.key_clicks.selected_profile, Some(first));
            assert_eq!(text(app.control(KEY_STICK_RADIUS)), value);
        }
        set_text(app.control(KEY_STICK_RADIUS), "2048");
        assert!(app.commit());
        let (loaded, _) = Settings::load(&app.path).unwrap();
        assert_eq!(loaded.key_clicks.stick.radius_px, 2048);
        assert_eq!(loaded.key_clicks.profiles[0].stick.radius_px, 2048);
    }
    #[test]
    fn profile_name_failures_keep_the_draft_and_delete_uses_the_non_dialog_path() {
        let mut fixture = TestApp::new();
        let app = &mut fixture.app;
        app.action(NAV_KEYS);
        set_text(app.control(KEY_PROFILE), "First");
        app.action(KEY_PROFILE_SAVE);
        let first = app.settings.key_clicks.selected_profile.unwrap();
        app.action(KEY_PROFILE_NEW);
        set_text(app.control(KEY_PROFILE), "Second");
        app.action(KEY_PROFILE_SAVE);
        let second = app.settings.key_clicks.selected_profile.unwrap();
        for name in ["", "First"] {
            set_text(app.control(KEY_PROFILE), name);
            app.action(KEY_PROFILE_SAVE);
            assert_eq!(app.settings.key_clicks.selected_profile, Some(second));
            assert_eq!(app.settings.key_clicks.profiles.len(), 2);
            assert_eq!(text(app.control(KEY_PROFILE)), name);
            assert!(app.message.contains("設定名"));
        }
        app.delete_key_profile_with_confirmation(second, |prompt| {
            assert!(prompt.contains("Second"));
            true
        });
        assert!(
            app.settings
                .key_clicks
                .profiles
                .iter()
                .any(|p| p.id == first)
        );
        assert!(
            !app.settings
                .key_clicks
                .profiles
                .iter()
                .any(|p| p.id == second)
        );
        assert!(!app.settings.key_clicks.enabled);
        assert!(app.key_target.is_none());
        let (loaded, _) = Settings::load(&app.path).unwrap();
        assert_eq!(loaded.key_clicks, app.settings.key_clicks);
    }
    #[test]
    fn key_layout_keeps_profile_management_together_and_follows_language_theme_and_dpi() {
        let mut fixture = TestApp::new();
        let app = &mut fixture.app;
        app.settings.lang = "en".into();
        app.settings.theme = "light".into();
        app.settings
            .hotkeys
            .insert("click_stop".into(), "f13".into());
        app.scale = 1.5;
        app.refresh_fonts();
        app.action(NAV_KEYS);
        let row = app.controls[&KEY_PROFILE];
        for id in [KEY_PROFILE_SAVE, KEY_PROFILE_NEW, KEY_PROFILE_DELETE] {
            let control = app.controls[&id];
            assert_eq!(control.y, row.y);
            assert!(control.x >= row.x + row.w);
        }
        assert!(text(app.control(KEY_ENABLE)).contains("Enable"));
        assert!(text(app.control(KEY_STATUS)).contains("F13"));
        app.action(KEY_MOVE_STICK);
        set_text(app.control(KEY_STICK_RADIUS), "4");
        select_key_combo(app, KEY_INPUT, 1);
        assert_eq!(app.settings.key_clicks.stick.radius_px, 4);
        assert!(text(app.control(KEY_STICK_PREVIEW)).contains("preview"));
        app.scroll_by(app.s(300));
        assert!(app.scroll > 0);
        assert_eq!(unsafe { IsWindowVisible(app.hwnd) }, 0);
        assert_eq!(app.settings.lang, "en");
        assert_eq!(app.settings.theme, "light");
        let (loaded, _) = Settings::load(&app.path).unwrap();
        assert_eq!(loaded.lang, "en");
        assert_eq!(loaded.theme, "light");
        assert_eq!(loaded.hotkeys["click_stop"], "f13");
    }
    #[test]
    fn shift_capture_is_only_enabled_for_game_bindings() {
        let mut hotkey = key_click::KeyCapture::default();
        assert!(
            capture_key_event(&mut hotkey, false, VK_LSHIFT as u32, true, MOD_SHIFT)
                .1
                .is_none()
        );
        assert!(
            capture_key_event(&mut hotkey, false, VK_LSHIFT as u32, false, 0)
                .1
                .is_none()
        );
        let mut binding = key_click::KeyCapture::default();
        capture_key_event(&mut binding, true, VK_LSHIFT as u32, true, MOD_SHIFT);
        let (_, assigned) = capture_key_event(&mut binding, true, VK_LSHIFT as u32, false, 0);
        let (mods, key) = assigned.expect("A released Shift can be assigned to a game action");
        let mut fixture = TestApp::new();
        let app = &mut fixture.app;
        app.action(NAV_KEYS);
        app.action(KEY_ADD);
        app.capturing = Some(ACTIONS.len());
        app.captured(mods, key);
        assert_eq!(
            app.settings.key_clicks.bindings[0].key,
            Some(VK_SHIFT as u32)
        );
        assert_eq!(text(app.control(KEY_BIND)), "Shift");
        let previous = app.settings.hotkeys.clone();
        app.capturing = Some(0);
        app.captured(0, VK_SHIFT as u32);
        assert_eq!(app.settings.hotkeys, previous);
        app.settings.key_clicks.movement_mode = MovementMode::Stick;
        app.rebuild();
        app.capturing = Some(0);
        app.captured(0, b'W' as u32);
        assert_eq!(app.settings.hotkeys, previous);
        for key in [
            0x10, 0x11, 0x12, 0x5b, 0x5c, 0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5,
        ] {
            assert!(parse_hotkey(&format!("vk{key}")).is_none());
            assert!(parse_hotkey(&format!("ctrl+vk{key}")).is_none());
            assert!(combo_from_key(0, key).is_none());
        }
        assert_eq!(parse_hotkey("shift+w"), Some((MOD_SHIFT, b'W' as u32)));
        let mut chord = key_click::KeyCapture::default();
        for (key, down, mods) in [
            (VK_LSHIFT as u32, true, MOD_SHIFT),
            (b'W' as u32, true, MOD_SHIFT),
            (b'W' as u32, false, MOD_SHIFT),
            (VK_LSHIFT as u32, false, 0),
        ] {
            let (_, assigned) = capture_key_event(&mut chord, true, key, down, mods);
            assert!(assigned.is_none_or(|(_, key)| !matches!(key, 0x10 | 0xa0 | 0xa1)));
        }
    }
    #[test]
    fn movement_errors_have_japanese_and_english_messages() {
        for code in [
            ErrorCode::StickRange,
            ErrorCode::ProfileName,
            ErrorCode::ProfileLimit,
            ErrorCode::InputCombination,
            ErrorCode::InputRightButton,
            ErrorCode::InputMouseMoved,
            ErrorCode::InputMouseBusy,
            ErrorCode::InputInitialize,
        ] {
            let error: ClickerError = code.into();
            let japanese = error_text(&error, true);
            let english = error_text(&error, false);
            assert_ne!(japanese, english);
            assert!(!japanese.is_ascii());
            assert!(english.is_ascii() || error.code == ErrorCode::StickRange);
        }
    }
    #[test]
    fn reserved_hotkeys_include_shift_stop_and_only_registered_operations() {
        let mut settings = Settings {
            hotkeys: BTreeMap::from([
                ("record_start".into(), "shift+a".into()),
                ("record_stop".into(), "shift+s".into()),
                ("click_start".into(), "ctrl+f11".into()),
                ("click_stop".into(), "shift+w".into()),
                ("unused".into(), "shift+d".into()),
            ]),
            ..Settings::default()
        };
        let operations = vec![
            (MOD_SHIFT, b'A' as u32),
            (MOD_SHIFT, b'S' as u32),
            (MOD_CONTROL, VK_F11 as u32),
            (MOD_SHIFT, b'W' as u32),
        ];
        let lock = (MOD_CONTROL | MOD_ALT, b'L' as u32);
        let mut all = operations.clone();
        all.push(lock);
        assert_eq!(reserved_key_hotkeys(&settings, true, true), all);
        assert_eq!(reserved_key_hotkeys(&settings, true, false), operations);
        assert_eq!(reserved_key_hotkeys(&settings, false, true), vec![lock]);
        assert!(reserved_key_hotkeys(&settings, false, false).is_empty());
        settings.hotkeys.insert("click_stop".into(), "f13".into());
        let reserved = reserved_key_hotkeys(&settings, true, true);
        assert!(reserved.contains(&(0, VK_F13 as u32)));
        assert!(!reserved.contains(&(MOD_SHIFT, b'W' as u32)));
        assert!(!reserved.contains(&(MOD_SHIFT, b'D' as u32)));
        settings
            .hotkeys
            .insert("record_start".into(), "invalid".into());
        assert!(!reserved_key_hotkeys(&settings, true, true).contains(&(MOD_SHIFT, b'A' as u32)));
    }
    fn fill_profile_slots(app: &mut App) {
        app.settings.key_clicks.profiles = (1..=key_click::MAX_PROFILES as u64)
            .map(|id| key_click::GameProfile {
                id,
                name: format!("Saved {id}"),
                ..key_click::GameProfile::default()
            })
            .collect();
        app.settings.key_clicks.selected_profile = None;
    }
    #[test]
    fn deleting_a_saved_profile_at_capacity_keeps_an_unselected_draft_and_allows_its_return() {
        let mut fixture = TestApp::new();
        let app = &mut fixture.app;
        fill_profile_slots(app);
        let point = saved_game_point();
        app.settings.key_clicks.last_target = Some(point.game.clone());
        app.settings.key_clicks.bindings = vec![KeyBinding {
            key: Some(b'E' as u32),
            position: Some(point.clone()),
            right: true,
            behavior: PressBehavior::Hold,
            label: "Draft action".into(),
        }];
        app.settings.key_clicks.movement_mode = MovementMode::Stick;
        app.settings.key_clicks.input_method = InputMethod::Mouse;
        app.settings.key_clicks.stick.center = Some(point);
        app.action(NAV_KEYS);
        assert_eq!(unsafe { IsWindowEnabled(app.control(KEY_PROFILE_NEW)) }, 0);
        assert_ne!(
            unsafe { IsWindowEnabled(app.control(KEY_PROFILE_DELETE)) },
            0
        );
        assert!(app.settings.key_clicks.selected_profile.is_none());
        let candidates = profile_deletion_candidates(&app.settings.key_clicks);
        assert_eq!(candidates.len(), key_click::MAX_PROFILES);
        let (deleted, name) = candidates.last().unwrap().clone();
        assert_eq!(name, format!("Saved {}", key_click::MAX_PROFILES));
        set_text(app.control(KEY_PROFILE), "Draft name");
        set_text(app.control(KEY_STICK_RADIUS), "140");
        set_text(app.control(KEY_LABEL), "Pending draft action");
        assert!(app.commit());
        let draft = app.settings.key_clicks.clone();
        let saved_profiles = draft.profiles.clone();
        select_key_combo(app, KEY_PROFILE, 0);
        assert_eq!(app.settings.key_clicks.selected_profile, None);
        assert_eq!(app.settings.key_clicks.profiles, saved_profiles);
        assert_eq!(text(app.control(KEY_LABEL)), "Pending draft action");
        app.delete_key_profile_with_confirmation(deleted, |prompt| {
            assert!(prompt.contains(&name));
            true
        });
        assert_eq!(
            app.settings.key_clicks.profiles.len(),
            key_click::MAX_PROFILES - 1
        );
        assert_eq!(app.settings.key_clicks.selected_profile, None);
        assert_eq!(app.settings.key_clicks.bindings, draft.bindings);
        assert_eq!(app.settings.key_clicks.last_target, draft.last_target);
        assert_eq!(app.settings.key_clicks.stick, draft.stick);
        assert_eq!(app.settings.key_clicks.movement_mode, draft.movement_mode);
        assert_eq!(app.settings.key_clicks.input_method, draft.input_method);
        assert_eq!(text(app.control(KEY_PROFILE)), "Draft name");
        assert_eq!(text(app.control(KEY_STICK_RADIUS)), "140");
        assert_ne!(unsafe { IsWindowEnabled(app.control(KEY_PROFILE_NEW)) }, 0);
        assert!(!app.saved_error, "{}", app.message);
        let (loaded, _) = Settings::load(&app.path).unwrap();
        assert_eq!(loaded.key_clicks, app.settings.key_clicks);
        select_key_combo(app, KEY_PROFILE, 0);
        assert_eq!(app.settings.key_clicks.selected_profile, Some(1));
        assert_eq!(
            app.settings.key_clicks.profiles.len(),
            key_click::MAX_PROFILES
        );
        let archived = app
            .settings
            .key_clicks
            .profiles
            .iter()
            .find(|profile| profile.bindings == draft.bindings)
            .unwrap();
        let archived_id = archived.id;
        assert_eq!(archived.stick, draft.stick);
        let index = app
            .settings
            .key_clicks
            .profiles
            .iter()
            .position(|p| p.id == archived_id)
            .unwrap();
        select_key_combo(app, KEY_PROFILE, index);
        assert_eq!(app.settings.key_clicks.selected_profile, Some(archived_id));
        assert_eq!(app.settings.key_clicks.bindings, draft.bindings);
        assert_eq!(app.settings.key_clicks.stick, draft.stick);
        assert_eq!(text(app.control(KEY_LABEL)), "Pending draft action");
        assert_eq!(text(app.control(KEY_STICK_RADIUS)), "140");
        assert!(app.key_controller.is_none());
        assert_eq!(unsafe { IsWindowVisible(app.hwnd) }, 0);
    }
    #[test]
    fn an_empty_unselected_editor_at_capacity_can_delete_and_select_without_creating_a_draft() {
        let mut fixture = TestApp::new();
        let app = &mut fixture.app;
        fill_profile_slots(app);
        app.settings.key_clicks.input_method = InputMethod::Touch;
        app.action(NAV_KEYS);
        assert_ne!(
            unsafe { IsWindowEnabled(app.control(KEY_PROFILE_DELETE)) },
            0
        );
        let candidates = profile_deletion_candidates(&app.settings.key_clicks);
        let (deleted, name) = candidates.last().unwrap().clone();
        app.delete_key_profile_with_confirmation(deleted, |prompt| {
            assert!(prompt.contains(&name));
            true
        });
        assert_eq!(app.settings.key_clicks.selected_profile, None);
        assert!(app.settings.key_clicks.bindings.is_empty());
        assert!(app.settings.key_clicks.last_target.is_none());
        assert_eq!(
            app.settings.key_clicks.profiles.len(),
            key_click::MAX_PROFILES - 1
        );
        select_key_combo(app, KEY_PROFILE, 0);
        assert_eq!(app.settings.key_clicks.selected_profile, Some(1));
        assert_eq!(
            app.settings.key_clicks.profiles.len(),
            key_click::MAX_PROFILES - 1
        );
        let (loaded, _) = Settings::load(&app.path).unwrap();
        assert_eq!(loaded.key_clicks, app.settings.key_clicks);
    }
    #[test]
    fn deleting_an_unselected_saved_profile_requires_confirmation_of_its_name() {
        let mut fixture = TestApp::new();
        let app = &mut fixture.app;
        fill_profile_slots(app);
        app.action(NAV_KEYS);
        let previous = app.settings.key_clicks.clone();
        let mut confirmed = false;
        app.delete_key_profile_with_confirmation(2, |prompt| {
            confirmed = true;
            assert!(prompt.contains("Saved 2"));
            false
        });
        assert!(confirmed);
        assert_eq!(app.settings.key_clicks, previous);
        app.delete_key_profile_with_confirmation(u64::MAX, |_| {
            panic!("An invalid ID has no deletion prompt")
        });
        assert_eq!(app.settings.key_clicks, previous);
    }
    #[test]
    fn invalid_profile_edits_keep_the_enabled_state_and_never_retire_the_current_target() {
        let mut fixture = TestApp::new();
        let app = &mut fixture.app;
        app.action(NAV_KEYS);
        set_text(app.control(KEY_PROFILE), "First");
        app.action(KEY_PROFILE_SAVE);
        let first = app.settings.key_clicks.selected_profile;
        app.action(KEY_MOVE_STICK);
        app.action(KEY_ENABLE);
        assert!(app.settings.key_clicks.enabled);
        let previous = app.settings.key_clicks.clone();
        let mut stopped = 0;
        // The token represents a live target without constructing a native controller or target.
        let mut target = Some(7usize);
        for radius in ["invalid", "3", "2049"] {
            set_text(app.control(KEY_STICK_RADIUS), radius);
            app.change_key_profile_using(ProfileChange::New, |app| {
                stopped += 1;
                target = None;
                app.stop_key_profile_runtime();
            });
            assert_eq!(stopped, 0);
            assert_eq!(target, Some(7));
            assert_eq!(app.settings.key_clicks, previous);
            assert_eq!(app.settings.key_clicks.selected_profile, first);
            assert_eq!(
                unsafe { SendMessageW(app.control(KEY_ENABLE), BM_GETCHECK, 0, 0) },
                1
            );
            assert_eq!(text(app.control(KEY_STICK_RADIUS)), radius);
            assert!(app.key_controller.is_none());
        }
        set_text(app.control(KEY_STICK_RADIUS), "140");
        app.change_key_profile_using(ProfileChange::New, |app| {
            stopped += 1;
            target = None;
            app.stop_key_profile_runtime();
        });
        assert_eq!(stopped, 1);
        assert!(target.is_none());
        assert!(!app.settings.key_clicks.enabled);
        assert_eq!(app.settings.key_clicks.selected_profile, None);
        assert_eq!(app.settings.key_clicks.profiles[0].stick.radius_px, 140);
        assert_eq!(unsafe { IsWindowVisible(app.hwnd) }, 0);
    }
    #[test]
    fn input_warnings_explain_the_correct_recovery_in_both_languages() {
        for (code, japanese_terms, english_terms) in [
            (
                ErrorCode::InputRightButton,
                ["タッチ", "右クリック", "マウス"],
                ["Touch", "right-click", "Choose Mouse"],
            ),
            (
                ErrorCode::InputMouseMoved,
                ["手動", "キーを離して", "押し直し"],
                ["Manual mouse", "Release the assigned key", "press it again"],
            ),
            (
                ErrorCode::InputMouseBusy,
                ["物理マウス", "ボタン", "押し直し"],
                [
                    "physical mouse button",
                    "Release the mouse button",
                    "press the key again",
                ],
            ),
        ] {
            let error = ClickerError::with_detail(code, "backend-only warning detail");
            let japanese = error_text(&error, true);
            let english = error_text(&error, false);
            assert!(!japanese.contains("backend-only"));
            assert!(!english.contains("backend-only"));
            for term in japanese_terms {
                assert!(japanese.contains(term), "{japanese}");
            }
            for term in english_terms {
                assert!(english.contains(term), "{english}");
            }
            assert!(!japanese.contains("同時"));
            assert!(!english.contains("at once"));
        }
        let combination: ClickerError = ErrorCode::InputCombination.into();
        assert!(error_text(&combination, true).contains("同時に保持"));
        assert!(error_text(&combination, false).contains("hold separate positions at once"));
    }
    #[test]
    fn key_error_recovery_is_limited_to_target_position_size_and_stick_guidance() {
        for code in [
            ErrorCode::KeyTarget,
            ErrorCode::PointOutside,
            ErrorCode::ScreenChanged,
            ErrorCode::StickRange,
        ] {
            assert!(!fatal_key_error(&code), "{code:?}");
        }
        for code in [
            ErrorCode::InputSend,
            ErrorCode::InputInitialize,
            ErrorCode::WorkerPanic,
            ErrorCode::InputMonitor,
            ErrorCode::ThreadStart,
            ErrorCode::ScreenUnavailable,
            ErrorCode::KeyMapping,
            ErrorCode::InputCombination,
            ErrorCode::InputRightButton,
            ErrorCode::InputMouseMoved,
            ErrorCode::InputMouseBusy,
            ErrorCode::Other,
        ] {
            assert!(fatal_key_error(&code), "{code:?}");
        }
    }
    #[test]
    fn fatal_key_errors_disable_and_persist_without_starting_a_controller() {
        for code in [
            ErrorCode::InputSend,
            ErrorCode::InputInitialize,
            ErrorCode::WorkerPanic,
            ErrorCode::InputMonitor,
            ErrorCode::ThreadStart,
            ErrorCode::ScreenUnavailable,
        ] {
            let mut fixture = TestApp::new();
            let app = &mut fixture.app;
            app.settings.lang = "en".into();
            app.settings.interval_ticks = 1;
            app.settings.points.push(Point {
                x: 31,
                y: 42,
                interval_ticks: Some(7),
            });
            app.settings.delay_ms = 7000;
            app.rebuild();
            app.action(NAV_KEYS);
            app.action(KEY_WASD);
            set_text(app.control(KEY_PROFILE), "First");
            app.action(KEY_PROFILE_SAVE);
            app.action(KEY_ENABLE);
            assert!(app.settings.key_clicks.enabled);
            assert_eq!(
                unsafe { SendMessageW(app.control(KEY_ENABLE), BM_GETCHECK, 0, 0) },
                BST_CHECKED as isize
            );
            let mut expected = app.settings.clone();
            expected.key_clicks.enabled = false;
            let error = ClickerError::with_detail(code, "Input failure detail");
            let detail = error_text(&error, false);
            app.handle_key_error(error);
            assert!(!app.settings.key_clicks.enabled);
            assert_eq!(
                unsafe { SendMessageW(app.control(KEY_ENABLE), BM_GETCHECK, 0, 0) },
                BST_UNCHECKED as isize
            );
            assert!(app.key_controller.is_none());
            assert!(!app.saved_error);
            app.update_stats();
            app.update_stats();
            let status = text(app.control(KEY_STATUS));
            assert!(status.contains("Disabled"));
            assert!(!status.contains("Ready"));
            assert!(text(app.control(MESSAGE)).contains("Key controls stopped"));
            assert!(app.message.contains(&detail));
            let (loaded, _) = Settings::load(&app.path).unwrap();
            assert_eq!(
                serde_json::to_value(&loaded).unwrap(),
                serde_json::to_value(&expected).unwrap()
            );
            assert_eq!(unsafe { IsWindowVisible(app.hwnd) }, 0);
        }
    }
    #[test]
    fn transient_key_errors_and_stick_guidance_keep_key_controls_enabled() {
        for code in [
            ErrorCode::KeyTarget,
            ErrorCode::PointOutside,
            ErrorCode::ScreenChanged,
            ErrorCode::StickRange,
        ] {
            let mut fixture = TestApp::new();
            let app = &mut fixture.app;
            app.action(NAV_KEYS);
            app.action(KEY_ENABLE);
            let previous = app.settings.key_clicks.clone();
            let error: ClickerError = code.into();
            let detail = error_text(&error, true);
            app.handle_key_error(error);
            app.update_stats();
            assert_eq!(app.settings.key_clicks, previous);
            assert_eq!(
                unsafe { SendMessageW(app.control(KEY_ENABLE), BM_GETCHECK, 0, 0) },
                BST_CHECKED as isize
            );
            assert_eq!(app.message, detail);
            assert!(!app.saved_error);
            assert!(app.key_controller.is_none());
        }
    }
    #[test]
    fn fatal_key_errors_on_other_tabs_keep_pending_editors_and_unrelated_settings() {
        for tab in [NAV_MAIN, NAV_SETTINGS] {
            let mut fixture = TestApp::new();
            let app = &mut fixture.app;
            app.action(NAV_KEYS);
            app.action(KEY_ENABLE);
            app.action(tab);
            let editor = if tab == NAV_MAIN { INTERVAL } else { DELAY };
            set_text(app.control(editor), "pending edit");
            let mut expected = app.settings.clone();
            expected.key_clicks.enabled = false;
            let current_tab = app.tab;
            app.handle_key_error(ErrorCode::InputSend.into());
            app.update_stats();
            assert_eq!(app.tab, current_tab);
            assert_eq!(text(app.control(editor)), "pending edit");
            assert!(!app.settings.key_clicks.enabled);
            assert!(text(app.control(MESSAGE)).contains("キー操作を停止"));
            let (loaded, _) = Settings::load(&app.path).unwrap();
            assert_eq!(
                serde_json::to_value(&loaded).unwrap(),
                serde_json::to_value(&expected).unwrap()
            );
        }
    }
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
                lock_hotkey_ready: true,
                wheel_remainder: 0,
                main_offset: 0,
                selecting_area: false,
                key_controller: None,
                key_target: None,
                key_pick: None,
                profile_draft_name: None,
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
        assert_eq!(a.settings.fast_interval_ticks, 30_000);
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
        set_text(a.control(FAST_INTERVAL), "0.0000001");
        assert!(a.commit());
        assert_eq!(a.settings.fast_interval_ticks, 1);
        let (back, _) = Settings::load(&a.path).unwrap();
        assert_eq!(back.fast_interval_ticks, 1);
        assert_eq!(text(a.control(FAST_INTERVAL)), "0.0000001");
        let current = a.settings.area;
        set_text(a.control(AREA_TOP), "bad");
        assert!(!a.commit());
        assert_eq!(a.settings.area, current);
        unsafe {
            SendMessageW(a.control(MODE), CB_SETCURSEL, 3, 0);
        }
        a.action(MODE);
        assert_eq!(a.settings.mode, ClickMode::Pointer);
        assert_eq!(
            unsafe { SendMessageW(a.control(MODE), CB_GETCURSEL, 0, 0) },
            2
        );
        assert_eq!(text(a.control(AREA_TOP)), "bad");
    }
    #[test]
    fn lock_key_conflict_does_not_disable_start_and_stop() {
        let mut f = TestApp::new();
        let a = &mut f.app;
        let mut ids = vec![];
        assert!(
            a.register_hotkeys_using(|id, _, _| {
                ids.push(id);
                id != LOCK_HOTKEY
            })
            .is_ok()
        );
        assert_eq!(ids, vec![5, 1, 2, 3, 4]);
        assert!(a.hotkeys_ready);
        assert!(!a.lock_hotkey_ready);
        a.rebuild();
        assert_ne!(unsafe { IsWindowEnabled(a.control(START)) }, 0);
        assert!(text(a.control(LOCK)).contains("ボタン"));
    }
    #[test]
    fn pending_point_intervals_survive_new_mode_switches() {
        let mut f = TestApp::new();
        let a = &mut f.app;
        a.settings.points = vec![Point {
            x: 1,
            y: 2,
            interval_ticks: None,
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
        assert_eq!(a.settings.points[0].interval_ticks, Some(110_000));
        assert_eq!(a.settings.interval_ticks, model::DEFAULT_INTERVAL_TICKS);
        assert_eq!(a.settings.fast_interval_ticks, model::TICKS_PER_MILLISECOND);
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
        assert!(follows_common_interval(None, "0.0000001", 1));
        assert!(!follows_common_interval(None, "0.011", 1));
        assert!(!follows_common_interval(Some(1), "0.0000001", 1));
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
        assert!(english.contains("0.0000001"));
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
        assert_eq!(parse_hotkey("alt+vk32"), Some((MOD_ALT, VK_SPACE as u32)));
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
