use super::*;

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("clicker-update-{}", unique_id()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn binary(root: &Path, version: &str, bytes: &[u8]) -> Binary {
    let binary = Binary {
        version: Version::parse(version).unwrap(),
        sha256: digest(bytes),
        protocol: 1,
    };
    stage_binary(root, &binary, bytes).unwrap();
    binary
}

fn installed(root: &Path, version: &str, data: &str) -> Installation {
    let binary = binary(root, version, version.as_bytes());
    let pair = Installation {
        binary,
        data: unique_id(),
        settings_expected: true,
    };
    let path = pair.data_root(root).join("MultiPointClicker");
    fs::create_dir_all(&path).unwrap();
    fs::write(path.join("settings.json"), data).unwrap();
    pair
}

fn state(current: Installation, staged: Option<Binary>) -> State {
    State {
        format: 1,
        current,
        previous: None,
        staged,
        pending: None,
        rejected: None,
        confirmed_at: 0,
        timeout_candidate: None,
        timeout_failures: 0,
    }
}

#[derive(Default)]
struct FakeRuntime {
    starts: Vec<Installation>,
    fail: Option<Version>,
}
impl AppRuntime for FakeRuntime {
    type Session = Installation;
    fn start(&mut self, pair: &Installation, root: &Path) -> io::Result<Self::Session> {
        self.starts.push(pair.clone());
        if self.fail.as_ref() == Some(&pair.binary.version) {
            fs::write(
                pair.data_root(root).join("MultiPointClicker/settings.json"),
                b"migrated but incompatible",
            )?;
            return Err(io::Error::other(
                "Startup failed after changing application data",
            ));
        }
        Ok(pair.clone())
    }
    fn stop(&mut self, _: Self::Session) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn successful_update_keeps_the_old_executable_and_exact_data_pair() {
    let temp = Temp::new();
    let old = installed(&temp.0, "2.1.0", "original settings\r\n");
    fs::create_dir_all(old.data_root(&temp.0).join("MultiPointClicker/nested")).unwrap();
    fs::write(
        old.data_root(&temp.0)
            .join("MultiPointClicker/nested/backup"),
        b"backup bytes",
    )
    .unwrap();
    let new = binary(&temp.0, "2.2.0", b"new executable");
    let mut state = state(old.clone(), Some(new.clone()));
    state.save(&temp.0).unwrap();
    let launched = start(&temp.0, &mut state, &mut FakeRuntime::default()).unwrap();
    assert_eq!(launched.binary, new);
    assert_eq!(state.previous, Some(old.clone()));
    assert_ne!(launched.data, old.data);
    assert_eq!(
        fs::read(
            launched
                .data_root(&temp.0)
                .join("MultiPointClicker/nested/backup")
        )
        .unwrap(),
        b"backup bytes"
    );
    assert_eq!(
        fs::read(
            old.data_root(&temp.0)
                .join("MultiPointClicker/settings.json")
        )
        .unwrap(),
        b"original settings\r\n"
    );
    let saved = State::load(&temp.0).unwrap();
    assert_eq!(saved.current, launched);
    assert!(saved.pending.is_none());
}

#[test]
fn failed_update_immediately_launches_old_code_with_old_unmodified_data() {
    let temp = Temp::new();
    let old = installed(&temp.0, "2.1.0", "last working data");
    let new = binary(&temp.0, "2.2.0", b"broken new executable");
    let mut state = state(old.clone(), Some(new.clone()));
    state.save(&temp.0).unwrap();
    let mut runtime = FakeRuntime {
        fail: Some(new.version.clone()),
        ..Default::default()
    };
    let launched = start(&temp.0, &mut state, &mut runtime).unwrap();
    assert_eq!(launched, old);
    assert_eq!(runtime.starts.len(), 2);
    assert_ne!(runtime.starts[0].data, old.data);
    assert_eq!(runtime.starts[1], old);
    assert_eq!(
        fs::read(
            old.data_root(&temp.0)
                .join("MultiPointClicker/settings.json")
        )
        .unwrap(),
        b"last working data"
    );
    assert_eq!(state.rejected, Some(new.sha256));
    assert!(state.staged.is_none());
    let saved = State::load(&temp.0).unwrap();
    assert_eq!(saved.current, old);
    assert!(saved.pending.is_none());
}

#[test]
fn interrupted_trial_recovers_to_old_pair_and_does_not_retry_bad_release() {
    let temp = Temp::new();
    let old = installed(&temp.0, "2.1.0", "old");
    let new = installed(&temp.0, "2.2.0", "partially migrated");
    let mut state = state(old.clone(), Some(new.binary.clone()));
    state.pending = Some(Pending { trial: new.clone() });
    state.save(&temp.0).unwrap();
    let mut recovered = State::load(&temp.0).unwrap();
    assert!(recover(&temp.0, &mut recovered).unwrap());
    assert_eq!(recovered.rejected, Some(new.binary.sha256));
    let mut runtime = FakeRuntime::default();
    assert_eq!(start(&temp.0, &mut recovered, &mut runtime).unwrap(), old);
    assert_eq!(runtime.starts, [old]);
}

#[test]
fn corrupt_installed_executable_falls_back_to_previous_pair() {
    let temp = Temp::new();
    let old = installed(&temp.0, "2.1.0", "good old data");
    let current = installed(&temp.0, "2.2.0", "new data");
    let mut state = state(current.clone(), None);
    state.previous = Some(old.clone());
    state.save(&temp.0).unwrap();
    fs::write(current.binary.path(&temp.0), b"corrupt").unwrap();
    let mut runtime = FakeRuntime::default();
    assert_eq!(start(&temp.0, &mut state, &mut runtime).unwrap(), old);
    assert_eq!(runtime.starts, [old]);
}

#[test]
fn missing_current_data_falls_back_instead_of_resetting_settings() {
    let temp = Temp::new();
    let old = installed(&temp.0, "2.1.0", "old data");
    let current = installed(&temp.0, "2.2.0", "new data");
    fs::remove_dir_all(current.data_root(&temp.0)).unwrap();
    let mut state = state(current, None);
    state.previous = Some(old.clone());
    state.save(&temp.0).unwrap();
    assert_eq!(
        start(&temp.0, &mut state, &mut FakeRuntime::default()).unwrap(),
        old
    );
}

#[test]
fn missing_settings_file_or_inner_directory_restores_the_old_data_pair() {
    for remove_directory in [false, true] {
        let temp = Temp::new();
        let old = installed(&temp.0, "2.1.0", "original settings");
        let current = installed(&temp.0, "2.2.0", "new settings");
        let folder = current.data_root(&temp.0).join("MultiPointClicker");
        if remove_directory {
            fs::remove_dir_all(&folder).unwrap();
        } else {
            fs::remove_file(folder.join("settings.json")).unwrap();
        }
        let mut state = state(current, None);
        state.previous = Some(old.clone());
        state.save(&temp.0).unwrap();
        let launched = start(&temp.0, &mut state, &mut FakeRuntime::default()).unwrap();
        assert_eq!(launched, old);
        assert_eq!(
            fs::read(
                launched
                    .data_root(&temp.0)
                    .join("MultiPointClicker/settings.json")
            )
            .unwrap(),
            b"original settings"
        );
    }
}

#[test]
fn previously_saved_settings_are_not_silently_reset_when_no_backup_pair_exists() {
    let temp = Temp::new();
    let current = installed(&temp.0, "2.2.0", "last working data");
    fs::remove_file(
        current
            .data_root(&temp.0)
            .join("MultiPointClicker/settings.json"),
    )
    .unwrap();
    let mut state = state(current, None);
    state.save(&temp.0).unwrap();
    let mut runtime = FakeRuntime::default();
    assert!(start(&temp.0, &mut state, &mut runtime).is_err());
    assert!(runtime.starts.is_empty());
}

#[test]
fn transient_startup_timeout_is_retried_once_then_rejected() {
    struct TimeoutRuntime;
    impl AppRuntime for TimeoutRuntime {
        type Session = ();
        fn start(&mut self, pair: &Installation, _: &Path) -> io::Result<()> {
            if pair.binary.version == Version(2, 2, 0) {
                Err(io::Error::new(io::ErrorKind::TimedOut, "slow startup"))
            } else {
                Ok(())
            }
        }
        fn stop(&mut self, _: ()) -> io::Result<()> {
            Ok(())
        }
    }
    let temp = Temp::new();
    let old = installed(&temp.0, "2.1.0", "old");
    let candidate = binary(&temp.0, "2.2.0", b"new");
    let mut state = state(old.clone(), Some(candidate.clone()));
    state.save(&temp.0).unwrap();
    start(&temp.0, &mut state, &mut TimeoutRuntime).unwrap();
    assert!(state.rejected.is_none());
    assert!(state.staged.is_some());
    start(&temp.0, &mut state, &mut TimeoutRuntime).unwrap();
    assert_eq!(state.rejected, Some(candidate.sha256));
    assert!(state.staged.is_none());
    assert_eq!(state.current, old);
}

#[test]
fn rollback_grace_does_not_include_later_normal_use_or_clock_reversal() {
    assert!(startup_grace(1000, 1001));
    assert!(startup_grace(1000, 1060));
    assert!(!startup_grace(1000, 1061));
    assert!(!startup_grace(0, 10));
    assert!(!startup_grace(1000, 999));
}

#[test]
fn old_binary_cleanup_preserves_recovery_versions_all_data_and_unknown_files() {
    let temp = Temp::new();
    let obsolete = installed(&temp.0, "2.0.0", "retained user settings");
    let current = installed(&temp.0, "2.2.0", "current settings");
    let previous = installed(&temp.0, "2.1.0", "previous settings");
    let seed = binary(&temp.0, "2.0.1", b"original installer");
    atomic_json(
        &temp.0.join("bootstrap.json"),
        &Bootstrap {
            seed: seed.clone(),
            previous: None,
        },
    )
    .unwrap();
    let mut state = state(current.clone(), None);
    state.previous = Some(previous.clone());
    state.save(&temp.0).unwrap();
    let unknown = temp.0.join("versions/custom");
    fs::create_dir_all(&unknown).unwrap();
    fs::write(unknown.join("important"), b"keep").unwrap();
    clean_versions(&temp.0, &state);
    assert!(!obsolete.binary.path(&temp.0).exists());
    for binary in [&current.binary, &previous.binary, &seed] {
        assert!(binary.path(&temp.0).is_file());
    }
    assert_eq!(
        fs::read(
            obsolete
                .data_root(&temp.0)
                .join("MultiPointClicker/settings.json")
        )
        .unwrap(),
        b"retained user settings"
    );
    assert_eq!(fs::read(unknown.join("important")).unwrap(), b"keep");
}

#[test]
fn corrupt_state_can_read_the_previous_atomic_selector() {
    let temp = Temp::new();
    let old = installed(&temp.0, "2.1.0", "data");
    let mut state = state(old.clone(), None);
    state.save(&temp.0).unwrap();
    state.staged = Some(binary(&temp.0, "2.2.0", b"new"));
    state.save(&temp.0).unwrap();
    fs::write(temp.0.join("state.json"), b"{broken").unwrap();
    assert_eq!(State::load(&temp.0).unwrap().current, old);
}

#[test]
fn incomplete_or_wrong_download_does_not_publish_an_executable() {
    let temp = Temp::new();
    let binary = Binary {
        version: Version(2, 2, 0),
        sha256: digest(b"complete"),
        protocol: 1,
    };
    assert!(stage_binary(&temp.0, &binary, b"com").is_err());
    assert!(!binary.path(&temp.0).exists());
    stage_binary(&temp.0, &binary, b"complete").unwrap();
    assert_eq!(fs::read(binary.path(&temp.0)).unwrap(), b"complete");
    fs::write(binary.path(&temp.0), b"corrupt").unwrap();
    stage_binary(&temp.0, &binary, b"complete").unwrap();
    assert_eq!(fs::read(binary.path(&temp.0)).unwrap(), b"complete");
}

#[test]
fn first_start_imports_legacy_settings_without_altering_originals() {
    let temp = Temp::new();
    let legacy = temp.0.join("legacy");
    fs::create_dir_all(&legacy).unwrap();
    fs::write(legacy.join("settings.json"), b"legacy data").unwrap();
    fs::write(legacy.join("settings.v2-backup.json"), b"older data").unwrap();
    let old = binary(&temp.0, "2.1.0", b"old");
    let seed = binary(&temp.0, "2.2.0", b"seed");
    atomic_json(
        &temp.0.join("bootstrap.json"),
        &Bootstrap {
            seed: seed.clone(),
            previous: Some(old.clone()),
        },
    )
    .unwrap();
    let state = initialize(&temp.0, &legacy).unwrap();
    assert_eq!(state.current.binary, old);
    assert_eq!(state.staged, Some(seed));
    assert_eq!(
        fs::read(
            state
                .current
                .data_root(&temp.0)
                .join("MultiPointClicker/settings.json")
        )
        .unwrap(),
        b"legacy data"
    );
    assert_eq!(
        fs::read(legacy.join("settings.json")).unwrap(),
        b"legacy data"
    );
    assert_eq!(
        fs::read(legacy.join("settings.v2-backup.json")).unwrap(),
        b"older data"
    );
}

#[test]
fn only_expected_stable_release_asset_with_sha256_is_accepted() {
    let json = serde_json::json!({"tag_name":"v2.2.0", "draft":false,"prerelease":false,"assets":[{"name":ASSET_NAME,"size":123,"digest":format!("sha256:{}",digest(b"test")),"browser_download_url":format!("https://github.com/{REPOSITORY}/releases/download/v2.2.0/{ASSET_NAME}")}]});
    let release: Release = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(release.candidate().unwrap().0.version, Version(2, 2, 0));
    for (key, value) in [
        ("digest", serde_json::Value::Null),
        ("browser_download_url", "https://example.com/app.exe".into()),
        ("size", (MAX_BINARY + 1).into()),
    ] {
        let mut changed = json.clone();
        changed["assets"][0][key] = value;
        assert!(
            serde_json::from_value::<Release>(changed)
                .unwrap()
                .candidate()
                .is_err()
        );
    }
    let mut changed = json;
    changed["prerelease"] = true.into();
    assert!(
        serde_json::from_value::<Release>(changed)
            .unwrap()
            .candidate()
            .is_err()
    );
    for version in ["2.2", "2.2.0-beta", "../2.2.0", "02.2.0", "2.2.4294967296"] {
        assert!(Version::parse(version).is_err());
    }
}

#[cfg(windows)]
mod processes {
    use super::*;
    use crate::startup::{READY_PATH, READY_TOKEN, Ready};
    use std::{
        os::windows::process::CommandExt,
        process::{Child, Command, Stdio},
        time::Duration,
    };

    fn raw_fixture(
        pair: &Installation,
        root: &Path,
        behavior: &str,
    ) -> io::Result<(Child, PathBuf, String)> {
        let report = root.join(format!("ready-{}.json", unique_id()));
        let token = unique_id();
        let child = Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "updater::tests::processes::child_process_fixture",
                "--nocapture",
            ])
            .env("MPC_FIXTURE_BEHAVIOR", behavior)
            .env("MPC_FIXTURE_VERSION", pair.binary.version.to_string())
            .env("APPDATA", pair.data_root(root))
            .env(READY_PATH, &report)
            .env(READY_TOKEN, &token)
            .creation_flags(0x0800_0000)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        Ok((child, report, token))
    }
    fn fixture(
        pair: &Installation,
        root: &Path,
        behavior: &str,
        timeout: Duration,
    ) -> io::Result<Child> {
        let (child, report, token) = raw_fixture(pair, root, behavior)?;
        runtime::supervise(child, &report, &token, &pair.binary, timeout)
    }

    #[test]
    fn child_process_fixture() {
        let Ok(behavior) = std::env::var("MPC_FIXTURE_BEHAVIOR") else {
            return;
        };
        if behavior == "job-parent" {
            use crate::updater::process::{self, ObservedProcess};
            let root = PathBuf::from(std::env::var_os("APPDATA").unwrap());
            let exe = std::env::current_exe().unwrap();
            let child = process::spawn(
                &exe,
                &[
                    "--exact".into(),
                    "updater::tests::processes::child_process_fixture".into(),
                    "--nocapture".into(),
                ],
                exe.parent().unwrap(),
                &[("MPC_FIXTURE_BEHAVIOR".into(), Some("spawn-hang".into()))],
            )
            .unwrap();
            atomic_json(&root.join("job-child.json"), &child.id()).unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(15);
            while !root.join("exit-parent").exists() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            // Exit without destructors: the OS must close the job and kill both descendants.
            std::process::exit(23);
        }
        if behavior == "spawn-hang" {
            let root = PathBuf::from(std::env::var_os("APPDATA").unwrap());
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "updater::tests::processes::child_process_fixture",
                    "--nocapture",
                ])
                .env("MPC_FIXTURE_BEHAVIOR", "hang")
                .creation_flags(0x0800_0000)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            atomic_json(&root.join("job-grandchild.json"), &child.id()).unwrap();
            std::thread::sleep(Duration::from_secs(30));
            let _ = child.wait();
            return;
        }
        if behavior == "mutate-fail" {
            let path = PathBuf::from(std::env::var_os("APPDATA").unwrap())
                .join("MultiPointClicker/settings.json");
            fs::write(path, b"new incompatible data").unwrap();
            std::process::exit(17);
        }
        if behavior == "create-ready" {
            fs::write(
                PathBuf::from(std::env::var_os("APPDATA").unwrap())
                    .join("MultiPointClicker/settings.json"),
                b"fresh saved settings",
            )
            .unwrap();
        }
        if behavior != "hang" {
            let ready = Ready {
                token: if behavior == "bad-token" {
                    "wrong".into()
                } else {
                    std::env::var(READY_TOKEN).unwrap()
                },
                version: std::env::var("MPC_FIXTURE_VERSION").unwrap(),
                pid: std::process::id(),
            };
            atomic_json(Path::new(&std::env::var_os(READY_PATH).unwrap()), &ready).unwrap();
            if behavior == "ready-close" {
                std::process::exit(0);
            }
            if behavior == "ready-crash" {
                std::process::exit(19);
            }
        }
        std::thread::sleep(Duration::from_secs(30));
    }

    struct Processes {
        starts: Vec<Installation>,
        bad_version: Version,
        old_behavior: &'static str,
    }
    impl AppRuntime for Processes {
        type Session = Child;
        fn start(&mut self, pair: &Installation, root: &Path) -> io::Result<Child> {
            self.starts.push(pair.clone());
            fixture(
                pair,
                root,
                if pair.binary.version == self.bad_version {
                    "mutate-fail"
                } else {
                    self.old_behavior
                },
                Duration::from_secs(2),
            )
        }
        fn stop(&mut self, mut child: Child) -> io::Result<()> {
            if child.try_wait()?.is_none() {
                child.kill()?;
            }
            child.wait()?;
            Ok(())
        }
    }

    #[test]
    fn real_child_failure_after_data_migration_relaunches_old_data_immediately() {
        let temp = Temp::new();
        let old = installed(&temp.0, "2.1.0", "last working settings");
        let new = binary(&temp.0, "2.2.0", b"new app");
        let mut state = state(old.clone(), Some(new));
        state.save(&temp.0).unwrap();
        let mut runtime = Processes {
            starts: vec![],
            bad_version: Version(2, 2, 0),
            old_behavior: "ready-close",
        };
        let mut restored = start(&temp.0, &mut state, &mut runtime).unwrap();
        assert!(restored.wait().unwrap().success());
        assert_eq!(runtime.starts.len(), 2);
        assert_eq!(runtime.starts[1], old);
        assert_eq!(
            fs::read(
                old.data_root(&temp.0)
                    .join("MultiPointClicker/settings.json")
            )
            .unwrap(),
            b"last working settings"
        );
        assert_eq!(
            fs::read(
                runtime.starts[0]
                    .data_root(&temp.0)
                    .join("MultiPointClicker/settings.json")
            )
            .unwrap(),
            b"new incompatible data"
        );
    }

    #[test]
    fn hung_wrong_token_and_immediately_crashing_children_are_reaped() {
        let temp = Temp::new();
        let pair = installed(&temp.0, "2.2.0", "data");
        for behavior in ["hang", "bad-token", "ready-crash"] {
            let result = fixture(&pair, &temp.0, behavior, Duration::from_millis(400));
            assert!(result.is_err(), "{behavior}");
        }
        // A normal user close after reporting ready is a successful startup.
        let mut child = fixture(&pair, &temp.0, "ready-close", Duration::from_secs(2)).unwrap();
        assert!(child.wait().unwrap().success());
    }

    #[test]
    fn abnormal_fallback_and_missing_fallback_report_errors_without_loops() {
        for has_previous in [false, true] {
            let temp = Temp::new();
            let current = installed(&temp.0, "2.2.0", "new data");
            let mut state = state(current.clone(), None);
            state.confirmed_at = epoch_seconds();
            if has_previous {
                let mut old = installed(&temp.0, "2.1.0", "old data");
                old.binary.protocol = 0;
                state.previous = Some(old);
            }
            state.save(&temp.0).unwrap();
            let (child, _, _) = raw_fixture(&current, &temp.0, "ready-crash").unwrap();
            let mut runtime = Processes {
                starts: vec![],
                bad_version: Version(99, 0, 0),
                old_behavior: "ready-crash",
            };
            assert!(runtime::finish(&temp.0, &mut state, child, &mut runtime).is_err());
            assert_eq!(runtime.starts.len(), usize::from(has_previous));
        }
    }

    #[test]
    fn first_created_settings_are_recorded_before_a_crash_and_later_deletion() {
        let temp = Temp::new();
        let mut current = installed(&temp.0, "2.2.0", "temporary");
        let settings = current
            .data_root(&temp.0)
            .join("MultiPointClicker/settings.json");
        fs::remove_file(&settings).unwrap();
        current.settings_expected = false;
        let mut state = state(current, None);
        state.save(&temp.0).unwrap();
        let mut runtime = Processes {
            starts: vec![],
            bad_version: Version(99, 0, 0),
            old_behavior: "create-ready",
        };
        let mut child = start(&temp.0, &mut state, &mut runtime).unwrap();
        assert!(State::load(&temp.0).unwrap().current.settings_expected);
        child.kill().unwrap();
        assert!(runtime::finish(&temp.0, &mut state, child, &mut runtime).is_err());
        fs::remove_file(settings).unwrap();
        let mut reloaded = State::load(&temp.0).unwrap();
        assert!(start(&temp.0, &mut reloaded, &mut runtime).is_err());
        assert_eq!(runtime.starts.len(), 1);
    }

    #[test]
    fn normal_close_and_late_crash_keep_recent_settings_and_current_version() {
        for behavior in ["ready-close", "ready-crash"] {
            let temp = Temp::new();
            let current = installed(&temp.0, "2.2.0", "recent settings");
            let old = installed(&temp.0, "2.1.0", "old settings");
            let mut state = state(current.clone(), None);
            state.previous = Some(old);
            state.confirmed_at = epoch_seconds().saturating_sub(61);
            state.save(&temp.0).unwrap();
            let (child, _, _) = raw_fixture(&current, &temp.0, behavior).unwrap();
            let mut runtime = Processes {
                starts: vec![],
                bad_version: Version(99, 0, 0),
                old_behavior: "ready-close",
            };
            let result = runtime::finish(&temp.0, &mut state, child, &mut runtime);
            assert_eq!(result.is_ok(), behavior == "ready-close");
            assert!(runtime.starts.is_empty());
            assert_eq!(state.current, current);
            assert!(state.rejected.is_none());
            assert_eq!(
                fs::read(
                    current
                        .data_root(&temp.0)
                        .join("MultiPointClicker/settings.json")
                )
                .unwrap(),
                b"recent settings"
            );
        }
    }

    #[test]
    fn launcher_termination_kills_unconfirmed_application_and_its_descendant() {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use windows_sys::Win32::{
            Foundation::WAIT_OBJECT_0,
            System::Threading::{
                OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
                WaitForSingleObject,
            },
        };
        struct Parent(Child);
        impl Drop for Parent {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let temp = Temp::new();
        let pair = installed(&temp.0, "2.2.0", "data");
        let root = pair.data_root(&temp.0);
        let (child, _, _) = raw_fixture(&pair, &temp.0, "job-parent").unwrap();
        let mut parent = Parent(child);
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        while !root.join("job-grandchild.json").exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let handles: Vec<_> = ["job-child.json", "job-grandchild.json"]
            .into_iter()
            .map(|name| {
                let pid: u32 = serde_json::from_slice(&fs::read(root.join(name)).unwrap()).unwrap();
                let raw = unsafe {
                    OpenProcess(
                        PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                        0,
                        pid,
                    )
                };
                assert!(!raw.is_null());
                unsafe { OwnedHandle::from_raw_handle(raw) }
            })
            .collect();
        fs::write(root.join("exit-parent"), b"exit").unwrap();
        assert_eq!(parent.0.wait().unwrap().code(), Some(23));
        for handle in handles {
            assert_eq!(
                unsafe { WaitForSingleObject(handle.as_raw_handle(), 5000) },
                WAIT_OBJECT_0
            );
        }
    }
}
