use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum ErrorCode {
    KeyMapping,
    KeyTarget,
    AreaMissing,
    AreaInvalid,
    InputMonitor,
    IntervalRange,
    IntervalFormat,
    PointLimit,
    Limits,
    Config,
    SettingsRead,
    SettingsJson,
    SettingsVersion,
    PointData,
    Coordinate,
    SettingsBackup,
    SettingsWrite,
    EmptyPoints,
    SignalCreate,
    TimerCreate,
    TimerSet,
    TimerWait,
    ThreadStart,
    WorkerPanic,
    ScreenUnavailable,
    PointOutside,
    ScreenChanged,
    InputSend,
    BenchmarkRange,
    Other,
}

#[derive(Clone, Debug, Serialize)]
pub struct ClickerError {
    pub code: ErrorCode,
    pub detail: Option<String>,
}
impl ClickerError {
    pub fn with_detail(code: ErrorCode, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: Some(detail.into()),
        }
    }
}
impl From<ErrorCode> for ClickerError {
    fn from(code: ErrorCode) -> Self {
        Self { code, detail: None }
    }
}
impl From<String> for ClickerError {
    fn from(detail: String) -> Self {
        Self::with_detail(ErrorCode::Other, detail)
    }
}
impl From<&str> for ClickerError {
    fn from(detail: &str) -> Self {
        detail.to_string().into()
    }
}
impl std::fmt::Display for ClickerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.code)?;
        if let Some(detail) = &self.detail {
            write!(f, ": {detail}")?;
        }
        Ok(())
    }
}
impl std::error::Error for ClickerError {}
