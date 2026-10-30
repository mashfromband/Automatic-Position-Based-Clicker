use crate::model::{Point, Settings};
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
    pub error: Option<String>,
    pub mean_input_ms: f64,
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
    fn new() -> Result<Self, String> {
        let h = unsafe { CreateEventW(null(), 1, 0, null()) };
        if h.is_null() {
            return Err(os_error("停止イベントを作成できません"));
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
fn os_error(message: &str) -> String {
    format!("{message}: {}", std::io::Error::last_os_error())
}

struct Waiter {
    timer: Handle,
    signal: Arc<Signal>,
    high_resolution: bool,
}
impl Waiter {
    fn new(signal: Arc<Signal>) -> Result<Self, String> {
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
            return Err(os_error("待機タイマーを作成できません"));
        }
        Ok(Self {
            timer: Handle(timer),
            signal,
            high_resolution,
        })
    }
    fn until(&self, deadline: Instant) -> Result<bool, String> {
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
                return Err(os_error("待機時間を設定できません"));
            }
            let handles = [self.signal.event.0, self.timer.0];
            match unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) } {
                WAIT_OBJECT_0 => return Ok(false),
                x if x == WAIT_OBJECT_0 + 1 => {}
                _ => return Err(os_error("クリック間隔の待機に失敗しました")),
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
    error: Mutex<Option<String>>,
}
pub struct Runner {
    signal: Arc<Signal>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    started: Instant,
    delay_ms: u64,
}
impl Runner {
    pub fn start(settings: Settings) -> Result<Self, String> {
        settings.validate()?;
        let desktop = Desktop::current()?;
        for point in &settings.points {
            desktop.validate(point)?;
        }
        Self::with_emitter_mode(
            settings,
            move |point, right| desktop.send(point, right),
            true,
        )
    }
    #[cfg(test)]
    fn with_emitter<F>(settings: Settings, emit: F) -> Result<Self, String>
    where
        F: FnMut(&Point, bool) -> Result<(), String> + Send + 'static,
    {
        Self::with_emitter_mode(settings, emit, false)
    }
    fn with_emitter_mode<F>(
        settings: Settings,
        mut emit: F,
        await_modifiers: bool,
    ) -> Result<Self, String>
    where
        F: FnMut(&Point, bool) -> Result<(), String> + Send + 'static,
    {
        settings.validate()?;
        if settings.points.is_empty() {
            return Err("クリック位置がありません。".into());
        }
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
                    let click_started = Instant::now();
                    let mut cycles = 0u64;
                    'outer: loop {
                        for (i, point) in settings.points.iter().enumerate() {
                            if thread_signal.stopped() {
                                break 'outer;
                            }
                            let sent_at = Instant::now();
                            emit(point, settings.button == "right")?;
                            thread_shared.input_ns.fetch_add(
                                sent_at.elapsed().as_nanos().min(u64::MAX as u128) as u64,
                                Ordering::Relaxed,
                            );
                            thread_shared.clicks.fetch_add(1, Ordering::Relaxed);
                            let last = i + 1 == settings.points.len();
                            if last {
                                cycles += 1;
                                thread_shared.cycles.store(cycles, Ordering::Relaxed);
                            }
                            if last && settings.repeat != 0 && cycles >= settings.repeat as u64 {
                                break 'outer;
                            }
                            // Each deadline starts at this click, never at a stale global tick. Late
                            // scheduling therefore cannot accumulate a backlog of catch-up clicks.
                            if !waiter
                                .until(sent_at + Duration::from_millis(settings.interval_for(i)))?
                            {
                                break 'outer;
                            }
                        }
                    }
                    thread_shared.elapsed_ms.store(
                        click_started.elapsed().as_millis() as u64,
                        Ordering::Relaxed,
                    );
                    Ok::<(), String>(())
                }));
                let error = match result {
                    Ok(Ok(())) => None,
                    Ok(Err(e)) => Some(e),
                    Err(_) => Some("クリック処理で予期しないエラーが発生しました。".into()),
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
            .map_err(|e| format!("クリック処理を開始できません: {e}"))?;
        Ok(Self {
            signal,
            shared,
            thread: Some(worker),
            started,
            delay_ms,
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
        }
    }
    pub fn stop(&mut self) {
        self.signal.stop();
        if let Some(worker) = self.thread.take() {
            let _ = worker.join();
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
    fn current() -> Result<Self, String> {
        let d = unsafe {
            Self {
                left: GetSystemMetrics(SM_XVIRTUALSCREEN),
                top: GetSystemMetrics(SM_YVIRTUALSCREEN),
                width: GetSystemMetrics(SM_CXVIRTUALSCREEN),
                height: GetSystemMetrics(SM_CYVIRTUALSCREEN),
            }
        };
        if d.width <= 0 || d.height <= 0 {
            return Err("画面の座標を取得できません。".into());
        }
        Ok(d)
    }
    fn validate(&self, p: &Point) -> Result<(), String> {
        let x = p.x as i64 - self.left as i64;
        let y = p.y as i64 - self.top as i64;
        if x < 0
            || y < 0
            || x >= self.width as i64
            || y >= self.height as i64
            || unsafe { MonitorFromPoint(POINT { x: p.x, y: p.y }, MONITOR_DEFAULTTONULL) }
                .is_null()
        {
            return Err(format!(
                "クリック位置 X {} Y {} が現在の画面外です。再記録してください。",
                p.x, p.y
            ));
        }
        Ok(())
    }
    fn send(&self, p: &Point, right: bool) -> Result<(), String> {
        if Self::current()? != *self {
            return Err("画面構成が変わったため停止しました。位置を確認してください。".into());
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
            let error = os_error("マウス入力を送信できません。対象アプリの権限を確認してください");
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
fn normalize(value: i32, origin: i32, size: i32) -> i32 {
    (((value as i64 - origin as i64) * 2 + 1) * 65536 / (size as i64 * 2)).clamp(0, 65535) as i32
}

#[derive(Debug, Serialize)]
pub struct BenchmarkReport {
    pub requested_interval_ms: u64,
    pub intervals: usize,
    pub high_resolution: bool,
    pub elapsed_ms: f64,
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub max_ms: f64,
}
pub fn benchmark(interval_ms: u64, count: usize) -> Result<BenchmarkReport, String> {
    if interval_ms == 0 || interval_ms > 3_600_000 || count == 0 || count > 10_000 {
        return Err("測定の範囲が不正です。".into());
    }
    let waiter = Waiter::new(Arc::new(Signal::new()?))?;
    let start = Instant::now();
    let mut previous = start;
    let mut measurements = Vec::with_capacity(count);
    for _ in 0..count {
        waiter.until(previous + Duration::from_millis(interval_ms))?;
        let now = Instant::now();
        measurements.push(now.duration_since(previous).as_secs_f64() * 1000.);
        previous = now;
    }
    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.;
    let mean_ms = measurements.iter().sum::<f64>() / count as f64;
    measurements.sort_by(|a, b| a.total_cmp(b));
    Ok(BenchmarkReport {
        requested_interval_ms: interval_ms,
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
                interval_ms: Some(1),
            },
            Point {
                x: 2,
                y: 0,
                interval_ms: Some(1),
            },
            Point {
                x: 3,
                y: 0,
                interval_ms: Some(30_000),
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
            interval_ms: None,
        }];
        let mut runner = Runner::with_emitter(
            Settings {
                points,
                interval_ms: 30_000,
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
                    interval_ms: None,
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
        runner.stop();
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
                        interval_ms: None,
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
