use super::*;
use std::{
    ffi::c_void,
    ptr::{null, null_mut},
    time::{Duration, Instant},
};
use windows_sys::{Win32::Networking::WinHttp::*, core::w};

struct Internet(*mut c_void);
impl Drop for Internet {
    fn drop(&mut self) {
        unsafe {
            WinHttpCloseHandle(self.0);
        }
    }
}
impl Internet {
    fn new(handle: *mut c_void) -> io::Result<Self> {
        if handle.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(handle))
        }
    }
}

fn checked(result: i32) -> io::Result<()> {
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Runs in a disposable worker. Its parent also enforces an overall wall-clock limit.
fn get(url: &str, limit: usize) -> io::Result<Vec<u8>> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| invalid("HTTPS is required"))?;
    let (host, path) = rest
        .split_once('/')
        .ok_or_else(|| invalid("Invalid HTTPS URL"))?;
    if host != "api.github.com" && host != "github.com" {
        return Err(invalid("Unexpected release host"));
    }
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    unsafe {
        let session = Internet::new(WinHttpOpen(
            w!("MultiPointClicker-Updater/1"),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            null(),
            null(),
            0,
        ))?;
        checked(WinHttpSetTimeouts(session.0, 1000, 1000, 1500, 1500))?;
        let connection = Internet::new(WinHttpConnect(session.0, wide(host).as_ptr(), 443, 0))?;
        let request = Internet::new(WinHttpOpenRequest(
            connection.0,
            w!("GET"),
            wide(&format!("/{path}")).as_ptr(),
            null(),
            null(),
            null(),
            WINHTTP_FLAG_SECURE,
        ))?;
        let redirects = WINHTTP_OPTION_REDIRECT_POLICY_DISALLOW_HTTPS_TO_HTTP;
        checked(WinHttpSetOption(
            request.0,
            WINHTTP_OPTION_REDIRECT_POLICY,
            (&redirects as *const u32).cast(),
            size_of::<u32>() as u32,
        ))?;
        let disabled = WINHTTP_DISABLE_AUTHENTICATION | WINHTTP_DISABLE_COOKIES;
        checked(WinHttpSetOption(
            request.0,
            WINHTTP_OPTION_DISABLE_FEATURE,
            (&disabled as *const u32).cast(),
            size_of::<u32>() as u32,
        ))?;
        let headers =
            wide("Accept: application/vnd.github+json\r\nX-GitHub-Api-Version: 2026-03-10\r\n");
        checked(WinHttpSendRequest(
            request.0,
            headers.as_ptr(),
            (headers.len() - 1) as u32,
            null(),
            0,
            0,
            0,
        ))?;
        checked(WinHttpReceiveResponse(request.0, null_mut()))?;
        let mut status = 0u32;
        let mut length = size_of::<u32>() as u32;
        checked(WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            null(),
            (&mut status as *mut u32).cast(),
            &mut length,
            null_mut(),
        ))?;
        if status != 200 {
            return Err(io::Error::other(format!(
                "Release server returned HTTP {status}"
            )));
        }
        let deadline = Instant::now() + Duration::from_secs(7);
        let mut body = Vec::new();
        loop {
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Release download timed out",
                ));
            }
            let mut chunk = [0u8; 32 * 1024];
            let mut count = 0;
            checked(WinHttpReadData(
                request.0,
                chunk.as_mut_ptr().cast(),
                chunk.len() as u32,
                &mut count,
            ))?;
            if count == 0 {
                break;
            }
            if body.len() + count as usize > limit {
                return Err(invalid("Release response is too large"));
            }
            body.extend_from_slice(&chunk[..count as usize]);
        }
        Ok(body)
    }
}

pub(super) fn check(root: &Path, report: &Path) -> io::Result<()> {
    let state = State::load(root)?;
    let bytes = get(
        &format!("https://api.github.com/repos/{REPOSITORY}/releases/latest"),
        1024 * 1024,
    )?;
    let release: Release = serde_json::from_slice(&bytes).map_err(|e| invalid(&e.to_string()))?;
    let (binary, asset) = release.candidate()?;
    if binary.version <= state.current.binary.version
        || state.rejected.as_ref() == Some(&binary.sha256)
    {
        return atomic_json(report, &Option::<Binary>::None);
    }
    if binary.verify(root).is_err() {
        let bytes = get(&asset.browser_download_url, MAX_BINARY)?;
        if bytes.len() != asset.size {
            return Err(invalid("Incomplete release download"));
        }
        stage_binary(root, &binary, &bytes)?;
    }
    atomic_json(report, &Some(binary))
}
