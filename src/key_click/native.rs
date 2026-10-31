use super::*;
use std::{
    cell::RefCell,
    mem::{size_of, zeroed},
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr::{null, null_mut},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, SyncSender},
    },
    thread::{self, JoinHandle},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::{
        Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute},
        Gdi::{ClientToScreen, ScreenToClient},
    },
    System::{LibraryLoader::GetModuleHandleW, Threading::*},
    UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

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
        if !self.foreground() || window_at(screen) != Some(self.window as HWND) {
            return Err(ErrorCode::KeyTarget.into());
        }
        Ok(screen)
    }
}

#[derive(Default)]
struct Snapshot {
    target: Option<Arc<Target>>,
    bindings: Vec<KeyBinding>,
    source_bindings: Vec<KeyBinding>,
    size: Option<(i32, i32)>,
}
struct Request {
    snapshot: Arc<Snapshot>,
    generation: u64,
    index: usize,
}
struct Shared {
    snapshot: RwLock<Arc<Snapshot>>,
    active: AtomicBool,
    desired: AtomicBool,
    stopping: AtomicBool,
    generation: AtomicU64,
    clicks: AtomicU64,
    error: Mutex<Option<ClickerError>>,
    output: Mutex<()>,
}
impl Shared {
    fn current(&self, request: &Request) -> bool {
        self.active.load(Ordering::Acquire)
            && self.desired.load(Ordering::Acquire)
            && !self.stopping.load(Ordering::Acquire)
            && self.generation.load(Ordering::Acquire) == request.generation
            && self
                .snapshot
                .read()
                .is_ok_and(|snapshot| Arc::ptr_eq(&snapshot, &request.snapshot))
    }
    fn report(&self, error: ClickerError) {
        if let Ok(mut slot) = self.error.lock() {
            *slot = Some(error);
        }
    }
    fn pause(&self) {
        self.active.store(false, Ordering::Release);
        self.generation.fetch_add(1, Ordering::AcqRel);
        drop(
            self.output
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
    }
}
struct Context {
    shared: Arc<Shared>,
    sender: SyncSender<Request>,
    keys: KeyState,
}
fn dispatch(
    shared: &Shared,
    request: &Request,
    resolve: impl FnOnce() -> Result<Option<(i32, i32, bool)>, ClickerError>,
    emit: impl FnOnce(i32, i32, bool) -> Result<bool, ClickerError>,
) -> Result<bool, ClickerError> {
    if !shared.current(request) {
        return Ok(false);
    }
    let _output = shared
        .output
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !shared.current(request) {
        return Ok(false);
    }
    let Some((x, y, right)) = resolve()? else {
        return Ok(false);
    };
    if !shared.current(request) {
        return Ok(false);
    }
    let clicked = emit(x, y, right)?;
    if clicked {
        shared.clicks.fetch_add(1, Ordering::Relaxed);
    }
    Ok(clicked)
}
thread_local! {static CONTEXT:RefCell<Option<Context>>=const {RefCell::new(None)};}
unsafe extern "system" fn keyboard(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0
        && matches!(
            wp as u32,
            WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP
        )
    {
        let event = unsafe { &*(lp as *const KBDLLHOOKSTRUCT) };
        let suppress = CONTEXT.with(|slot| {
            let Ok(mut slot) = slot.try_borrow_mut() else {
                return false;
            };
            let Some(context) = slot.as_mut() else {
                return false;
            };
            let shared = &context.shared;
            let generation = shared.generation.load(Ordering::Acquire);
            let snapshot = shared.snapshot.try_read().ok().map(|s| s.clone());
            let ready = shared.active.load(Ordering::Acquire)
                && shared.desired.load(Ordering::Acquire)
                && !shared.stopping.load(Ordering::Acquire);
            let binding = snapshot.as_ref().filter(|_| ready).and_then(|s| {
                s.target.as_ref().filter(|t| t.foreground()).and_then(|_| {
                    s.bindings
                        .iter()
                        .position(|b| b.key == Some(event.vkCode) && b.position.is_some())
                })
            });
            let modifiers = [VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN]
                .into_iter()
                .any(|key| unsafe { GetAsyncKeyState(key as i32) } < 0);
            let decision = context.keys.event(
                event.vkCode,
                matches!(wp as u32, WM_KEYDOWN | WM_SYSKEYDOWN),
                event.flags & LLKHF_INJECTED != 0,
                binding,
                modifiers,
            );
            if let Some(index) = decision.click {
                if let Some(snapshot) = snapshot {
                    let request = Request {
                        snapshot,
                        generation,
                        index,
                    };
                    if context.sender.try_send(request).is_err() {
                        shared.report(ErrorCode::InputSend.into());
                    }
                }
            }
            decision.suppress
        });
        if suppress {
            return 1;
        }
    }
    unsafe { CallNextHookEx(null_mut(), code, wp, lp) }
}

pub struct Controller {
    shared: Arc<Shared>,
    sender: Option<SyncSender<Request>>,
    hook: Option<JoinHandle<()>>,
    worker: Option<JoinHandle<()>>,
    hook_id: u32,
}
impl Controller {
    pub fn start() -> Result<Self, ClickerError> {
        let shared = Arc::new(Shared {
            snapshot: RwLock::new(Arc::new(Snapshot::default())),
            active: AtomicBool::new(false),
            desired: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            clicks: AtomicU64::new(0),
            error: Mutex::new(None),
            output: Mutex::new(()),
        });
        let (sender, receiver) = mpsc::sync_channel::<Request>(64);
        let worker_shared = shared.clone();
        let worker = thread::Builder::new()
            .name("key-click-input".into())
            .spawn(move || {
                while let Ok(request) = receiver.recv() {
                    let result = dispatch(
                        &worker_shared,
                        &request,
                        || {
                            let Some(target) = &request.snapshot.target else {
                                return Ok(None);
                            };
                            let Some(binding) = request.snapshot.bindings.get(request.index) else {
                                return Ok(None);
                            };
                            let Some(point) = &binding.position else {
                                return Ok(None);
                            };
                            let screen = target.screen_point(point)?;
                            Ok(Some((screen.x, screen.y, binding.right)))
                        },
                        |x, y, right| {
                            if !request
                                .snapshot
                                .target
                                .as_ref()
                                .is_some_and(|target| target.foreground())
                            {
                                return Ok(false);
                            }
                            crate::engine::click_once_restore(x, y, right)?;
                            Ok(true)
                        },
                    );
                    if let Err(error) = result {
                        worker_shared.report(error);
                    }
                }
            })
            .map_err(|e| ClickerError::with_detail(ErrorCode::ThreadStart, e.to_string()))?;
        let hook_shared = shared.clone();
        let hook_sender = sender.clone();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let hook_result = thread::Builder::new()
            .name("key-click-keyboard".into())
            .spawn(move || unsafe {
                let mut message: MSG = zeroed();
                PeekMessageW(&mut message, null_mut(), 0, 0, PM_NOREMOVE);
                let mut keys = KeyState::default();
                keys.seed_down((0..256).filter(|key| GetAsyncKeyState(*key as i32) < 0));
                CONTEXT.with(|slot| {
                    *slot.borrow_mut() = Some(Context {
                        shared: hook_shared.clone(),
                        sender: hook_sender,
                        keys,
                    })
                });
                let hook =
                    SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard), GetModuleHandleW(null()), 0);
                if hook.is_null() {
                    let _ = ready_tx.send(Err(error(ErrorCode::InputMonitor)));
                    CONTEXT.with(|slot| slot.borrow_mut().take());
                    return;
                }
                let _ = ready_tx.send(Ok(GetCurrentThreadId()));
                while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
                    if message.message == RESEED {
                        CONTEXT.with(|slot| {
                            if let Some(context) = slot.borrow_mut().as_mut() {
                                context.keys.seed_down(
                                    (0..256).filter(|key| GetAsyncKeyState(*key as i32) < 0),
                                );
                            }
                        });
                        if message.wParam as u64 == hook_shared.generation.load(Ordering::Acquire)
                            && hook_shared.desired.load(Ordering::Acquire)
                            && !hook_shared.stopping.load(Ordering::Acquire)
                        {
                            hook_shared.active.store(true, Ordering::Release);
                        }
                        continue;
                    }
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
                UnhookWindowsHookEx(hook);
                CONTEXT.with(|slot| slot.borrow_mut().take());
            });
        let hook = match hook_result {
            Ok(hook) => hook,
            Err(e) => {
                drop(sender);
                let _ = worker.join();
                return Err(ClickerError::with_detail(
                    ErrorCode::ThreadStart,
                    e.to_string(),
                ));
            }
        };
        let hook_id = match ready_rx.recv() {
            Ok(Ok(id)) => id,
            result => {
                let _ = hook.join();
                drop(sender);
                let _ = worker.join();
                return Err(result
                    .ok()
                    .and_then(Result::err)
                    .unwrap_or_else(|| ErrorCode::InputMonitor.into()));
            }
        };
        Ok(Self {
            shared,
            sender: Some(sender),
            hook: Some(hook),
            worker: Some(worker),
            hook_id,
        })
    }
    pub fn configure(&mut self, target: Option<Arc<Target>>, settings: &KeyClickSettings) {
        let size = target.as_ref().and_then(|target| target.client_size().ok());
        if self.shared.snapshot.read().is_ok_and(|old| {
            old.source_bindings == settings.bindings
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
        self.shared.pause();
        let mut bindings = settings.bindings.clone();
        for binding in &mut bindings {
            if !binding.position.as_ref().is_some_and(|point| {
                target
                    .as_ref()
                    .zip(size)
                    .is_some_and(|(target, (width, height))| {
                        point.fits(&target.identity, width, height)
                    })
            }) {
                binding.position = None;
            }
        }
        if let Ok(mut snapshot) = self.shared.snapshot.write() {
            *snapshot = Arc::new(Snapshot {
                target,
                bindings,
                source_bindings: settings.bindings.clone(),
                size,
            });
        }
        self.shared.generation.fetch_add(1, Ordering::AcqRel);
        self.shared.desired.store(desired, Ordering::Release);
        if let Ok(mut error) = self.shared.error.lock() {
            *error = None;
        }
        if self.shared.desired.load(Ordering::Acquire) {
            unsafe {
                PostThreadMessageW(
                    self.hook_id,
                    RESEED,
                    self.shared.generation.load(Ordering::Acquire) as usize,
                    0,
                );
            }
        }
    }
    pub fn set_active(&self, active: bool) {
        if self.shared.desired.swap(active, Ordering::AcqRel) != active {
            if active {
                unsafe {
                    PostThreadMessageW(
                        self.hook_id,
                        RESEED,
                        self.shared.generation.load(Ordering::Acquire) as usize,
                        0,
                    );
                }
            } else {
                self.shared.pause();
            }
        }
    }
    pub fn clicks(&self) -> u64 {
        self.shared.clicks.load(Ordering::Relaxed)
    }
    pub fn take_error(&self) -> Option<ClickerError> {
        self.shared.error.lock().ok().and_then(|mut e| e.take())
    }
    pub fn stop(&mut self) {
        self.set_active(false);
        self.shared.stopping.store(true, Ordering::Release);
        if let Some(hook) = self.hook.take() {
            unsafe {
                PostThreadMessageW(self.hook_id, WM_QUIT, 0, 0);
            }
            let _ = hook.join();
        }
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
impl Drop for Controller {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dispatch_clicks_only_the_current_enabled_request_and_resolved_input() {
        let snapshot = Arc::new(Snapshot::default());
        let shared = Shared {
            snapshot: RwLock::new(snapshot.clone()),
            active: AtomicBool::new(true),
            desired: AtomicBool::new(true),
            stopping: AtomicBool::new(false),
            generation: AtomicU64::new(1),
            clicks: AtomicU64::new(0),
            error: Mutex::new(None),
            output: Mutex::new(()),
        };
        let request = Request {
            snapshot: snapshot.clone(),
            generation: 1,
            index: 0,
        };
        let emitted = Mutex::new(Vec::new());
        let send = |x, y, right| {
            emitted.lock().unwrap().push((x, y, right));
            Ok(true)
        };
        assert!(dispatch(&shared, &request, || Ok(Some((120, 230, false))), send).unwrap());
        assert!(!dispatch(&shared, &request, || Ok(None), send).unwrap());
        shared.desired.store(false, Ordering::Release);
        shared.pause();
        assert!(
            !dispatch(
                &shared,
                &request,
                || panic!("stopped request reached resolver"),
                send
            )
            .unwrap()
        );
        shared.desired.store(true, Ordering::Release);
        shared.active.store(true, Ordering::Release);
        assert!(
            !dispatch(
                &shared,
                &request,
                || panic!("stale generation reached resolver"),
                send
            )
            .unwrap()
        );
        *shared.snapshot.write().unwrap() = Arc::new(Snapshot::default());
        let stale = Request {
            snapshot,
            generation: shared.generation.load(Ordering::Acquire),
            index: 0,
        };
        assert!(
            !dispatch(
                &shared,
                &stale,
                || panic!("obsolete mapping reached resolver"),
                send
            )
            .unwrap()
        );
        let current = Request {
            snapshot: shared.snapshot.read().unwrap().clone(),
            generation: shared.generation.load(Ordering::Acquire),
            index: 0,
        };
        assert!(dispatch(&shared, &current, || Ok(Some((340, 450, true))), send).unwrap());
        assert_eq!(
            *emitted.lock().unwrap(),
            [(120, 230, false), (340, 450, true)]
        );
        assert_eq!(shared.clicks.load(Ordering::Relaxed), 2);
    }
}
