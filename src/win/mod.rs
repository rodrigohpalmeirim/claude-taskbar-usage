//! Windows-only code: the widget window, rendering and system services.

pub mod app;
mod cli;
mod http;
mod locale;
mod render;

/// UTF-16, NUL-terminated, for Win32 `PCWSTR` parameters.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

pub fn from_wide(buf: &[u16]) -> String {
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..n])
}
