//! Physical drag state is independent of injected click events.
use crate::error::{ClickerError, ErrorCode};
use std::{
    cell::RefCell,
    ptr::{null, null_mut},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
};
use windows_sys::Win32::{
    Foundation::*,
    System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
    UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};
const REPLAY_TAP: u32 = WM_APP + 20;

#[derive(Default)]
pub(crate) struct Gesture {
    origin: Option<(i32, i32)>,
    pub active: bool,
}
impl Gesture {
    fn ends_tap(&self, message: u32) -> bool {
        message == WM_LBUTTONUP && self.held() && !self.active
    }
    fn event(&mut self, message: u32, x: i32, y: i32, injected: bool, excluded: bool) -> bool {
        if injected {
            return false;
        }
        match message {
            WM_LBUTTONDOWN if !excluded => {
                self.down(x, y);
                true
            }
            WM_LBUTTONUP => {
                let held = self.held();
                self.up();
                held
            }
            WM_MOUSEMOVE => {
                self.motion(x, y);
                false
            }
            _ => false,
        }
    }
    pub fn down(&mut self, x: i32, y: i32) {
        self.origin = Some((x, y));
        self.active = false;
    }
    pub fn motion(&mut self, x: i32, y: i32) {
        if let Some((sx, sy)) = self.origin {
            if (i64::from(x) - i64::from(sx)).abs() >= 3
                || (i64::from(y) - i64::from(sy)).abs() >= 3
            {
                self.active = true;
            }
        }
    }
    pub fn up(&mut self) {
        self.origin = None;
        self.active = false;
    }
    pub fn held(&self) -> bool {
        self.origin.is_some()
    }
}
pub struct DragState {
    pub active: AtomicBool,
    pub locked: AtomicBool,
    stopping: AtomicBool,
    pub input_failed: AtomicBool,
}
impl DragState {
    pub fn idle() -> Self {
        Self {
            active: AtomicBool::new(false),
            locked: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            input_failed: AtomicBool::new(false),
        }
    }
    pub fn engaged(&self) -> bool {
        !self.stopping.load(Ordering::Acquire)
            && (self.active.load(Ordering::Acquire) || self.locked.load(Ordering::Acquire))
    }
    pub fn cancel(&self) {
        self.stopping.store(true, Ordering::Release);
        self.active.store(false, Ordering::Release);
        self.locked.store(false, Ordering::Release);
    }
}
struct Context {
    state: Arc<DragState>,
    gesture: Gesture,
    excluded_pid: u32,
}
thread_local! {static CONTEXT:RefCell<Option<Context>>=const {RefCell::new(None)};}
/// Inspect cached top-level geometry in Z order. WindowFromPoint sends
/// WM_NCHITTEST to other UI threads and must never be called by the hook or
/// click worker: the UI thread can be joining those threads during Stop.
pub(crate) fn point_hits_process(point: POINT, process: u32) -> bool {
    unsafe {
        let mut window = GetTopWindow(null_mut());
        for _ in 0..512 {
            if window.is_null() {
                return false;
            }
            if IsWindowVisible(window) != 0
                && IsIconic(window) == 0
                && GetWindowLongPtrW(window, GWL_EXSTYLE) & WS_EX_TRANSPARENT as isize == 0
            {
                let mut rect: RECT = std::mem::zeroed();
                if GetWindowRect(window, &mut rect) != 0
                    && point.x >= rect.left
                    && point.x < rect.right
                    && point.y >= rect.top
                    && point.y < rect.bottom
                {
                    let mut pid = 0;
                    GetWindowThreadProcessId(window, &mut pid);
                    return pid == process;
                }
            }
            window = GetWindow(window, GW_HWNDNEXT);
        }
        false
    }
}
unsafe extern "system" fn hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 {
        // The hook pointer is owned by Windows and valid only during this callback.
        let event = unsafe { &*(lp as *const MSLLHOOKSTRUCT) };
        if event.flags & LLMHF_INJECTED == 0 {
            let suppress = CONTEXT.with(|slot| {
                let Ok(mut ctx) = slot.try_borrow_mut() else {
                    return false;
                };
                let Some(ctx) = ctx.as_mut() else {
                    return false;
                };
                if ctx.state.stopping.load(Ordering::Acquire) {
                    return false;
                }
                let tap =
                    ctx.gesture.ends_tap(wp as u32) && !ctx.state.locked.load(Ordering::Acquire);
                // Geometry is needed only for the start of a physical press.
                let excluded =
                    wp as u32 == WM_LBUTTONDOWN && point_hits_process(event.pt, ctx.excluded_pid);
                let suppress = ctx
                    .gesture
                    .event(wp as u32, event.pt.x, event.pt.y, false, excluded);
                ctx.state
                    .active
                    .store(ctx.gesture.active, Ordering::Release);
                if tap {
                    unsafe {
                        PostThreadMessageW(
                            GetCurrentThreadId(),
                            REPLAY_TAP,
                            event.pt.x as usize,
                            event.pt.y as isize,
                        );
                    }
                }
                suppress
            });
            if suppress {
                return 1;
            }
        }
    }
    unsafe { CallNextHookEx(null_mut(), code, wp, lp) }
}

pub struct DragMonitor {
    pub state: Arc<DragState>,
    thread: Option<JoinHandle<()>>,
    thread_id: u32,
}
impl DragMonitor {
    pub fn start(excluded_pid: u32) -> Result<Self, ClickerError> {
        let state = Arc::new(DragState::idle());
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let copy = state.clone();
        let thread = thread::Builder::new()
            .name("physical-drag".into())
            .spawn(move || unsafe {
                let mut msg: MSG = std::mem::zeroed();
                PeekMessageW(&mut msg, null_mut(), 0, 0, PM_NOREMOVE);
                CONTEXT.with(|ctx| {
                    *ctx.borrow_mut() = Some(Context {
                        state: copy.clone(),
                        gesture: Gesture::default(),
                        excluded_pid,
                    })
                });
                let h = SetWindowsHookExW(WH_MOUSE_LL, Some(hook), GetModuleHandleW(null()), 0);
                if h.is_null() {
                    let _ = sender.send(Err(ClickerError::with_detail(
                        ErrorCode::InputMonitor,
                        std::io::Error::last_os_error().to_string(),
                    )));
                    CONTEXT.with(|c| {
                        c.borrow_mut().take();
                    });
                    return;
                }
                let _ = sender.send(Ok(GetCurrentThreadId()));
                while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
                    if msg.message == REPLAY_TAP {
                        if !copy.stopping.load(Ordering::Acquire)
                            && !copy.locked.load(Ordering::Acquire)
                        {
                            let mut current: POINT = std::mem::zeroed();
                            // A tap is replayed only while the cursor is still at its release
                            // position. Never move it back or click an unrelated new location.
                            if GetCursorPos(&mut current) != 0
                                && current.x == msg.wParam as i32
                                && current.y == msg.lParam as i32
                                && !point_hits_process(current, excluded_pid)
                            {
                                let events = crate::engine::pointer_inputs(false);
                                if SendInput(
                                    2,
                                    events.as_ptr(),
                                    std::mem::size_of::<INPUT>() as i32,
                                ) != 2
                                {
                                    let up = events[1];
                                    SendInput(1, &up, std::mem::size_of::<INPUT>() as i32);
                                    copy.input_failed.store(true, Ordering::Release);
                                }
                            }
                        }
                        continue;
                    }
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                copy.stopping.store(true, Ordering::Release);
                copy.active.store(false, Ordering::Release);
                copy.locked.store(false, Ordering::Release);
                UnhookWindowsHookEx(h);
                CONTEXT.with(|c| {
                    c.borrow_mut().take();
                });
            })
            .map_err(|e| ClickerError::with_detail(ErrorCode::InputMonitor, e.to_string()))?;
        let id = match receiver.recv() {
            Ok(Ok(id)) => id,
            Ok(Err(e)) => {
                let _ = thread.join();
                return Err(e);
            }
            Err(_) => {
                let _ = thread.join();
                return Err(ErrorCode::InputMonitor.into());
            }
        };
        Ok(Self {
            state,
            thread: Some(thread),
            thread_id: id,
        })
    }
    pub fn stop(&mut self) {
        self.state.cancel();
        if let Some(thread) = self.thread.take() {
            unsafe {
                PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0);
            }
            let _ = thread.join();
        }
    }
}
impl Drop for DragMonitor {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn injected_clicks_and_own_window_presses_do_not_trigger_drag() {
        let mut g = Gesture::default();
        assert!(!g.event(WM_LBUTTONDOWN, 0, 0, true, false));
        g.event(WM_MOUSEMOVE, 9, 0, false, false);
        assert!(!g.active);
        assert!(!g.event(WM_LBUTTONDOWN, 0, 0, false, true));
        assert!(!g.held());
        assert!(g.event(WM_LBUTTONDOWN, 0, 0, false, false));
        g.event(WM_MOUSEMOVE, 6, 0, false, false);
        assert!(g.active);
        assert!(!g.event(WM_LBUTTONUP, 0, 0, true, false));
        assert!(g.active);
        assert!(g.event(WM_LBUTTONUP, 0, 0, false, false));
        assert!(!g.active);
        assert!(!g.held());
    }
    #[test]
    fn drag_requires_physical_hold_and_motion() {
        let mut g = Gesture::default();
        g.motion(99, 99);
        assert!(!g.active);
        g.down(10, 10);
        g.motion(12, 12);
        assert!(!g.active);
        g.motion(13, 10);
        assert!(g.active);
        g.up();
        assert!(!g.active);
        g.motion(50, 50);
        assert!(!g.active);
    }
    #[test]
    fn lock_is_separate_from_drag_and_never_starts_on_creation() {
        let state = DragState::idle();
        assert!(!state.engaged());
        state.locked.store(true, Ordering::Release);
        assert!(state.engaged());
        state.locked.store(false, Ordering::Release);
        assert!(!state.engaged());
        state.active.store(true, Ordering::Release);
        assert!(state.engaged());
    }
    #[test]
    fn ordinary_taps_are_distinct_from_drag_release() {
        let mut g = Gesture::default();
        assert!(!g.ends_tap(WM_LBUTTONUP));
        g.event(WM_LBUTTONDOWN, 10, 10, false, false);
        assert!(g.ends_tap(WM_LBUTTONUP));
        g.event(WM_LBUTTONUP, 10, 10, false, false);
        assert!(!g.ends_tap(WM_LBUTTONUP));
        g.event(WM_LBUTTONDOWN, 10, 10, false, false);
        g.event(WM_MOUSEMOVE, 30, 10, false, false);
        assert!(!g.ends_tap(WM_LBUTTONUP));
    }
}
