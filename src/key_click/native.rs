use super::*;
use std::{
    cell::RefCell,
    mem::{size_of, zeroed},
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    panic::{AssertUnwindSafe, catch_unwind},
    ptr::{null, null_mut},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::{
        Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute},
        Gdi::{ClientToScreen, MONITOR_DEFAULTTONULL, MonitorFromPoint, ScreenToClient},
    },
    System::{LibraryLoader::GetModuleHandleW, Threading::*},
    UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

mod controls;
mod injection;
use controls::{ActiveSettings, RouteContext, SourceKeyState};
use injection::{NativeSink, PhysicalMouse, Pixel, Runtime, Sink};

fn error(code: ErrorCode) -> ClickerError {
    ClickerError::with_detail(code, std::io::Error::last_os_error().to_string())
}
const RESEED: u32 = WM_APP + 41;
fn caption(hwnd: HWND) -> String {
    let mut text = [0u16; 513];
    let n = unsafe { GetWindowTextW(hwnd, text.as_mut_ptr(), text.len() as i32) };
    String::from_utf16_lossy(&text[..n.max(0) as usize])
}

/// Cached geometry only: never send WM_NCHITTEST to a UI thread being joined.
pub fn window_at(point: POINT) -> Option<HWND> {
    unsafe {
        let mut window = GetTopWindow(null_mut());
        for _ in 0..512 {
            if window.is_null() {
                return None;
            }
            let mut cloaked = 0u32;
            DwmGetWindowAttribute(
                window,
                DWMWA_CLOAKED as u32,
                (&mut cloaked as *mut u32).cast(),
                size_of::<u32>() as u32,
            );
            if IsWindowVisible(window) != 0
                && IsIconic(window) == 0
                && cloaked == 0
                && GetWindowLongPtrW(window, GWL_EXSTYLE) & WS_EX_TRANSPARENT as isize == 0
            {
                let mut rect: RECT = zeroed();
                if GetWindowRect(window, &mut rect) != 0
                    && point.x >= rect.left
                    && point.x < rect.right
                    && point.y >= rect.top
                    && point.y < rect.bottom
                {
                    return Some(GetAncestor(window, GA_ROOT));
                }
            }
            window = GetWindow(window, GW_HWNDNEXT);
        }
        None
    }
}

pub struct Target {
    window: usize,
    pid: u32,
    process: OwnedHandle,
    pub identity: GameIdentity,
    pin_caption: bool,
}
impl Target {
    pub fn capture_at(point: POINT) -> Result<Arc<Self>, ClickerError> {
        let hwnd = window_at(point).ok_or(ErrorCode::KeyTarget)?;
        unsafe {
            let mut pid = 0;
            GetWindowThreadProcessId(hwnd, &mut pid);
            if pid == 0 || pid == std::process::id() {
                return Err(ErrorCode::KeyTarget.into());
            }
            let raw = OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                pid,
            );
            if raw.is_null() {
                return Err(error(ErrorCode::KeyTarget));
            }
            let process = OwnedHandle::from_raw_handle(raw);
            let mut path = vec![0u16; 32768];
            let mut length = path.len() as u32;
            if QueryFullProcessImageNameW(raw, 0, path.as_mut_ptr(), &mut length) == 0 {
                return Err(error(ErrorCode::KeyTarget));
            }
            let mut class = [0u16; 257];
            let n = GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32);
            if n <= 0 {
                return Err(error(ErrorCode::KeyTarget));
            }
            let executable = String::from_utf16_lossy(&path[..length as usize]);
            let name = executable
                .rsplit(['\\', '/'])
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            let pin_caption = matches!(
                name.as_str(),
                "chrome.exe"
                    | "msedge.exe"
                    | "firefox.exe"
                    | "brave.exe"
                    | "opera.exe"
                    | "vivaldi.exe"
            );
            let target = Self {
                window: hwnd as usize,
                pid,
                process,
                identity: GameIdentity {
                    executable,
                    class: String::from_utf16_lossy(&class[..n as usize]),
                    title: caption(hwnd),
                },
                pin_caption,
            };
            target.client_size()?;
            Ok(Arc::new(target))
        }
    }
    pub fn alive(&self) -> bool {
        let hwnd = self.window as HWND;
        let mut pid = 0;
        unsafe {
            IsWindow(hwnd) != 0
                && GetWindowThreadProcessId(hwnd, &mut pid) != 0
                && pid == self.pid
                && WaitForSingleObject(self.process.as_raw_handle(), 0) == WAIT_TIMEOUT
        }
    }
    pub fn foreground(&self) -> bool {
        // Native game titles can change with levels/FPS; browser tabs need a caption pin.
        (unsafe { GetForegroundWindow() == self.window as HWND }) && self.context_matches()
    }
    pub fn context_matches(&self) -> bool {
        if !self.alive() {
            return false;
        }
        let mut class = [0u16; 257];
        let count =
            unsafe { GetClassNameW(self.window as HWND, class.as_mut_ptr(), class.len() as i32) };
        count > 0
            && String::from_utf16_lossy(&class[..count as usize]) == self.identity.class
            && (!self.pin_caption || caption(self.window as HWND) == self.identity.title)
    }
    pub fn client_size(&self) -> Result<(i32, i32), ClickerError> {
        let mut rect: RECT = unsafe { zeroed() };
        if !self.alive()
            || unsafe { GetClientRect(self.window as HWND, &mut rect) } == 0
            || rect.right <= 0
            || rect.bottom <= 0
        {
            return Err(ErrorCode::KeyTarget.into());
        }
        Ok((rect.right, rect.bottom))
    }
    pub fn record_point(&self, point: POINT) -> Result<ClientPoint, ClickerError> {
        if !self.context_matches() || window_at(point) != Some(self.window as HWND) {
            return Err(ErrorCode::KeyTarget.into());
        }
        let (width, height) = self.client_size()?;
        let mut client = point;
        if unsafe { ScreenToClient(self.window as HWND, &mut client) } == 0 {
            return Err(error(ErrorCode::KeyTarget));
        }
        let result = ClientPoint {
            game: self.identity.clone(),
            x: client.x,
            y: client.y,
            width,
            height,
        };
        if !result.fits(&self.identity, width, height) {
            return Err(ErrorCode::KeyTarget.into());
        }
        Ok(result)
    }
    fn screen_point(&self, point: &ClientPoint) -> Result<POINT, ClickerError> {
        let (width, height) = self.client_size()?;
        if !point.fits(&self.identity, width, height) {
            return Err(ErrorCode::ScreenChanged.into());
        }
        let mut screen = POINT {
            x: point.x,
            y: point.y,
        };
        if unsafe { ClientToScreen(self.window as HWND, &mut screen) } == 0 {
            return Err(error(ErrorCode::KeyTarget));
        }
        if unsafe { MonitorFromPoint(screen, MONITOR_DEFAULTTONULL) }.is_null() {
            return Err(ErrorCode::PointOutside.into());
        }
        if !self.foreground() || window_at(screen) != Some(self.window as HWND) {
            return Err(ErrorCode::KeyTarget.into());
        }
        Ok(screen)
    }
}

struct Snapshot {
    target: Option<Arc<Target>>,
    source: ActiveSettings,
    settings: ActiveSettings,
    size: Option<(i32, i32)>,
}
impl Default for Snapshot {
    fn default() -> Self {
        let settings = ActiveSettings::from(&KeyClickSettings::default());
        Self {
            target: None,
            source: settings.clone(),
            settings,
            size: None,
        }
    }
}
struct Shared<S: Sink = NativeSink> {
    snapshot: RwLock<Arc<Snapshot>>,
    source: Mutex<SourceKeyState>,
    active: AtomicBool,
    desired: AtomicBool,
    stopping: AtomicBool,
    generation: AtomicU64,
    clicks: AtomicU64,
    error: Mutex<Option<ClickerError>>,
    warning: Mutex<Option<ClickerError>>,
    output: Mutex<Runtime<S>>,
    wake: Mutex<Option<thread::Thread>>,
    physical: Arc<Mutex<PhysicalMouse>>,
}
impl<S: Sink> Shared<S> {
    fn new(sink: S, physical: Arc<Mutex<PhysicalMouse>>) -> Self {
        Self {
            snapshot: RwLock::new(Arc::new(Snapshot::default())),
            source: Mutex::new(SourceKeyState::default()),
            active: AtomicBool::new(false),
            desired: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            clicks: AtomicU64::new(0),
            error: Mutex::new(None),
            warning: Mutex::new(None),
            output: Mutex::new(Runtime::new(sink)),
            wake: Mutex::new(None),
            physical,
        }
    }
    fn notify(&self) {
        if let Some(worker) = self
            .wake
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            worker.unpark();
        }
    }
    fn report(&self, error: ClickerError) {
        *self
            .error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error);
    }
    fn current(&self, generation: u64, snapshot: &Arc<Snapshot>) -> bool {
        self.active.load(Ordering::Acquire)
            && self.desired.load(Ordering::Acquire)
            && !self.stopping.load(Ordering::Acquire)
            && self.generation.load(Ordering::Acquire) == generation
            && self
                .snapshot
                .read()
                .is_ok_and(|current| Arc::ptr_eq(&current, snapshot))
    }
    fn drain(&self) -> bool {
        let mut output = self
            .output
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let source = self
            .source
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .snapshot();
        let result = output.halt(&source);
        if let Some(warning) = output.warning.take() {
            *self
                .warning
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(warning);
        }
        if let Err(error) = result {
            self.report(error);
            false
        } else {
            true
        }
    }
    fn pause(&self) -> bool {
        self.active.store(false, Ordering::Release);
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.source
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .suspend();
        self.notify();
        // Shares the worker's output lock and releases its ownership journal.
        // Returning from pause therefore also drains an in-flight DOWN/pulse.
        self.drain()
    }
    fn process(
        &self,
        snapshot: &Arc<Snapshot>,
        now: Instant,
        foreground: impl Fn() -> bool,
        resolve: impl FnMut(&ClientPoint) -> Result<Pixel, ClickerError>,
    ) {
        let generation = self.generation.load(Ordering::Acquire);
        let mut output = self
            .output
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !self.current(generation, snapshot)
            && self.active.load(Ordering::Acquire)
            && self.desired.load(Ordering::Acquire)
            && !self.stopping.load(Ordering::Acquire)
        {
            // A frame captured before reconfiguration must not disarm input
            // already accepted by the newly armed mapping.
            return;
        }
        let before = output.completed;
        let result = catch_unwind(AssertUnwindSafe(|| {
            let ready = self.current(generation, snapshot) && foreground();
            let mut source = self
                .source
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !ready {
                source.suspend();
            }
            let state = source.snapshot();
            drop(source);
            output.frame(&snapshot.settings, &state, now, resolve, || {
                let context_ready = self.current(generation, snapshot) && foreground();
                let mut source = self
                    .source
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let allowed = context_ready && !source.shortcut_active();
                if !allowed {
                    source.suspend();
                }
                allowed
            })
        }))
        .unwrap_or_else(|_| Err(ErrorCode::WorkerPanic.into()));
        self.clicks
            .fetch_add(output.completed - before, Ordering::Relaxed);
        if let Some(warning) = output.warning.take() {
            *self
                .warning
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(warning);
        }
        if let Err(error) = result {
            let transient = matches!(
                &error.code,
                ErrorCode::KeyTarget | ErrorCode::PointOutside | ErrorCode::ScreenChanged
            );
            if !transient {
                self.active.store(false, Ordering::Release);
                self.generation.fetch_add(1, Ordering::AcqRel);
            }
            let mut source = self
                .source
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            source.suspend();
            let state = source.snapshot();
            drop(source);
            let failure = if let Err(cleanup) = output.halt(&state) {
                self.active.store(false, Ordering::Release);
                self.generation.fetch_add(1, Ordering::AcqRel);
                let detail = format!("{error}; cleanup: {cleanup}");
                ClickerError::with_detail(cleanup.code, detail)
            } else {
                error
            };
            self.report(failure);
        }
    }
}
struct Context {
    shared: Arc<Shared>,
}
thread_local! { static CONTEXT: RefCell<Option<Context>> = const { RefCell::new(None) }; }

struct Hooks {
    keyboard: HHOOK,
    mouse: HHOOK,
    shared: Arc<Shared>,
}
impl Drop for Hooks {
    fn drop(&mut self) {
        self.shared.active.store(false, Ordering::Release);
        self.shared.stopping.store(true, Ordering::Release);
        self.shared.generation.fetch_add(1, Ordering::AcqRel);
        self.shared
            .source
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .suspend();
        unsafe {
            if !self.mouse.is_null() {
                UnhookWindowsHookEx(self.mouse);
            }
            if !self.keyboard.is_null() {
                UnhookWindowsHookEx(self.keyboard);
            }
        }
        let _ = CONTEXT.try_with(|slot| {
            if let Ok(mut slot) = slot.try_borrow_mut() {
                slot.take();
            }
        });
        // Never join/drain from the hook thread: SendInput may still be waiting
        // for that thread's callback. Unhook first, then wake the output owner.
        self.shared.notify();
    }
}

unsafe extern "system" fn keyboard(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0
        && matches!(
            wp as u32,
            WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP
        )
    {
        let event = unsafe { &*(lp as *const KBDLLHOOKSTRUCT) };
        if event.flags & LLKHF_INJECTED == 0 {
            let suppress = CONTEXT.with(|slot| {
                let Ok(slot) = slot.try_borrow() else {
                    return false;
                };
                let Some(context) = slot.as_ref() else {
                    return false;
                };
                let shared = &context.shared;
                match catch_unwind(AssertUnwindSafe(|| {
                    let snapshot = shared
                        .snapshot
                        .read()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone();
                    let ready = shared.active.load(Ordering::Acquire)
                        && shared.desired.load(Ordering::Acquire)
                        && !shared.stopping.load(Ordering::Acquire)
                        && snapshot
                            .target
                            .as_ref()
                            .is_some_and(|target| target.foreground());
                    let mut source = shared
                        .source
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let suppress = source.route(
                        &snapshot.settings,
                        event.vkCode,
                        matches!(wp as u32, WM_KEYDOWN | WM_SYSKEYDOWN),
                        false,
                        RouteContext {
                            ready,
                            alt: event.flags & LLKHF_ALTDOWN != 0,
                        },
                        Instant::now(),
                    );
                    drop(source);
                    shared.notify();
                    suppress
                })) {
                    Ok(suppress) => suppress,
                    Err(_) => {
                        shared.active.store(false, Ordering::Release);
                        shared.generation.fetch_add(1, Ordering::AcqRel);
                        shared.report(ErrorCode::WorkerPanic.into());
                        shared.notify();
                        false
                    }
                }
            });
            if suppress {
                return 1;
            }
        }
    }
    unsafe { CallNextHookEx(null_mut(), code, wp, lp) }
}
unsafe extern "system" fn mouse(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 {
        let event = unsafe { &*(lp as *const MSLLHOOKSTRUCT) };
        // Promoted touch input is not a physical takeover of Mouse mode.
        let from_touch = event.dwExtraInfo & 0xffff_ff00 == 0xff51_5700;
        if !from_touch && event.dwExtraInfo != crate::engine::KEY_CLICK_TAG {
            CONTEXT.with(|slot| {
                if let Ok(slot) = slot.try_borrow() {
                    if let Some(context) = slot.as_ref() {
                        let mut physical = context
                            .shared
                            .physical
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        physical.activity = physical.activity.wrapping_add(1);
                        match wp as u32 {
                            WM_LBUTTONDOWN => physical.left = true,
                            WM_LBUTTONUP => physical.left = false,
                            WM_RBUTTONDOWN => physical.right = true,
                            WM_RBUTTONUP => physical.right = false,
                            _ => {}
                        }
                        drop(physical);
                        context.shared.notify();
                    }
                }
            });
        }
    }
    unsafe { CallNextHookEx(null_mut(), code, wp, lp) }
}

pub struct Controller {
    shared: Arc<Shared>,
    hook: Option<JoinHandle<()>>,
    worker: Option<JoinHandle<()>>,
    hook_id: u32,
}
impl Controller {
    pub fn start() -> Result<Self, ClickerError> {
        let physical = Arc::new(Mutex::new(PhysicalMouse::default()));
        let shared = Arc::new(Shared::new(NativeSink::new(physical.clone()), physical));
        let worker_shared = shared.clone();
        let worker = thread::Builder::new()
            .name("key-click-input".into())
            .spawn(move || {
                let result = catch_unwind(AssertUnwindSafe(|| {
                    while !worker_shared.stopping.load(Ordering::Acquire) {
                        let snapshot = worker_shared
                            .snapshot
                            .read()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .clone();
                        worker_shared.process(
                            &snapshot,
                            Instant::now(),
                            || {
                                snapshot
                                    .target
                                    .as_ref()
                                    .is_some_and(|target| target.foreground())
                            },
                            |point| {
                                snapshot
                                    .target
                                    .as_ref()
                                    .ok_or_else(|| ClickerError::from(ErrorCode::KeyTarget))?
                                    .screen_point(point)
                                    .map(Pixel::from)
                            },
                        );
                        thread::park_timeout(Duration::from_millis(8));
                    }
                }));
                if result.is_err() {
                    worker_shared.active.store(false, Ordering::Release);
                    worker_shared.report(ErrorCode::WorkerPanic.into());
                }
                worker_shared.drain();
            })
            .map_err(|error| {
                ClickerError::with_detail(ErrorCode::ThreadStart, error.to_string())
            })?;
        *shared
            .wake
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(worker.thread().clone());
        let hook_shared = shared.clone();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let hook_result = thread::Builder::new()
            .name("key-click-keyboard".into())
            .spawn(move || {
                let result = catch_unwind(AssertUnwindSafe(|| unsafe {
                    let mut message: MSG = zeroed();
                    PeekMessageW(&mut message, null_mut(), 0, 0, PM_NOREMOVE);
                    hook_shared
                        .source
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .seed_down((0..256).filter(|key| GetAsyncKeyState(*key as i32) < 0));
                    {
                        let mut physical = hook_shared
                            .physical
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        physical.left = GetAsyncKeyState(VK_LBUTTON as i32) < 0;
                        physical.right = GetAsyncKeyState(VK_RBUTTON as i32) < 0;
                    }
                    CONTEXT.with(|slot| {
                        *slot.borrow_mut() = Some(Context {
                            shared: hook_shared.clone(),
                        })
                    });
                    let keyboard_hook = SetWindowsHookExW(
                        WH_KEYBOARD_LL,
                        Some(keyboard),
                        GetModuleHandleW(null()),
                        0,
                    );
                    let mut hooks = Hooks {
                        keyboard: keyboard_hook,
                        mouse: null_mut(),
                        shared: hook_shared.clone(),
                    };
                    hooks.mouse = if keyboard_hook.is_null() {
                        null_mut()
                    } else {
                        SetWindowsHookExW(WH_MOUSE_LL, Some(mouse), GetModuleHandleW(null()), 0)
                    };
                    if keyboard_hook.is_null() || hooks.mouse.is_null() {
                        let failure = error(ErrorCode::InputMonitor);
                        let _ = ready_tx.send(Err(failure));
                        return;
                    }
                    let _ = ready_tx.send(Ok(GetCurrentThreadId()));
                    loop {
                        let result = GetMessageW(&mut message, null_mut(), 0, 0);
                        if result <= 0 {
                            if result < 0 {
                                hook_shared.report(error(ErrorCode::InputMonitor));
                            }
                            break;
                        }
                        if message.message == RESEED {
                            if message.wParam as u64
                                == hook_shared.generation.load(Ordering::Acquire)
                                && hook_shared.desired.load(Ordering::Acquire)
                                && !hook_shared.stopping.load(Ordering::Acquire)
                            {
                                hook_shared
                                    .source
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                                    .seed_down(
                                        (0..256).filter(|key| GetAsyncKeyState(*key as i32) < 0),
                                    );
                                hook_shared.active.store(true, Ordering::Release);
                                hook_shared.notify();
                            }
                            continue;
                        }
                        TranslateMessage(&message);
                        DispatchMessageW(&message);
                    }
                }));
                if result.is_err() {
                    hook_shared.active.store(false, Ordering::Release);
                    hook_shared.stopping.store(true, Ordering::Release);
                    hook_shared.report(ErrorCode::WorkerPanic.into());
                    hook_shared.notify();
                }
            });
        let hook = match hook_result {
            Ok(hook) => hook,
            Err(error) => {
                shared.stopping.store(true, Ordering::Release);
                shared.notify();
                let _ = worker.join();
                return Err(ClickerError::with_detail(
                    ErrorCode::ThreadStart,
                    error.to_string(),
                ));
            }
        };
        let hook_id = match ready_rx.recv() {
            Ok(Ok(id)) => id,
            result => {
                let _ = hook.join();
                shared.stopping.store(true, Ordering::Release);
                shared.notify();
                let _ = worker.join();
                return Err(result
                    .ok()
                    .and_then(Result::err)
                    .or_else(|| {
                        shared
                            .error
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .take()
                    })
                    .unwrap_or_else(|| ErrorCode::InputMonitor.into()));
            }
        };
        Ok(Self {
            shared,
            hook: Some(hook),
            worker: Some(worker),
            hook_id,
        })
    }
    fn reseed(&self) {
        if unsafe {
            PostThreadMessageW(
                self.hook_id,
                RESEED,
                self.shared.generation.load(Ordering::Acquire) as usize,
                0,
            )
        } == 0
        {
            self.shared.report(error(ErrorCode::InputMonitor));
        }
    }
    pub fn configure(&mut self, target: Option<Arc<Target>>, settings: &KeyClickSettings) {
        self.configure_with_hotkeys(target, settings, &[]);
    }
    pub fn configure_with_hotkeys(
        &mut self,
        target: Option<Arc<Target>>,
        settings: &KeyClickSettings,
        hotkeys: &[(u32, u32)],
    ) {
        let size = target.as_ref().and_then(|target| target.client_size().ok());
        let mut source = ActiveSettings::from(settings);
        source.operation_hotkeys = hotkeys
            .iter()
            .map(|(mods, key)| (*mods & 0x0f, *key))
            .collect();
        if self.shared.snapshot.read().is_ok_and(|old| {
            old.source == source
                && old.size == size
                && match (&old.target, &target) {
                    (None, None) => true,
                    (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                    _ => false,
                }
        }) {
            return;
        }
        let desired = self.shared.desired.swap(false, Ordering::AcqRel);
        let drained = self.shared.pause();
        let mut active = source.clone();
        let fits = |point: &ClientPoint| {
            target
                .as_ref()
                .zip(size)
                .is_some_and(|(target, (width, height))| {
                    point.fits(&target.identity, width, height)
                })
        };
        for binding in &mut active.bindings {
            if !binding.position.as_ref().is_some_and(fits) {
                binding.position = None;
            }
        }
        let stick_valid = active.stick.center.as_ref().is_some_and(|center| {
            let radius = i64::from(active.stick.radius_px);
            fits(center)
                && (4..=2048).contains(&active.stick.radius_px)
                && i64::from(center.x) - radius >= 0
                && i64::from(center.y) - radius >= 0
                && i64::from(center.x) + radius < i64::from(center.width)
                && i64::from(center.y) + radius < i64::from(center.height)
        });
        if !stick_valid {
            active.stick.center = None;
        }
        let warning = if active.input_method == InputMethod::Touch
            && active
                .bindings
                .iter()
                .any(|binding| binding.right && binding.position.is_some())
        {
            Some(ClickerError::with_detail(
                ErrorCode::InputRightButton,
                "Touch cannot send right-button actions; select Mouse for this setting.",
            ))
        } else {
            None
        };
        *self
            .shared
            .warning
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = warning;
        if drained
            && source.movement_mode == MovementMode::Stick
            && source.stick.center.is_some()
            && !stick_valid
        {
            self.shared
                .error
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get_or_insert_with(|| ErrorCode::StickRange.into());
        }
        *self
            .shared
            .snapshot
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(Snapshot {
            target,
            source,
            settings: active,
            size,
        });
        self.shared.desired.store(desired, Ordering::Release);
        if desired && drained {
            self.reseed();
        }
    }
    pub fn set_active(&self, active: bool) {
        if self.shared.stopping.load(Ordering::Acquire) {
            return;
        }
        if self.shared.desired.swap(active, Ordering::AcqRel) != active {
            if active {
                self.reseed();
            } else {
                self.shared.pause();
            }
        }
    }
    pub fn clicks(&self) -> u64 {
        self.shared.clicks.load(Ordering::Relaxed)
    }
    pub fn take_error(&self) -> Option<ClickerError> {
        self.shared
            .error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
    pub fn warning(&self) -> Option<ClickerError> {
        self.shared
            .warning
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    pub fn stop(&mut self) {
        self.shared.desired.store(false, Ordering::Release);
        self.shared.stopping.store(true, Ordering::Release);
        self.shared.pause();
        if let Some(hook) = self.hook.take() {
            if unsafe { PostThreadMessageW(self.hook_id, WM_QUIT, 0, 0) } == 0 {
                self.shared.report(error(ErrorCode::InputMonitor));
            }
            if hook.join().is_err() {
                self.shared.report(ErrorCode::WorkerPanic.into());
            }
        }
        self.shared.notify();
        if let Some(worker) = self.worker.take() {
            if worker.join().is_err() {
                self.shared.report(ErrorCode::WorkerPanic.into());
            }
        }
        self.shared.drain();
    }
}
impl Drop for Controller {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests;
