#![windows_subsystem = "windows"]

fn main() {
    #[cfg(windows)]
    if let Err(error) = multi_point_clicker::updater::runtime::entry() {
        use windows_sys::{Win32::UI::WindowsAndMessaging::*, core::w};
        // Worker failures are silent; the parent opens the installed version.
        if std::env::args_os().nth(1).is_none() {
            let message = format!("アプリを起動できませんでした。\n\n{error}");
            let wide: Vec<_> = message.encode_utf16().chain(Some(0)).collect();
            unsafe {
                MessageBoxW(
                    std::ptr::null_mut(),
                    wide.as_ptr(),
                    w!("Multi Point Clicker"),
                    MB_OK | MB_ICONERROR,
                );
            }
        }
        std::process::exit(1);
    }
    #[cfg(not(windows))]
    eprintln!("Multi Point Clicker requires Windows.");
}
