use crate::drag::{DragMonitor, DragState, point_hits_process};
use crate::error::{ClickerError, ErrorCode};
use crate::model::{ClickMode, Point, Settings};
use serde::Serialize;
use std::{
    mem::size_of,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr::null,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::{MONITOR_DEFAULTTONULL, MonitorFromPoint},
    System::Threading::*,
    UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

#[derive(Clone, Debug, Default, Serialize)]
pub struct Progress {
    pub clicks: u64,
    pub cycles: u64,
    pub elapsed_ms: u64,
    pub countdown_ms: u64,
    pub running: bool,
    pub error: Option<ClickerError>,
    pub mean_input_ms: f64,
    pub locked: bool,
    pub waiting_for_drag: bool,
}

struct Handle(HANDLE);
// These are unnamed kernel synchronization handles, safe to signal/wait across threads.
// Arc ownership keeps the event alive until both the worker and Runner have released it.
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
struct Signal {
    event: Handle,
    stopped: AtomicBool,
}
impl Signal {
    fn new() -> Result<Self, ClickerError> {
        let h = unsafe { CreateEventW(null(), 1, 0, null()) };
        if h.is_null() {
            return Err(os_error(ErrorCode::SignalCreate));
        }
        Ok(Self {
            event: Handle(h),
            stopped: AtomicBool::new(false),
        })
    }
    fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        unsafe {
            SetEvent(self.event.0);
        }
    }
    fn stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }
}
fn os_error(code: ErrorCode) -> ClickerError {
    ClickerError::with_detail(code, std::io::Error::last_os_error().to_string())
}

struct Waiter {
    timer: Handle,
    signal: Arc<Signal>,
    high_resolution: bool,
}
impl Waiter {
    fn new(signal: Arc<Signal>) -> Result<Self, ClickerError> {
        let mut timer = unsafe {
            CreateWaitableTimerExW(
                null(),
                null(),
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_ALL_ACCESS,
            )
        };
        let mut high_resolution = true;
        if timer.is_null() {
            high_resolution = false;
            timer = unsafe { CreateWaitableTimerExW(null(), null(), 0, TIMER_ALL_ACCESS) };
        }
        if timer.is_null() {
            return Err(os_error(ErrorCode::TimerCreate));
        }
        Ok(Self {
            timer: Handle(timer),
            signal,
            high_resolution,
        })
    }
    fn until(&self, deadline: Instant) -> Result<bool, ClickerError> {
        loop {
            if self.signal.stopped() {
                return Ok(false);
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(true);
            }
            // Sleep for most of the gap; a bounded 150µs final spin avoids timer
            // dispatch overhead dominating a 1ms interval. Cancellation stays live.
            let remaining = deadline.duration_since(now);
            let guard = Duration::from_micros(150);
            if remaining <= guard {
                std::hint::spin_loop();
                continue;
            }
            let ticks = ((remaining - guard).as_nanos().div_ceil(100)).min(i64::MAX as u128) as i64;
            let due = -ticks.max(1);
            if unsafe { SetWaitableTimerEx(self.timer.0, &due, 0, None, null(), null(), 0) } == 0 {
                return Err(os_error(ErrorCode::TimerSet));
            }
            let handles = [self.signal.event.0, self.timer.0];
            match unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) } {
                WAIT_OBJECT_0 => return Ok(false),
                x if x == WAIT_OBJECT_0 + 1 => {}
                _ => return Err(os_error(ErrorCode::TimerWait)),
            }
        }
    }
}

struct Shared {
    clicks: AtomicU64,
    cycles: AtomicU64,
    elapsed_ms: AtomicU64,
    running: AtomicBool,
    input_ns: AtomicU64,
    error: Mutex<Option<ClickerError>>,
}
pub struct Runner {
    signal: Arc<Signal>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    started: Instant,
    delay_ms: u64,
    drag_monitor: Option<DragMonitor>,
    gate: Option<Arc<DragState>>,
}
impl Runner {
    pub fn start(settings: Settings) -> Result<Self, ClickerError> {
        settings.validate()?;
        let desktop = Desktop::current()?;
        if settings.mode == ClickMode::Recorded {
            for point in &settings.points {
                desktop.validate(point)?;
            }
        }
        if settings.mode == ClickMode::Area {
            let a = settings.area.ok_or(ErrorCode::AreaMissing)?;
            for (x, y) in [(a.left, a.top), (a.right, a.bottom)] {
                desktop.validate(&Point {
                    x,
                    y,
                    interval_ticks: None,
                })?;
            }
        }
        let drag_monitor = if settings.mode == ClickMode::Drag {
            Some(DragMonitor::start(std::process::id())?)
        } else {
            None
        };
        let gate = drag_monitor.as_ref().map(|m| m.state.clone());
        let mode = settings.mode;
        let mut runner = Self::with_control(
            settings,
            move |point, right| {
                if matches!(mode, ClickMode::Pointer | ClickMode::Drag) {
                    desktop.send_at_cursor(right)
                } else {
                    if mode == ClickMode::Area
                        && point_hits_process(
                            POINT {
                                x: point.x,
                                y: point.y,
                            },
                            std::process::id(),
                        )
                    {
                        return Ok(false);
                    }
                    desktop.send(point, right).map(|_| true)
                }
            },
            true,
            gate,
        )?;
        runner.drag_monitor = drag_monitor;
        Ok(runner)
    }
    #[cfg(test)]
    fn with_emitter<F>(settings: Settings, emit: F) -> Result<Self, ClickerError>
    where
        F: FnMut(&Point, bool) -> Result<(), ClickerError> + Send + 'static,
    {
        Self::with_emitter_mode(settings, emit, false)
    }
    #[cfg(test)]
    fn with_emitter_mode<F>(
        settings: Settings,
        mut emit: F,
        await_modifiers: bool,
    ) -> Result<Self, ClickerError>
    where
        F: FnMut(&Point, bool) -> Result<(), ClickerError> + Send + 'static,
    {
        Self::with_control(
            settings,
            move |p, right| {
                emit(p, right)?;
                Ok(true)
            },
            await_modifiers,
            None,
        )
    }
    fn with_control<F>(
        settings: Settings,
        mut emit: F,
        await_modifiers: bool,
        gate: Option<Arc<DragState>>,
    ) -> Result<Self, ClickerError>
    where
        F: FnMut(&Point, bool) -> Result<bool, ClickerError> + Send + 'static,
    {
        settings.validate()?;
        if settings.mode == ClickMode::Recorded && settings.points.is_empty() {
            return Err(ErrorCode::EmptyPoints.into());
        }
        let points_per_cycle = match settings.mode {
            ClickMode::Recorded => settings.points.len() as u64,
            ClickMode::Area => {
                let (x, y) = settings
                    .area
                    .ok_or(ErrorCode::AreaMissing)?
                    .grid_size(settings.area_step_px)
                    .ok_or(ErrorCode::AreaInvalid)?;
                x * y
            }
            _ => 1,
        };
        let signal = Arc::new(Signal::new()?);
        let waiter = Waiter::new(signal.clone())?;
        let shared = Arc::new(Shared {
            clicks: AtomicU64::new(0),
            cycles: AtomicU64::new(0),
            elapsed_ms: AtomicU64::new(0),
            running: AtomicBool::new(true),
            input_ns: AtomicU64::new(0),
            error: Mutex::new(None),
        });
        let started = Instant::now();
        let delay_ms = settings.delay_ms;
        let thread_shared = shared.clone();
        let thread_signal = signal.clone();
        let thread_gate = gate.clone();
        let worker = thread::Builder::new()
            .name("click-timer".into())
            .spawn(move || {
                // A modest priority increase reduces jitter without realtime priority.
                unsafe {
                    SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL);
                }
                let result = catch_unwind(AssertUnwindSafe(|| {
                    if !waiter.until(started + Duration::from_millis(settings.delay_ms))? {
                        return Ok(());
                    }
                    // Starting via a global chord must not turn the first click
                    // into Ctrl/Alt-click before the user releases the keys.
                    if await_modifiers {
                        while [VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN]
                            .iter()
                            .any(|key| unsafe { GetAsyncKeyState(*key as i32) } < 0)
                        {
                            if !waiter.until(Instant::now() + Duration::from_millis(2))? {
                                return Ok(());
                            }
                        }
                    }
                    let mut cycles = 0u64;
                    let mut index = 0u64;
                    'outer: loop {
                        {
                            if thread_signal.stopped() {
                                break 'outer;
                            }
                            if await_modifiers
                                && [VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN]
                                    .iter()
                                    .any(|key| unsafe { GetAsyncKeyState(*key as i32) } < 0)
                            {
                                if !waiter.until(Instant::now() + Duration::from_millis(2))? {
                                    break 'outer;
                                }
                                continue;
                            }
                            if thread_gate.as_ref().is_some_and(|g| !g.engaged()) {
                                if !waiter.until(Instant::now() + Duration::from_millis(2))? {
                                    break 'outer;
                                }
                                continue;
                            }
                            let point = match settings.mode {
                                ClickMode::Recorded => settings.points[index as usize].clone(),
                                ClickMode::Area => settings
                                    .area
                                    .and_then(|a| a.point_at(settings.area_step_px, index))
                                    .ok_or(ErrorCode::AreaInvalid)?,
                                _ => Point {
                                    x: 0,
                                    y: 0,
                                    interval_ticks: None,
                                },
                            };
                            let sent_at = Instant::now();
                            if !emit(&point, settings.button == "right")? {
                                if settings.mode == ClickMode::Area {
                                    index += 1;
                                    if index == points_per_cycle {
                                        index = 0;
                                        cycles += 1;
                                        thread_shared.cycles.store(cycles, Ordering::Relaxed);
                                        if settings.repeat != 0 && cycles >= settings.repeat as u64
                                        {
                                            break 'outer;
                                        }
                                    }
                                }
                                if !waiter.until(Instant::now() + Duration::from_millis(2))? {
                                    break 'outer;
                                }
                                continue;
                            }
                            thread_shared.input_ns.fetch_add(
                                sent_at.elapsed().as_nanos().min(u64::MAX as u128) as u64,
                                Ordering::Relaxed,
                            );
                            thread_shared.clicks.fetch_add(1, Ordering::Relaxed);
                            let last = index + 1 == points_per_cycle;
                            if last {
                                cycles += 1;
                                thread_shared.cycles.store(cycles, Ordering::Relaxed);
                            }
                            if last
                                && matches!(settings.mode, ClickMode::Recorded | ClickMode::Area)
                                && settings.repeat != 0
                                && cycles >= settings.repeat as u64
                            {
                                break 'outer;
                            }
                            // Each deadline starts at this click, never at a stale global tick. Late
                            // scheduling therefore cannot accumulate a backlog of catch-up clicks.
                            let interval = if settings.mode == ClickMode::Recorded {
                                settings.interval_for(index as usize)
                            } else {
                                settings.fast_interval_ticks
                            };
                            index = if last { 0 } else { index + 1 };
                            if !waiter.until(sent_at + Duration::from_nanos(interval * 100))? {
                                break 'outer;
                            }
                        }
                    }
                    Ok::<(), ClickerError>(())
                }));
                let error = match result {
                    Ok(Ok(())) => None,
                    Ok(Err(e)) => Some(e),
                    Err(_) => Some(ErrorCode::WorkerPanic.into()),
                };
                if let Some(error) = error {
                    if let Ok(mut slot) = thread_shared.error.lock() {
                        *slot = Some(error);
                    }
                }
                thread_shared.elapsed_ms.store(
                    started
                        .elapsed()
                        .as_millis()
                        .saturating_sub(delay_ms as u128) as u64,
                    Ordering::Relaxed,
                );
                thread_shared.running.store(false, Ordering::Release);
            })
            .map_err(|e| ClickerError::with_detail(ErrorCode::ThreadStart, e.to_string()))?;
        Ok(Self {
            signal,
            shared,
            thread: Some(worker),
            started,
            delay_ms,
            drag_monitor: None,
            gate,
        })
    }
    pub fn snapshot(&self) -> Progress {
        let running = self.shared.running.load(Ordering::Acquire);
        let since = self.started.elapsed().as_millis() as u64;
        Progress {
            clicks: self.shared.clicks.load(Ordering::Relaxed),
            cycles: self.shared.cycles.load(Ordering::Relaxed),
            elapsed_ms: if running {
                since.saturating_sub(self.delay_ms)
            } else {
                self.shared.elapsed_ms.load(Ordering::Relaxed)
            },
            countdown_ms: if running {
                self.delay_ms.saturating_sub(since)
            } else {
                0
            },
            running,
            error: self.shared.error.lock().ok().and_then(|e| e.clone()),
            mean_input_ms: self.shared.input_ns.load(Ordering::Relaxed) as f64
                / self.shared.clicks.load(Ordering::Relaxed).max(1) as f64
                / 1_000_000.,
            locked: self
                .gate
                .as_ref()
                .is_some_and(|g| g.locked.load(Ordering::Acquire)),
            waiting_for_drag: self.gate.as_ref().is_some_and(|g| !g.engaged()),
        }
    }
    pub fn stop(&mut self) {
        self.signal.stop();
        if let Some(worker) = self.thread.take() {
            let _ = worker.join();
        }
        if let Some(mut monitor) = self.drag_monitor.take() {
            monitor.stop();
        }
        if let Some(gate) = &self.gate {
            gate.locked.store(false, Ordering::Release);
            gate.active.store(false, Ordering::Release);
        }
    }
    pub fn toggle_lock(&self) -> bool {
        if self.finished() {
            return false;
        }
        if let Some(gate) = &self.gate {
            !gate.locked.fetch_xor(true, Ordering::AcqRel)
        } else {
            false
        }
    }
    pub fn finished(&self) -> bool {
        !self.shared.running.load(Ordering::Acquire)
    }
}
impl Drop for Runner {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Desktop {
    left: i32,
    top: i32,
    width: i32,
    height: i32,
}
impl Desktop {
    fn send_at_cursor(&self, right: bool) -> Result<bool, ClickerError> {
        if Self::current()? != *self {
            return Err(ErrorCode::ScreenChanged.into());
        }
        let mut point = POINT { x: 0, y: 0 };
        if unsafe { GetCursorPos(&mut point) } == 0 {
            return Err(os_error(ErrorCode::InputSend));
        }
        if point_hits_process(point, std::process::id()) {
            return Ok(false);
        }
        self.validate(&Point {
            x: point.x,
            y: point.y,
            interval_ticks: None,
        })?;
        // Button events carry no MOVE/ABSOLUTE flag: user motion remains untouched.
        let events = pointer_inputs(right);
        if unsafe { SendInput(2, events.as_ptr(), size_of::<INPUT>() as i32) } != 2 {
            let err = os_error(ErrorCode::InputSend);
            let release = pointer_inputs(right)[1];
            unsafe {
                SendInput(1, &release, size_of::<INPUT>() as i32);
            }
            return Err(err);
        }
        Ok(true)
    }
    fn current() -> Result<Self, ClickerError> {
        let d = unsafe {
            Self {
                left: GetSystemMetrics(SM_XVIRTUALSCREEN),
                top: GetSystemMetrics(SM_YVIRTUALSCREEN),
                width: GetSystemMetrics(SM_CXVIRTUALSCREEN),
                height: GetSystemMetrics(SM_CYVIRTUALSCREEN),
            }
        };
        if d.width <= 0 || d.height <= 0 {
            return Err(ErrorCode::ScreenUnavailable.into());
        }
        Ok(d)
    }
    fn validate(&self, p: &Point) -> Result<(), ClickerError> {
        let x = p.x as i64 - self.left as i64;
        let y = p.y as i64 - self.top as i64;
        if x < 0
            || y < 0
            || x >= self.width as i64
            || y >= self.height as i64
            || unsafe { MonitorFromPoint(POINT { x: p.x, y: p.y }, MONITOR_DEFAULTTONULL) }
                .is_null()
        {
            return Err(ClickerError::with_detail(
                ErrorCode::PointOutside,
                format!("X {} Y {}", p.x, p.y),
            ));
        }
        Ok(())
    }
    fn send(&self, p: &Point, right: bool) -> Result<(), ClickerError> {
        if Self::current()? != *self {
            return Err(ErrorCode::ScreenChanged.into());
        }
        self.validate(p)?;
        let dx = normalize(p.x, self.left, self.width);
        let dy = normalize(p.y, self.top, self.height);
        let down = if right {
            MOUSEEVENTF_RIGHTDOWN
        } else {
            MOUSEEVENTF_LEFTDOWN
        };
        let up = if right {
            MOUSEEVENTF_RIGHTUP
        } else {
            MOUSEEVENTF_LEFTUP
        };
        let make = |flags| INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx,
                    dy,
                    mouseData: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        let inputs = [
            make(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK | down),
            make(up),
        ];
        // Move and DOWN form one event; UP follows in the same atomic SendInput batch.
        // SDK INPUT layout is used; Windows copies both events before this array expires.
        let count = unsafe {
            SendInput(
                inputs.len() as u32,
                inputs.as_ptr(),
                size_of::<INPUT>() as i32,
            )
        };
        if count != inputs.len() as u32 {
            let error = os_error(ErrorCode::InputSend);
            // A partial batch may include DOWN. Always attempt UP before reporting the failure.
            let release = make(up);
            unsafe {
                SendInput(1, &release, size_of::<INPUT>() as i32);
            }
            return Err(error);
        }
        Ok(())
    }
}
fn pointer_inputs(right: bool) -> [INPUT; 2] {
    let (down, up) = if right {
        (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP)
    } else {
        (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP)
    };
    [down, up].map(|flags| INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    })
}
fn normalize(value: i32, origin: i32, size: i32) -> i32 {
    (((value as i64 - origin as i64) * 2 + 1) * 65536 / (size as i64 * 2)).clamp(0, 65535) as i32
}

#[derive(Debug, Serialize)]
pub struct BenchmarkReport {
    pub requested_interval_ticks: u64,
    pub intervals: usize,
    pub high_resolution: bool,
    pub elapsed_ms: f64,
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub max_ms: f64,
}
pub fn benchmark(interval_ticks: u64, count: usize) -> Result<BenchmarkReport, ClickerError> {
    if !(crate::model::MIN_INTERVAL_TICKS..=crate::model::MAX_INTERVAL_TICKS)
        .contains(&interval_ticks)
        || count == 0
        || count > 10_000
    {
        return Err(ErrorCode::BenchmarkRange.into());
    }
    let waiter = Waiter::new(Arc::new(Signal::new()?))?;
    let start = Instant::now();
    let mut previous = start;
    let mut measurements = Vec::with_capacity(count);
    for _ in 0..count {
        waiter.until(previous + Duration::from_nanos(interval_ticks * 100))?;
        let now = Instant::now();
        measurements.push(now.duration_since(previous).as_secs_f64() * 1000.);
        previous = now;
    }
    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.;
    let mean_ms = measurements.iter().sum::<f64>() / count as f64;
    measurements.sort_by(|a, b| a.total_cmp(b));
    Ok(BenchmarkReport {
        requested_interval_ticks: interval_ticks,
        intervals: count,
        high_resolution: waiter.high_resolution,
        elapsed_ms,
        mean_ms,
        p50_ms: measurements[count / 2],
        p95_ms: measurements[(count * 95 / 100).min(count - 1)],
        max_ms: measurements[count - 1],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn area_visits_every_grid_location_without_a_large_point_buffer() {
        let area = crate::model::ClickArea::from_corners((-4, -3), (4, 3));
        let expected: Vec<_> = (0..9)
            .map(|i| {
                let p = area.point_at(4, i).unwrap();
                (p.x, p.y)
            })
            .collect();
        let events = Arc::new(Mutex::new(Vec::new()));
        let copy = events.clone();
        let mut runner = Runner::with_emitter_mode(
            Settings {
                mode: ClickMode::Area,
                area: Some(area),
                area_step_px: 4,
                fast_interval_ticks: 1,
                repeat: 2,
                ..Settings::default()
            },
            move |p, _| {
                copy.lock().unwrap().push((p.x, p.y));
                Ok(())
            },
            false,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !runner.finished() && Instant::now() < deadline {
            thread::yield_now();
        }
        assert!(runner.finished());
        runner.stop();
        let recorded = events.lock().unwrap();
        assert_eq!(&recorded[..9], &expected);
        assert_eq!(&recorded[9..], &expected);
        assert_eq!(runner.snapshot().cycles, 2);
        assert_eq!(runner.snapshot().clicks, 18);
    }
    #[test]
    fn skipped_region_cells_advance_and_complete() {
        let a = crate::model::ClickArea::from_corners((0, 0), (2, 2));
        let mut runner = Runner::with_control(
            Settings {
                mode: ClickMode::Area,
                area: Some(a),
                area_step_px: 1,
                fast_interval_ticks: 1,
                repeat: 1,
                ..Settings::default()
            },
            |_, _| Ok(false),
            false,
            None,
        )
        .unwrap();
        let end = Instant::now() + Duration::from_secs(2);
        while !runner.finished() && Instant::now() < end {
            thread::yield_now();
        }
        assert!(runner.finished());
        runner.stop();
        assert_eq!(runner.snapshot().cycles, 1);
        assert_eq!(runner.snapshot().clicks, 0);
    }
    #[test]
    fn drag_waits_without_input_lock_works_and_stop_remains_quiet() {
        let gate = Arc::new(DragState::idle());
        let control = gate.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let (release, may_return) = std::sync::mpsc::sync_channel(0);
        let mut first = true;
        let mut runner = Runner::with_control(
            Settings {
                mode: ClickMode::Drag,
                repeat: 1,
                fast_interval_ticks: 1,
                ..Settings::default()
            },
            move |_, _| {
                let _ = tx.send(());
                if first {
                    first = false;
                    may_return.recv_timeout(Duration::from_secs(2)).unwrap();
                }
                Ok(true)
            },
            false,
            Some(gate),
        )
        .unwrap();
        assert!(rx.recv_timeout(Duration::from_millis(30)).is_err());
        assert!(runner.toggle_lock());
        rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(!runner.toggle_lock());
        release.send(()).unwrap();
        assert!(rx.recv_timeout(Duration::from_millis(30)).is_err());
        // Physical input can resume the worker without recreating it, after lock release.
        control.active.store(true, Ordering::Release);
        rx.recv_timeout(Duration::from_secs(1)).unwrap();
        runner.stop();
        let stopped = runner.snapshot().clicks;
        assert!(runner.finished());
        assert!(!runner.snapshot().locked);
        assert!(!control.engaged());
        assert!(!runner.toggle_lock());
        assert_eq!(runner.snapshot().clicks, stopped);
    }
    #[test]
    fn pointer_events_cannot_reposition_the_mouse() {
        for right in [false, true] {
            let events = pointer_inputs(right);
            for e in events {
                assert_eq!(e.r#type, INPUT_MOUSE);
                let m = unsafe { e.Anonymous.mi };
                assert_eq!(m.dx, 0);
                assert_eq!(m.dy, 0);
                assert_eq!(
                    m.dwFlags & (MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK),
                    0
                );
            }
            let down = unsafe { events[0].Anonymous.mi.dwFlags };
            let up = unsafe { events[1].Anonymous.mi.dwFlags };
            assert_eq!(
                down,
                if right {
                    MOUSEEVENTF_RIGHTDOWN
                } else {
                    MOUSEEVENTF_LEFTDOWN
                }
            );
            assert_eq!(
                up,
                if right {
                    MOUSEEVENTF_RIGHTUP
                } else {
                    MOUSEEVENTF_LEFTUP
                }
            );
        }
    }
    #[test]
    fn normalized_coordinates_hit_pixel_centers_including_negative_monitors() {
        assert_eq!(normalize(-1920, -1920, 3840), 8);
        assert_eq!(normalize(1919, -1920, 3840), 65527);
        assert!(normalize(0, -1920, 3840) > 32768);
    }
    #[test]
    fn finite_cycles_follow_point_order_without_final_wait() {
        let points = vec![
            Point {
                x: 1,
                y: 0,
                interval_ticks: Some(1),
            },
            Point {
                x: 2,
                y: 0,
                interval_ticks: Some(1),
            },
            Point {
                x: 3,
                y: 0,
                interval_ticks: Some(300_000_000),
            },
        ];
        let events = Arc::new(Mutex::new(Vec::new()));
        let recorded = events.clone();
        let settings = Settings {
            points,
            repeat: 1,
            per_point: true,
            ..Settings::default()
        };
        let mut runner = Runner::with_emitter(settings, move |p, _| {
            recorded.lock().unwrap().push(p.x);
            Ok(())
        })
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !runner.finished() && Instant::now() < deadline {
            thread::yield_now();
        }
        assert!(
            runner.finished(),
            "finite final click must finish without waiting another interval"
        );
        runner.stop();
        assert_eq!(*events.lock().unwrap(), vec![1, 2, 3]);
        assert_eq!(runner.snapshot().cycles, 1);
        assert_eq!(runner.snapshot().clicks, 3);
    }
    #[test]
    fn stop_interrupts_a_long_interval_and_joins_the_worker() {
        let (sent, received) = std::sync::mpsc::sync_channel(1);
        let points = vec![Point {
            x: 0,
            y: 0,
            interval_ticks: None,
        }];
        let mut runner = Runner::with_emitter(
            Settings {
                points,
                interval_ticks: 300_000_000,
                ..Settings::default()
            },
            move |_, _| {
                let _ = sent.try_send(());
                Ok(())
            },
        )
        .unwrap();
        received.recv_timeout(Duration::from_secs(2)).unwrap();
        let start = Instant::now();
        runner.stop();
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(runner.finished());
        assert_eq!(runner.snapshot().clicks, 1);
        assert!(runner.thread.is_none());
    }
    #[test]
    fn stop_during_countdown_never_injects() {
        let calls = Arc::new(AtomicU64::new(0));
        let emitted = calls.clone();
        let mut runner = Runner::with_emitter(
            Settings {
                points: vec![Point {
                    x: 0,
                    y: 0,
                    interval_ticks: None,
                }],
                delay_ms: 30_000,
                ..Settings::default()
            },
            move |_, _| {
                emitted.fetch_add(1, Ordering::Relaxed);
                Ok(())
            },
        )
        .unwrap();
        let stopping = Instant::now();
        runner.stop();
        assert!(stopping.elapsed() < Duration::from_secs(1));
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert_eq!(runner.snapshot().clicks, 0);
    }
    #[test]
    fn injection_failure_and_panic_reach_the_ui() {
        for panic in [false, true] {
            let mut runner = Runner::with_emitter(
                Settings {
                    points: vec![Point {
                        x: 0,
                        y: 0,
                        interval_ticks: None,
                    }],
                    ..Settings::default()
                },
                move |_, _| {
                    assert!(!panic, "fake injection panic");
                    Err("fake injection error".into())
                },
            )
            .unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            while !runner.finished() && Instant::now() < deadline {
                thread::yield_now();
            }
            runner.stop();
            assert!(runner.snapshot().error.is_some());
            assert_eq!(runner.snapshot().clicks, 0);
        }
    }
}
