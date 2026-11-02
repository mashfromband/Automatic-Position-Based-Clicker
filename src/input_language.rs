//! App-lifetime reservations for Windows' Alt+Shift input-language shortcut.
use std::{io, marker::PhantomData, ptr::null_mut, rc::Rc};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    MOD_ALT, MOD_NOREPEAT, MOD_SHIFT, RegisterHotKey, UnregisterHotKey, VK_MENU, VK_SHIFT,
};

const HOTKEYS: [(i32, u32, u32); 2] = [
    (0x4d10, MOD_ALT | MOD_NOREPEAT, VK_SHIFT as u32),
    (0x4d11, MOD_SHIFT | MOD_NOREPEAT, VK_MENU as u32),
];
struct Hotkey {
    id: i32,
    _thread: PhantomData<Rc<()>>,
}
impl Hotkey {
    fn register((id, modifiers, key): (i32, u32, u32)) -> io::Result<Self> {
        if unsafe { RegisterHotKey(null_mut(), id, modifiers, key) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            id,
            _thread: PhantomData,
        })
    }
}
impl Drop for Hotkey {
    fn drop(&mut self) {
        unsafe {
            UnregisterHotKey(null_mut(), self.id);
        }
    }
}
/// Owned by the UI thread throughout the message loop, including when idle.
/// Windows owns these registrations and releases them when their thread exits.
/// No saved language preference or keyboard/mouse input is modified.
pub struct InputLanguageGuard {
    hotkeys: [Hotkey; 2],
}
impl InputLanguageGuard {
    pub fn register() -> io::Result<Self> {
        Ok(Self {
            hotkeys: [Hotkey::register(HOTKEYS[0])?, Hotkey::register(HOTKEYS[1])?],
        })
    }
    pub fn owns_hotkey(&self, id: usize) -> bool {
        self.hotkeys.iter().any(|hotkey| hotkey.id as usize == id)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        panic::{AssertUnwindSafe, catch_unwind},
        sync::{
            atomic::{AtomicU64, Ordering},
            mpsc,
        },
        thread,
    };
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::MOD_CONTROL;
    use windows_sys::Win32::{
        Foundation::{
            CloseHandle, ERROR_HOTKEY_ALREADY_REGISTERED, GENERIC_ALL, HANDLE, WAIT_ABANDONED,
            WAIT_OBJECT_0,
        },
        System::{
            StationsAndDesktops::*,
            Threading::{CreateMutexW, GetCurrentThreadId, ReleaseMutex, WaitForSingleObject},
        },
    };
    use windows_sys::core::w;

    struct RegistrationLock(HANDLE);
    impl RegistrationLock {
        fn acquire() -> Self {
            let handle = unsafe {
                CreateMutexW(
                    null_mut(),
                    0,
                    w!("Local\\MultiPointClickerInputLanguageTests"),
                )
            };
            assert!(!handle.is_null(), "{}", io::Error::last_os_error());
            let result = unsafe { WaitForSingleObject(handle, 10_000) };
            if !matches!(result, WAIT_OBJECT_0 | WAIT_ABANDONED) {
                unsafe {
                    CloseHandle(handle);
                }
                panic!("Cannot acquire the input-language test mutex: {result}");
            }
            Self(handle)
        }
    }
    impl Drop for RegistrationLock {
        fn drop(&mut self) {
            unsafe {
                ReleaseMutex(self.0);
                CloseHandle(self.0);
            }
        }
    }

    struct Desktop {
        current: HDESK,
        original: HDESK,
    }
    static DESKTOP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
    impl Desktop {
        fn new() -> Self {
            let name: Vec<_> = format!(
                "MultiPointClickerLanguageTest-{}-{}",
                std::process::id(),
                DESKTOP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            )
            .encode_utf16()
            .chain(Some(0))
            .collect();
            let original = unsafe { GetThreadDesktop(GetCurrentThreadId()) };
            let current = unsafe {
                CreateDesktopW(
                    name.as_ptr(),
                    null_mut(),
                    null_mut(),
                    0,
                    GENERIC_ALL,
                    null_mut(),
                )
            };
            assert!(!current.is_null(), "{}", io::Error::last_os_error());
            let desktop = Self { current, original };
            assert_ne!(
                unsafe { SetThreadDesktop(current) },
                0,
                "{}",
                io::Error::last_os_error()
            );
            desktop
        }
        fn on_other_thread<T: Send + 'static>(
            &self,
            task: impl FnOnce() -> T + Send + 'static,
        ) -> T {
            let desktop = self.current as usize;
            thread::spawn(move || {
                assert_ne!(unsafe { SetThreadDesktop(desktop as HDESK) }, 0);
                task()
            })
            .join()
            .unwrap()
        }
        fn probe(&self, index: usize) -> Result<(), Option<i32>> {
            self.on_other_thread(move || {
                Hotkey::register(HOTKEYS[index])
                    .map(|_| ())
                    .map_err(|error| error.raw_os_error())
            })
        }
    }
    impl Drop for Desktop {
        fn drop(&mut self) {
            unsafe {
                SetThreadDesktop(self.original);
                CloseDesktop(self.current);
            }
        }
    }
    fn isolated(test: impl FnOnce() + Send + 'static) {
        thread::spawn(move || {
            let _lock = RegistrationLock::acquire();
            test();
        })
        .join()
        .unwrap();
    }

    #[test]
    fn reservations_and_drop_are_verified_on_a_separate_noninteractive_desktop() {
        isolated(|| {
            let desktop = Desktop::new();
            let guard = InputLanguageGuard::register().unwrap();
            for index in 0..2 {
                assert_eq!(
                    desktop.probe(index),
                    Err(Some(ERROR_HOTKEY_ALREADY_REGISTERED as i32))
                );
            }
            assert!(guard.owns_hotkey(HOTKEYS[0].0 as usize));
            assert!(guard.owns_hotkey(HOTKEYS[1].0 as usize));
            for id in [0, 1, 2, 3, 4, 5, usize::MAX] {
                assert!(!guard.owns_hotkey(id));
            }
            drop(guard);
            for index in 0..2 {
                assert_eq!(desktop.probe(index), Ok(()));
            }
        });
    }
    #[test]
    fn existing_operation_hotkeys_remain_available() {
        isolated(|| {
            let _desktop = Desktop::new();
            let _guard = InputLanguageGuard::register().unwrap();
            for (id, mods, key) in [
                (20, MOD_ALT, 0x20),
                (21, MOD_ALT, 0x58),
                (22, MOD_ALT | MOD_CONTROL, 0x53),
                (23, MOD_ALT | MOD_CONTROL, 0x58),
                (24, MOD_ALT | MOD_CONTROL, 0x4c),
            ] {
                drop(Hotkey::register((id, mods | MOD_NOREPEAT, key)).unwrap());
            }
        });
    }
    #[test]
    fn partial_registration_failure_restores_the_first_reservation() {
        isolated(|| {
            let desktop = Desktop::new();
            let (ready_tx, ready_rx) = mpsc::sync_channel(1);
            let (release_tx, release_rx) = mpsc::channel::<()>();
            let other_desktop = desktop.current as usize;
            let worker = thread::spawn(move || {
                assert_ne!(unsafe { SetThreadDesktop(other_desktop as HDESK) }, 0);
                let held = Hotkey::register(HOTKEYS[1]);
                let result = held
                    .as_ref()
                    .map(|_| ())
                    .map_err(|error| error.raw_os_error());
                ready_tx.send(result).unwrap();
                let _ = release_rx.recv();
                drop(held);
            });
            let result = catch_unwind(AssertUnwindSafe(|| {
                assert_eq!(ready_rx.recv().unwrap(), Ok(()));
                assert_eq!(
                    InputLanguageGuard::register().err().unwrap().raw_os_error(),
                    Some(ERROR_HOTKEY_ALREADY_REGISTERED as i32)
                );
                assert_eq!(desktop.probe(0), Ok(()));
                assert_eq!(
                    desktop.probe(1),
                    Err(Some(ERROR_HOTKEY_ALREADY_REGISTERED as i32))
                );
            }));
            drop(release_tx);
            worker.join().unwrap();
            if let Err(panic) = result {
                std::panic::resume_unwind(panic);
            }
            assert_eq!(desktop.probe(1), Ok(()));
        });
    }
    #[test]
    fn thread_exit_releases_reservations_without_running_the_guard_destructor() {
        isolated(|| {
            let desktop = Desktop::new();
            desktop.on_other_thread(|| std::mem::forget(InputLanguageGuard::register().unwrap()));
            for index in 0..2 {
                assert_eq!(desktop.probe(index), Ok(()));
            }
        });
    }
}
