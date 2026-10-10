//! A single HTTPS GET through WinHTTP: the system TLS stack, certificate
//! store and proxy settings, with nothing bundled.

use super::wide;
use std::ffi::c_void;
use windows_sys::Win32::Networking::WinHttp::*;

pub struct Response {
    pub status: u32,
    pub body: Vec<u8>,
    /// Retry-After in seconds, 0 if absent.
    pub retry_after: u64,
}

/// Closes a WinHTTP handle when dropped.
struct Handle(*mut c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: handle came from WinHTTP and is closed exactly once.
            unsafe { WinHttpCloseHandle(self.0) };
        }
    }
}

fn check(h: *mut c_void) -> Result<Handle, ()> {
    if h.is_null() { Err(()) } else { Ok(Handle(h)) }
}

fn query_number(req: &Handle, info: u32) -> Option<u32> {
    let mut value = 0u32;
    let mut len = 4u32;
    // SAFETY: value/len describe a 4-byte buffer, as WINHTTP_QUERY_FLAG_NUMBER requires.
    let ok = unsafe {
        WinHttpQueryHeaders(
            req.0,
            info | WINHTTP_QUERY_FLAG_NUMBER,
            std::ptr::null(),
            (&raw mut value).cast(),
            &mut len,
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(value)
}

/// GETs an `https://` URL, following redirects. `headers` are "Name: value"
/// lines separated by CRLF. The body is cut off after `max_bytes`. Err means
/// a connection-level failure (DNS, TLS, timeout, offline) or a non-HTTPS URL.
pub fn get(url: &str, headers: &str, user_agent: &str, max_bytes: usize) -> Result<Response, ()> {
    let rest = url.strip_prefix("https://").ok_or(())?;
    let (host, path) = rest.find('/').map_or((rest, "/"), |i| (&rest[..i], &rest[i..]));
    let (ua, host, path, headers) = (wide(user_agent), wide(host), wide(path), wide(headers));
    // SAFETY: all strings are NUL-terminated and outlive the calls; every
    // handle is wrapped so it is closed on every path.
    unsafe {
        let mut session = WinHttpOpen(ua.as_ptr(), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, std::ptr::null(), std::ptr::null(), 0);
        if session.is_null() {
            // Automatic proxy needs Windows 8.1+.
            session = WinHttpOpen(ua.as_ptr(), WINHTTP_ACCESS_TYPE_DEFAULT_PROXY, std::ptr::null(), std::ptr::null(), 0);
        }
        let session = check(session)?;
        WinHttpSetTimeouts(session.0, 10_000, 10_000, 15_000, 15_000);
        let conn = check(WinHttpConnect(session.0, host.as_ptr(), INTERNET_DEFAULT_HTTPS_PORT, 0))?;
        let req = check(WinHttpOpenRequest(
            conn.0,
            windows_sys::w!("GET"),
            path.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ))?;
        if WinHttpSendRequest(req.0, headers.as_ptr(), u32::MAX, std::ptr::null(), 0, 0, 0) == 0
            || WinHttpReceiveResponse(req.0, std::ptr::null_mut()) == 0
        {
            return Err(());
        }
        let status = query_number(&req, WINHTTP_QUERY_STATUS_CODE).ok_or(())?;
        let retry_after = query_number(&req, WINHTTP_QUERY_RETRY_AFTER).unwrap_or(0).into();

        let mut body = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let mut read = 0u32;
            if WinHttpReadData(req.0, chunk.as_mut_ptr().cast(), chunk.len() as u32, &mut read) == 0 {
                return Err(());
            }
            if read == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..read as usize]);
            if body.len() > max_bytes {
                return Err(());
            }
        }
        // Wipe the bearer token from our copy of the headers.
        let mut headers = headers;
        headers.fill(0);
        Ok(Response { status, body, retry_after })
    }
}
