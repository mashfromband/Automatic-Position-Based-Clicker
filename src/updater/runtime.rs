use super::process::{self, ObservedProcess, Session};
use super::*;
use crate::startup::{READY_PATH, READY_TOKEN, Ready};
use std::{
    os::windows::{fs::OpenOptionsExt, process::CommandExt},
    process::{Command, Stdio},
    ptr::null,
    thread,
    time::{Duration, Instant},
};
use windows_sys::{
    Win32::{
        Foundation::CloseHandle,
        System::Threading::{OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE},
        UI::WindowsAndMessaging::{FindWindowW, SW_RESTORE, SetForegroundWindow, ShowWindow},
    },
    core::w,
};

pub const LAUNCHER_NAME: &str = "Multi Point Clicker.exe";
const NO_WINDOW: u32 = 0x0800_0000;

pub fn root() -> io::Result<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(|p| PathBuf::from(p).join("MultiPointClicker"))
        .ok_or_else(|| invalid("LOCALAPPDATA is unavailable"))
}

fn command(exe: &Path) -> Command {
    let mut command = Command::new(exe);
    command
        .creation_flags(NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

pub fn existing_instance() -> bool {
    unsafe {
        let mutex = OpenMutexW(
            SYNCHRONIZATION_SYNCHRONIZE,
            0,
            w!("Local\\MultiPointClickerRust"),
        );
        if mutex.is_null() {
            return false;
        }
        CloseHandle(mutex);
        let hwnd = FindWindowW(w!("MultiPointClickerRust"), null());
        if !hwnd.is_null() {
            ShowWindow(hwnd, SW_RESTORE);
            SetForegroundWindow(hwnd);
        }
        true
    }
}

pub struct Runtime;
impl AppRuntime for Runtime {
    type Session = Session;
    fn start(&mut self, pair: &Installation, root: &Path) -> io::Result<Session> {
        let attempt = root.join("runs").join(unique_id());
        fs::create_dir_all(&attempt)?;
        let report = attempt.join("ready.json");
        let token = unique_id();
        let mut environment = vec![
            (
                "APPDATA".into(),
                Some(pair.data_root(root).into_os_string()),
            ),
            (READY_PATH.into(), None),
            (READY_TOKEN.into(), None),
        ];
        if pair.binary.protocol == 1 {
            environment[1].1 = Some(report.clone().into_os_string());
            environment[2].1 = Some(token.clone().into());
        }
        let executable = pair.binary.path(root);
        let result = process::spawn(
            &executable,
            &[],
            executable
                .parent()
                .ok_or_else(|| invalid("Missing executable directory"))?,
            &environment,
        )
        .and_then(|child| supervise(child, &report, &token, &pair.binary, Duration::from_secs(8)));
        if match &result {
            Ok(_) => true,
            Err(e) => !is_cleanup_error(e),
        } {
            let _ = fs::remove_file(report);
            let _ = fs::remove_dir(attempt);
        }
        result
    }
    fn stop(&mut self, mut child: Session) -> io::Result<()> {
        child.kill()?;
        child.wait()?;
        Ok(())
    }
}

/// Owns the child until ready; every error path terminates and reaps it.
pub fn supervise<P: ObservedProcess>(
    mut child: P,
    report: &Path,
    token: &str,
    binary: &Binary,
    timeout: Duration,
) -> io::Result<P> {
    let result = (|| {
        if binary.protocol == 0 {
            return Ok(());
        } // The explicitly imported, previously working legacy app.
        let deadline = Instant::now() + timeout;
        let mut ready_since = None;
        loop {
            let ready = fs::read(report)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Ready>(&bytes).ok())
                .is_some_and(|r| {
                    r.token == token
                        && r.pid == child.id()
                        && r.version == binary.version.to_string()
                });
            if let Some(status) = child.try_wait()? {
                if status.code() == Some(10) {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "Application is already running",
                    ));
                }
                return if ready && status.success() {
                    Ok(())
                } else {
                    Err(io::Error::other(format!(
                        "Application exited before startup completed ({status})"
                    )))
                };
            }
            if ready {
                let since = *ready_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= Duration::from_millis(250) {
                    return Ok(());
                }
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Application startup timed out",
                ));
            }
            thread::sleep(Duration::from_millis(20));
        }
    })();
    match result {
        Ok(()) => Ok(child),
        Err(error) => {
            child.kill().map_err(cleanup_error)?;
            child.wait().map_err(cleanup_error)?;
            Err(error)
        }
    }
}

pub fn install(
    root: &Path,
    seed_path: &Path,
    previous: Option<(&Path, Version)>,
) -> io::Result<()> {
    fs::create_dir_all(root)?;
    let _lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(root.join("update.lock"))?;
    let bytes = fs::read(seed_path)?;
    let seed = Binary {
        version: Version::parse(env!("CARGO_PKG_VERSION"))?,
        sha256: digest(&bytes),
        protocol: 1,
    };
    stage_binary(root, &seed, &bytes)?;
    let previous = if let Some((path, version)) = previous {
        let bytes = fs::read(path)?;
        let binary = Binary {
            version,
            sha256: digest(&bytes),
            protocol: 0,
        };
        stage_binary(root, &binary, &bytes)?;
        Some(binary)
    } else {
        None
    };
    if !root.join("bootstrap.json").exists() {
        atomic_json(
            &root.join("bootstrap.json"),
            &Bootstrap {
                seed: seed.clone(),
                previous,
            },
        )?;
    }
    if root.join("state.json").exists() {
        let mut state = State::load(root)?;
        if seed.version > state.current.binary.version {
            state.staged = Some(seed);
            state.save(root)?;
        }
    }
    let myself = std::env::current_exe()?;
    let destination = root.join(LAUNCHER_NAME);
    if !destination.exists() {
        atomic_bytes(&destination, &fs::read(myself)?)?;
    }
    Ok(())
}

fn check_update(root: &Path, state: &mut State) {
    let report_dir = root.join("checks").join(unique_id());
    let report = report_dir.join("result.json");
    let result = (|| {
        fs::create_dir_all(&report_dir)?;
        let mut child = command(&std::env::current_exe()?)
            .arg("--check")
            .arg(root)
            .arg(&report)
            .spawn()?;
        let mut deadline = Instant::now() + Duration::from_secs(4);
        let progress = report.with_extension("download.json");
        let mut downloading = false;
        loop {
            if !downloading && progress.is_file() {
                deadline = Instant::now() + Duration::from_secs(15);
                downloading = true;
            }
            match child.try_wait() {
                Ok(Some(status)) if status.success() => break,
                Ok(Some(_)) => return Err(io::Error::other("Could not check the latest release")),
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
                _ => {
                    child.kill()?;
                    child.wait()?;
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "Update check timed out",
                    ));
                }
            }
        }
        let candidate: Option<Binary> =
            serde_json::from_slice(&fs::read(&report)?).map_err(|e| invalid(&e.to_string()))?;
        if let Some(binary) = candidate {
            binary.verify(root)?;
            if binary.protocol != 1
                || binary.version <= state.current.binary.version
                || state.rejected.as_ref() == Some(&binary.sha256)
            {
                return Err(invalid("Unexpected update candidate"));
            }
            state.staged = Some(binary);
            state.save(root)?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        log(root, "using_installed_version", &error.to_string());
    }
    let _ = fs::remove_file(report);
    let _ = fs::remove_file(report_dir.join("result.download.json"));
    let _ = fs::remove_dir(report_dir);
}

pub fn launch(root: &Path) -> io::Result<()> {
    if existing_instance() {
        return Ok(());
    }
    fs::create_dir_all(root)?;
    let _lock = match OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(root.join("update.lock"))
    {
        Ok(lock) => lock,
        Err(error) if error.raw_os_error() == Some(32) => return Ok(()),
        Err(error) => return Err(error),
    };
    let state_result =
        if root.join("state.json").exists() || root.join("state-backup.json").exists() {
            State::load(root)
        } else {
            let legacy = std::env::var_os("APPDATA")
                .map(PathBuf::from)
                .ok_or_else(|| invalid("APPDATA is unavailable"))?
                .join("MultiPointClicker");
            initialize(root, &legacy)
        };
    let mut state = match state_result {
        Ok(state) => state,
        Err(error) => return emergency_previous(root, error),
    };
    let recovered = recover(root, &mut state)?;
    if !recovered {
        check_update(root, &mut state);
    }
    let mut runtime = Runtime;
    let child = match start(root, &mut state, &mut runtime) {
        Ok(child) => child,
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
        Err(error) => return Err(error),
    };
    clean_versions(root, &state);
    finish(root, &mut state, child, &mut runtime)
}

pub fn finish<R: AppRuntime>(
    root: &Path,
    state: &mut State,
    mut child: R::Session,
    runtime: &mut R,
) -> io::Result<()>
where
    R::Session: ObservedProcess,
{
    let status = child.wait();
    runtime.stop(child).map_err(cleanup_error)?;
    let status = status?;
    if !status.success() {
        if state.previous.is_none() || !startup_grace(state.confirmed_at, epoch_seconds()) {
            return Err(io::Error::other(format!(
                "Application exited abnormally ({status})"
            )));
        }
        log(
            root,
            "rolled_back",
            &format!("Application crashed ({status})"),
        );
        state.rejected = Some(state.current.binary.sha256.clone());
        state.current = state
            .previous
            .take()
            .ok_or_else(|| invalid("Previous application missing"))?;
        state.staged = None;
        state.pending = None;
        state.confirmed_at = 0;
        state.save(root)?;
        let mut restored = start(root, state, runtime)?;
        let status = restored.wait();
        runtime.stop(restored).map_err(cleanup_error)?;
        let status = status?;
        if !status.success() {
            return Err(io::Error::other(format!(
                "Previous application exited abnormally ({status})"
            )));
        }
    }
    if !state.current.settings_expected
        && state
            .current
            .data_root(root)
            .join("MultiPointClicker/settings.json")
            .is_file()
    {
        state.current.settings_expected = true;
        state.save(root)?;
    }
    Ok(())
}

fn emergency_previous(root: &Path, cause: io::Error) -> io::Result<()> {
    log(root, "using_original_version", &cause.to_string());
    let setup: Bootstrap = serde_json::from_slice(&fs::read(root.join("bootstrap.json"))?)
        .map_err(|e| invalid(&e.to_string()))?;
    let Some(binary) = setup.previous else {
        return Err(cause);
    };
    if binary.protocol != 0 {
        return Err(cause);
    }
    binary.verify(root)?;
    let executable = binary.path(root);
    let mut session = process::spawn(
        &executable,
        &[],
        executable
            .parent()
            .ok_or_else(|| invalid("Missing previous application"))?,
        &[(READY_PATH.into(), None), (READY_TOKEN.into(), None)],
    )?;
    let status = session.wait();
    session.kill().map_err(cleanup_error)?;
    let status = status?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "Original application exited abnormally ({status})"
        )));
    }
    Ok(())
}

pub fn entry() -> io::Result<()> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.get(1).is_some_and(|s| s == "--check") {
        if args.len() != 4 {
            return Err(invalid("Invalid update worker arguments"));
        }
        return super::http::check(Path::new(&args[2]), Path::new(&args[3]));
    }
    let install_root = root()?;
    if args.get(1).is_some_and(|s| s == "--install") {
        if args.len() != 3 && args.len() != 5 {
            return Err(invalid("Invalid installation arguments"));
        }
        let previous = if args.len() == 5 {
            Some((
                Path::new(&args[3]),
                Version::parse(&args[4].to_string_lossy())?,
            ))
        } else {
            None
        };
        return install(&install_root, Path::new(&args[2]), previous);
    }
    if !install_root.join("bootstrap.json").exists() {
        let seed = std::env::current_exe()?
            .parent()
            .ok_or_else(|| invalid("Missing launcher directory"))?
            .join("multi-point-clicker.exe");
        install(&install_root, &seed, None)?;
    }
    launch(&install_root)
}
