use super::super::{ClickerError, ErrorCode, InputMethod, MAX_BINDINGS};
use super::{controls::*, error};
use std::{
    collections::{BTreeMap, BTreeSet},
    mem::{size_of, zeroed},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    System::LibraryLoader::{GetModuleHandleW, GetProcAddress},
    UI::{
        Input::{KeyboardAndMouse::*, Pointer::*},
        WindowsAndMessaging::*,
    },
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Pixel {
    pub x: i32,
    pub y: i32,
}
impl From<POINT> for Pixel {
    fn from(point: POINT) -> Self {
        Self {
            x: point.x,
            y: point.y,
        }
    }
}
impl Pixel {
    fn matches_injected(self, expected: Self) -> bool {
        // Absolute-coordinate quantization may land one pixel either side.
        // Physical activity is tracked separately and still always wins.
        self.x.abs_diff(expected.x) <= 1 && self.y.abs_diff(expected.y) <= 1
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Desktop {
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
}
impl Desktop {
    fn contains(self, point: Pixel) -> bool {
        let x = point.x as i64 - self.left as i64;
        let y = point.y as i64 - self.top as i64;
        x >= 0 && y >= 0 && x < self.width as i64 && y < self.height as i64
    }
}
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct PhysicalMouse {
    pub left: bool,
    pub right: bool,
    pub activity: u64,
}
#[derive(Clone, Copy, Debug)]
pub(super) struct MouseState {
    pub position: Pixel,
    pub physical: PhysicalMouse,
}

pub(super) struct MouseFailure {
    pub inserted: usize,
    pub error: ClickerError,
}
pub(super) struct TouchFailure {
    pub retry: bool,
    pub error: ClickerError,
}
/// Tests implement this boundary; production is the only implementation that
/// calls SendInput/InjectTouchInput. Packet construction is shared by both.
pub(super) trait Sink {
    fn desktop(&mut self) -> Result<Desktop, ClickerError>;
    fn mouse_state(&mut self) -> Result<MouseState, ClickerError>;
    fn mouse(&mut self, packets: &[INPUT]) -> Result<(), MouseFailure>;
    fn initialize_touch(&mut self) -> Result<(), ClickerError>;
    fn touch(&mut self, packets: &[POINTER_TOUCH_INFO]) -> Result<(), TouchFailure>;
    fn clock(&self, frame_time: Instant) -> Instant {
        frame_time
    }
    fn retry_pause(&mut self) {}
}

type InitializeTouch = unsafe extern "system" fn(u32, u32) -> i32;
type InjectTouch = unsafe extern "system" fn(u32, *const POINTER_TOUCH_INFO) -> i32;
#[derive(Clone, Copy)]
struct TouchApi {
    inject: InjectTouch,
}
impl TouchApi {
    fn shared() -> Result<Self, ClickerError> {
        static API: Mutex<Option<TouchApi>> = Mutex::new(None);
        let mut cached = API
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(api) = *cached {
            return Ok(api);
        }
        let api = Self::load()?;
        *cached = Some(api);
        Ok(api)
    }
    fn load() -> Result<Self, ClickerError> {
        // Resolve the optional entry points before recording any contact. Missing
        // exports must not prevent Mouse mode from starting or trigger stray UPs.
        let library_name: Vec<u16> = "user32.dll\0".encode_utf16().collect();
        let library = unsafe { GetModuleHandleW(library_name.as_ptr()) };
        if library.is_null() {
            return Err(error(ErrorCode::InputInitialize));
        }
        let initialize =
            unsafe { GetProcAddress(library, c"InitializeTouchInjection".as_ptr().cast()) }
                .ok_or_else(|| error(ErrorCode::InputInitialize))?;
        let inject = unsafe { GetProcAddress(library, c"InjectTouchInput".as_ptr().cast()) }
            .ok_or_else(|| error(ErrorCode::InputInitialize))?;
        let initialize = unsafe {
            std::mem::transmute::<unsafe extern "system" fn() -> isize, InitializeTouch>(initialize)
        };
        let inject = unsafe {
            std::mem::transmute::<unsafe extern "system" fn() -> isize, InjectTouch>(inject)
        };
        if unsafe { initialize(MAX_BINDINGS as u32 + 1, TOUCH_FEEDBACK_NONE) } == 0 {
            return Err(error(ErrorCode::InputInitialize));
        }
        Ok(Self { inject })
    }
}
pub(super) struct NativeSink {
    physical: Arc<Mutex<PhysicalMouse>>,
    touch: Option<TouchApi>,
}
impl NativeSink {
    pub fn new(physical: Arc<Mutex<PhysicalMouse>>) -> Self {
        Self {
            physical,
            touch: None,
        }
    }
}
impl Sink for NativeSink {
    fn desktop(&mut self) -> Result<Desktop, ClickerError> {
        let desktop = unsafe {
            Desktop {
                left: GetSystemMetrics(SM_XVIRTUALSCREEN),
                top: GetSystemMetrics(SM_YVIRTUALSCREEN),
                width: GetSystemMetrics(SM_CXVIRTUALSCREEN),
                height: GetSystemMetrics(SM_CYVIRTUALSCREEN),
            }
        };
        if desktop.width <= 0 || desktop.height <= 0 {
            return Err(ErrorCode::ScreenUnavailable.into());
        }
        Ok(desktop)
    }
    fn mouse_state(&mut self) -> Result<MouseState, ClickerError> {
        let physical = *self
            .physical
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut position: POINT = unsafe { zeroed() };
        if unsafe { GetCursorPos(&mut position) } == 0 {
            return Err(error(ErrorCode::InputSend));
        }
        Ok(MouseState {
            position: position.into(),
            physical,
        })
    }
    fn mouse(&mut self, packets: &[INPUT]) -> Result<(), MouseFailure> {
        let inserted = unsafe {
            SendInput(
                packets.len() as u32,
                packets.as_ptr(),
                size_of::<INPUT>() as i32,
            )
        } as usize;
        if inserted == packets.len() {
            Ok(())
        } else {
            Err(MouseFailure {
                inserted,
                error: error(ErrorCode::InputSend),
            })
        }
    }
    fn initialize_touch(&mut self) -> Result<(), ClickerError> {
        if self.touch.is_none() {
            self.touch = Some(TouchApi::shared()?);
        }
        Ok(())
    }
    fn touch(&mut self, packets: &[POINTER_TOUCH_INFO]) -> Result<(), TouchFailure> {
        let Some(api) = &self.touch else {
            return Err(TouchFailure {
                retry: false,
                error: ErrorCode::InputInitialize.into(),
            });
        };
        if unsafe { (api.inject)(packets.len() as u32, packets.as_ptr()) } != 0 {
            return Ok(());
        }
        let code = unsafe { GetLastError() };
        Err(TouchFailure {
            retry: code == ERROR_NOT_READY,
            error: ClickerError::with_detail(
                ErrorCode::InputSend,
                std::io::Error::from_raw_os_error(code as i32).to_string(),
            ),
        })
    }
    fn retry_pause(&mut self) {
        std::thread::sleep(Duration::from_millis(1));
    }
    fn clock(&self, _: Instant) -> Instant {
        Instant::now()
    }
}

fn normalize(value: i32, origin: i32, size: i32) -> i32 {
    (((value as i64 - origin as i64) * 2 + 1) * 65536 / (size as i64 * 2)).clamp(0, 65535) as i32
}
pub(super) fn mouse_packet(desktop: Desktop, point: Option<Pixel>, flags: u32) -> INPUT {
    let (dx, dy, absolute) = point.map_or((0, 0, 0), |point| {
        (
            normalize(point.x, desktop.left, desktop.width),
            normalize(point.y, desktop.top, desktop.height),
            MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
        )
    });
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: 0,
                dwFlags: flags | absolute,
                time: 0,
                dwExtraInfo: crate::engine::KEY_CLICK_TAG,
            },
        },
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TouchPhase {
    Down,
    Update,
    Up,
}
pub(super) fn touch_packet(id: u32, point: Pixel, phase: TouchPhase) -> POINTER_TOUCH_INFO {
    let mut packet: POINTER_TOUCH_INFO = unsafe { zeroed() };
    packet.pointerInfo.pointerType = PT_TOUCH;
    packet.pointerInfo.pointerId = id;
    packet.pointerInfo.ptPixelLocation = POINT {
        x: point.x,
        y: point.y,
    };
    packet.pointerInfo.pointerFlags = match phase {
        TouchPhase::Down => POINTER_FLAG_DOWN | POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT,
        TouchPhase::Update => POINTER_FLAG_UPDATE | POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT,
        TouchPhase::Up => POINTER_FLAG_UP,
    };
    // With no optional mask, Windows supplies the default contact area/pressure.
    // Timestamps stay zero so all contacts in a frame share the API timestamp.
    packet
}

struct ResolvedAction {
    id: usize,
    point: Pixel,
    start: Pixel,
    pulse: bool,
}
struct Contact {
    pointer: u32,
    last: Pixel,
    updated: bool,
    pulse: bool,
}
struct MouseOwner {
    id: usize,
    last: Pixel,
    right: bool,
    pulse: bool,
    origin: Pixel,
    activity: u64,
}
struct Restore {
    origin: Pixel,
    last: Pixel,
    activity: u64,
    due: Instant,
}
pub(super) struct Runtime<S: Sink> {
    pub sink: S,
    planner: Planner,
    contacts: BTreeMap<usize, Contact>,
    mouse: Option<MouseOwner>,
    restore: Option<Restore>,
    mouse_blocked: BTreeSet<usize>,
    desktop: Option<Desktop>,
    cleanup_pending: bool,
    pub warning: Option<ClickerError>,
    pub completed: u64,
}
impl<S: Sink> Runtime<S> {
    pub fn new(sink: S) -> Self {
        Self {
            sink,
            planner: Planner::default(),
            contacts: BTreeMap::new(),
            mouse: None,
            restore: None,
            mouse_blocked: BTreeSet::new(),
            desktop: None,
            cleanup_pending: false,
            warning: None,
            completed: 0,
        }
    }
    fn warn(&mut self, code: ErrorCode, detail: &str) {
        self.warning = Some(ClickerError::with_detail(code, detail));
    }
    pub fn frame(
        &mut self,
        settings: &ActiveSettings,
        source: &SourceSnapshot,
        now: Instant,
        mut resolve: impl FnMut(&super::super::ClientPoint) -> Result<Pixel, ClickerError>,
        mut allowed: impl FnMut() -> bool,
    ) -> Result<(), ClickerError> {
        let result = catch_unwind(AssertUnwindSafe(|| {
            if self.cleanup_pending {
                return self.halt(source);
            }
            if source.shortcut || !allowed() {
                return self.halt(source);
            }
            if self.contacts.is_empty() && self.mouse.is_none() && self.restore.is_none() {
                self.desktop = None;
            }
            let actions = self.planner.actions(settings, source, now);
            self.mouse_blocked
                .retain(|id| actions.iter().any(|action| action.id == *id));
            if !allowed() {
                return self.halt(source);
            }
            if actions.is_empty()
                && self.contacts.is_empty()
                && self.mouse.is_none()
                && self.restore.is_none()
            {
                return Ok(());
            }
            let desktop = self.sink.desktop()?;
            if self.desktop.is_some_and(|old| old != desktop) {
                return Err(ErrorCode::ScreenChanged.into());
            }
            self.desktop = Some(desktop);
            if settings.input_method == InputMethod::Touch {
                let mut resolved = Vec::with_capacity(actions.len());
                for action in actions {
                    if action.right {
                        self.warn(ErrorCode::InputRightButton, "Touch cannot send right-button actions; select Mouse for this setting.");
                        self.planner.reject(action.id, now);
                        continue;
                    }
                    let point = resolve(&action.position)?;
                    let start = action
                        .start
                        .as_ref()
                        .map(&mut resolve)
                        .transpose()?
                        .unwrap_or(point);
                    if !desktop.contains(point) || !desktop.contains(start) {
                        return Err(ErrorCode::PointOutside.into());
                    }
                    resolved.push(ResolvedAction {
                        id: action.id,
                        point,
                        start,
                        pulse: action.pulse,
                    });
                }
                if !allowed() {
                    return self.halt(source);
                }
                self.reconcile_touch(&resolved, now, &mut allowed)?;
            } else {
                self.reconcile_mouse(&actions, now, desktop, &mut resolve, &mut allowed)?;
            }
            if !allowed() {
                self.halt(source)
            } else {
                Ok(())
            }
        }));
        let result = result.unwrap_or_else(|_| Err(ErrorCode::WorkerPanic.into()));
        if let Err(failure) = result {
            // The ownership journal is updated before calls that can unwind.
            // Cleanup failure does not hide the error that stopped the worker.
            let _ = self.halt(source);
            return Err(failure);
        }
        Ok(())
    }
    fn reconcile_touch(
        &mut self,
        actions: &[ResolvedAction],
        now: Instant,
        allowed: &mut impl FnMut() -> bool,
    ) -> Result<(), ClickerError> {
        let mut desired = BTreeMap::new();
        for action in actions {
            desired.insert(action.id, action);
        }
        if desired.is_empty() && self.contacts.is_empty() {
            return Ok(());
        }
        self.sink.initialize_touch()?;
        let mut frame = Vec::new();
        for (&id, contact) in &self.contacts {
            let (phase, point) = if let Some(action) = desired.get(&id) {
                (TouchPhase::Update, action.point)
            } else if contact.updated {
                (TouchPhase::Up, contact.last)
            } else {
                (TouchPhase::Update, contact.last)
            };
            frame.push((id, point, phase));
        }
        let mut added = Vec::new();
        for (&id, action) in &desired {
            if !self.contacts.contains_key(&id) {
                frame.push((id, action.start, TouchPhase::Down));
                added.push(id);
            }
        }
        if !allowed() {
            return Ok(());
        }
        for &id in &added {
            let action = desired[&id];
            self.contacts.insert(
                id,
                Contact {
                    pointer: id as u32 + 1,
                    last: action.start,
                    updated: false,
                    pulse: action.pulse,
                },
            );
        }
        let packets: Vec<_> = frame
            .iter()
            .map(|(id, point, phase)| touch_packet(self.contacts[id].pointer, *point, *phase))
            .collect();
        let mut sent = self.sink.touch(&packets);
        for _ in 0..2 {
            if !sent.as_ref().is_err_and(|failure| failure.retry) || !allowed() {
                break;
            }
            self.sink.retry_pause();
            if !allowed() {
                break;
            }
            // ERROR_NOT_READY inserted nothing. Retry the identical frame before
            // accepting another position or contact set into this sequence.
            sent = self.sink.touch(&packets);
        }
        match sent {
            Ok(()) => {
                for (id, point, phase) in frame {
                    match phase {
                        TouchPhase::Down => self.planner.started(id, self.sink.clock(now)),
                        TouchPhase::Update => {
                            let contact = self.contacts.get_mut(&id).unwrap();
                            contact.last = point;
                            contact.updated = true;
                        }
                        TouchPhase::Up => {
                            if self
                                .contacts
                                .remove(&id)
                                .is_some_and(|contact| contact.pulse)
                            {
                                self.completed += 1;
                                self.planner.released(id, self.sink.clock(now));
                            }
                        }
                    }
                }
                Ok(())
            }
            Err(failure) if failure.retry => {
                for id in added {
                    self.contacts.remove(&id);
                }
                if self.contacts.is_empty() || !allowed() {
                    // With no live sequence, the next tick can start a fresh
                    // gesture if the source key still calls for one.
                    Ok(())
                } else {
                    // Do not replace a repeatedly rejected live frame with a
                    // different one. Stop and release the last successful state.
                    Err(failure.error)
                }
            }
            Err(failure) => Err(failure.error),
        }
    }
    fn reconcile_mouse(
        &mut self,
        actions: &[Action],
        now: Instant,
        desktop: Desktop,
        resolve: &mut impl FnMut(&super::super::ClientPoint) -> Result<Pixel, ClickerError>,
        allowed: &mut impl FnMut() -> bool,
    ) -> Result<(), ClickerError> {
        let state = self.sink.mouse_state()?;
        if let Some(owner) = &self.mouse {
            if state.physical.activity != owner.activity
                || !state.position.matches_injected(owner.last)
            {
                let id = owner.id;
                self.release_mouse(now, false)?;
                self.restore = None;
                self.mouse_blocked.insert(id);
                for action in actions {
                    self.mouse_blocked.insert(action.id);
                    self.planner.reject(action.id, now);
                }
                self.warn(ErrorCode::InputMouseMoved, "Mouse movement interrupted the virtual control; release the source keys before using it again.");
            }
        }
        if let Some(owner) = &self.mouse {
            if !actions.iter().any(|action| action.id == owner.id) {
                self.release_mouse(now, true)?;
            }
        }
        self.restore_mouse(now, false)?;
        let current = self.mouse.as_ref().map(|owner| owner.id);
        let selected = current
            .and_then(|id| actions.iter().find(|action| action.id == id))
            .or_else(|| {
                actions
                    .iter()
                    .find(|action| !self.mouse_blocked.contains(&action.id))
            });
        let independent_hold = selected.is_some_and(|action| !action.pulse)
            || actions.iter().any(|action| !action.pulse);
        for action in actions {
            if self.mouse_blocked.contains(&action.id)
                || (independent_hold && selected.is_none_or(|selected| selected.id != action.id))
            {
                self.planner.reject(action.id, now);
            }
        }
        if actions.len() > 1 && independent_hold {
            self.warn(ErrorCode::InputCombination, "Mouse has one pointer and cannot hold separate positions or add an independent click while holding a control.");
        }
        let Some(action) = selected else {
            return Ok(());
        };
        if self.mouse_blocked.contains(&action.id) {
            self.planner.reject(action.id, now);
            return Ok(());
        }
        if self.mouse.is_none() {
            // A preceding one-shot click owns its release dwell. Leave other
            // fresh pulses pending (bounded by MAX_PRESS_AGE), rather than
            // turning ordinary Click+Click into a rejected multi-hold.
            if self.restore.is_some() {
                return Ok(());
            }
            let state = self.sink.mouse_state()?;
            if state.physical.left || state.physical.right {
                self.planner.reject(action.id, now);
                self.mouse_blocked.insert(action.id);
                self.warn(ErrorCode::InputMouseBusy, "Mouse is in use; the additional action was skipped to preserve the current input.");
                return Ok(());
            }
        }
        let point = resolve(&action.position)?;
        let start = action
            .start
            .as_ref()
            .map(&mut *resolve)
            .transpose()?
            .unwrap_or(point);
        if !desktop.contains(point) || !desktop.contains(start) {
            return Err(ErrorCode::PointOutside.into());
        }
        if self.mouse.is_none() {
            let state = self.sink.mouse_state()?;
            if self.restore.is_some() || state.physical.left || state.physical.right {
                self.planner.reject(action.id, now);
                self.mouse_blocked.insert(action.id);
                self.warn(ErrorCode::InputMouseBusy, "Mouse is in use; the additional action was skipped to preserve the current input.");
                return Ok(());
            }
            if !allowed() {
                return Ok(());
            }
            self.mouse = Some(MouseOwner {
                id: action.id,
                last: start,
                right: action.right,
                pulse: action.pulse,
                origin: state.position,
                activity: state.physical.activity,
            });
            let packets = [
                mouse_packet(desktop, Some(start), 0),
                mouse_packet(
                    desktop,
                    None,
                    if action.right {
                        MOUSEEVENTF_RIGHTDOWN
                    } else {
                        MOUSEEVENTF_LEFTDOWN
                    },
                ),
            ];
            if let Err(failure) = self.sink.mouse(&packets) {
                if failure.inserted < 2 {
                    let owner = self.mouse.take().unwrap();
                    if failure.inserted > 0 {
                        self.restore = Some(Restore {
                            origin: owner.origin,
                            last: start,
                            activity: owner.activity,
                            due: now,
                        });
                    }
                }
                return Err(failure.error);
            }
            self.planner.started(action.id, self.sink.clock(now));
        } else if let Some(owner) = self
            .mouse
            .as_mut()
            .filter(|owner| owner.last != point && allowed())
        {
            let packet = mouse_packet(desktop, Some(point), 0);
            self.sink
                .mouse(&[packet])
                .map_err(|failure| failure.error)?;
            owner.last = point;
        }
        Ok(())
    }
    fn release_mouse(&mut self, now: Instant, completed: bool) -> Result<(), ClickerError> {
        let Some(owner) = &self.mouse else {
            return Ok(());
        };
        let state = self.sink.mouse_state()?;
        let physical_down = if owner.right {
            state.physical.right
        } else {
            state.physical.left
        };
        if physical_down {
            // A physical DOWN has taken over the same OS button. Its physical UP
            // will release it; sending our UP here would release the user's hold.
            self.mouse = None;
            self.restore = None;
            self.warn(
                ErrorCode::InputMouseBusy,
                "A physical mouse press took over the button; its release is left to the user.",
            );
            return Ok(());
        }
        let desktop = self.desktop.unwrap();
        let packet = mouse_packet(
            desktop,
            None,
            if owner.right {
                MOUSEEVENTF_RIGHTUP
            } else {
                MOUSEEVENTF_LEFTUP
            },
        );
        if let Err(failure) = self.sink.mouse(&[packet]) {
            if failure.inserted == 0 {
                return Err(failure.error);
            }
        }
        let owner = self.mouse.take().unwrap();
        let now = self.sink.clock(now);
        if completed && owner.pulse {
            self.completed += 1;
        }
        self.planner.released(owner.id, now);
        self.restore = Some(Restore {
            origin: owner.origin,
            last: owner.last,
            activity: owner.activity,
            due: now
                + if owner.pulse {
                    Duration::from_millis(50)
                } else {
                    Duration::ZERO
                },
        });
        Ok(())
    }
    fn restore_mouse(&mut self, now: Instant, force: bool) -> Result<(), ClickerError> {
        let Some(restore) = &self.restore else {
            return Ok(());
        };
        if !force && now < restore.due {
            return Ok(());
        }
        let state = self.sink.mouse_state()?;
        if state.physical.activity == restore.activity
            && state.position.matches_injected(restore.last)
            && !state.physical.left
            && !state.physical.right
            && self.sink.desktop()? == self.desktop.unwrap()
        {
            let packet = mouse_packet(self.desktop.unwrap(), Some(restore.origin), 0);
            self.sink
                .mouse(&[packet])
                .map_err(|failure| failure.error)?;
        }
        self.restore = None;
        Ok(())
    }
    /// Synchronous release barrier. No lock owner can return from this method
    /// with queued presses; unconfirmed releases remain journaled for retry.
    pub fn halt(&mut self, source: &SourceSnapshot) -> Result<(), ClickerError> {
        self.planner.reset(source);
        self.mouse_blocked.clear();
        let mut failure = None;
        for _ in 0..3 {
            let result = catch_unwind(AssertUnwindSafe(|| {
                self.release_mouse(Instant::now(), false)
            }))
            .unwrap_or_else(|_| Err(ErrorCode::WorkerPanic.into()));
            if let Err(error) = result {
                failure = Some(error);
            }
            if self.mouse.is_none() {
                break;
            }
            self.sink.retry_pause();
        }
        if self.mouse.is_none() {
            let result = catch_unwind(AssertUnwindSafe(|| {
                self.restore_mouse(Instant::now(), true)
            }))
            .unwrap_or_else(|_| Err(ErrorCode::WorkerPanic.into()));
            if let Err(error) = result {
                failure = Some(error);
            }
        }
        if !self.contacts.is_empty() {
            // The last successful UPDATE is the authoritative UP position. Send
            // an UPDATE even after a lone DOWN, then end all owned contacts.
            let updates: Vec<_> = self
                .contacts
                .values()
                .map(|contact| touch_packet(contact.pointer, contact.last, TouchPhase::Update))
                .collect();
            if let Err(error) = self.cleanup_touch(&updates) {
                failure = Some(error);
            }
            let releases: Vec<_> = self
                .contacts
                .values()
                .map(|contact| touch_packet(contact.pointer, contact.last, TouchPhase::Up))
                .collect();
            match self.cleanup_touch(&releases) {
                Ok(()) => self.contacts.clear(),
                Err(error) => failure = Some(error),
            }
        }
        if self.contacts.is_empty() && self.mouse.is_none() && self.restore.is_none() {
            self.desktop = None;
        }
        self.cleanup_pending =
            !self.contacts.is_empty() || self.mouse.is_some() || self.restore.is_some();
        failure.map_or(Ok(()), Err)
    }
    fn cleanup_touch(&mut self, packets: &[POINTER_TOUCH_INFO]) -> Result<(), ClickerError> {
        let mut failure = ErrorCode::InputSend.into();
        for _ in 0..3 {
            match catch_unwind(AssertUnwindSafe(|| self.sink.touch(packets))) {
                Ok(Ok(())) => return Ok(()),
                Ok(Err(error)) => failure = error.error,
                Err(_) => failure = ErrorCode::WorkerPanic.into(),
            }
            self.sink.retry_pause();
        }
        Err(failure)
    }
}
impl<S: Sink> Drop for Runtime<S> {
    fn drop(&mut self) {
        // Do not let a failing backend panic from Drop, including during unwind.
        let _ = catch_unwind(AssertUnwindSafe(|| {
            self.halt(&SourceKeyState::default().snapshot())
        }));
    }
}
