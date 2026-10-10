//! The widget: a layered child window of the taskbar (`Shell_TrayWnd`) placed
//! left of the tray icons, plus a hidden top-level window for timers, menus
//! and broadcasts. Data is fetched on a background thread; the UI thread only
//! paints, so it never blocks Explorer (whose input queue it shares).

use super::locale::WinLocale;
use super::render::{Layout, Renderer, View, format_percent};
use super::{cli, http, updater, wide};
use crate::config::{Config, TOGGLES, toggle_locked, toggle_value};
use crate::timefmt::{self, Locale};
use crate::update::Release;
use crate::usage::{FetchError, Usage, demo_usage, parse_usage};
use std::cell::RefCell;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Registry::*;
use windows_sys::Win32::System::SystemInformation::GetTickCount;
use windows_sys::Win32::System::Threading::{CreateEventW, CreateMutexW, SetEvent, WaitForSingleObject};
use windows_sys::Win32::UI::Controls::*;
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetLastInputInfo, LASTINPUTINFO, ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;
use windows_sys::w;

const APP_NAME: &str = "Claude Taskbar Usage";
const VERSION: &str = updater::VERSION;
const CLASS: windows_sys::core::PCWSTR = w!("ClaudeTaskbarUsageWidget");
const RUN_KEY: windows_sys::core::PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const RUN_VALUE: windows_sys::core::PCWSTR = w!("ClaudeTaskbarUsage");
const USAGE_PAGE: &str = "https://claude.ai/settings/usage";
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";

const WM_APP_DATA: u32 = WM_APP + 1;
/// An update install finished (successfully or not).
const WM_APP_UPDATED: u32 = WM_APP + 2;
const TIMER_HOUSEKEEPING: usize = 1;

const CMD_REFRESH: u32 = 1;
const CMD_OPEN_USAGE: u32 = 2;
const CMD_AUTOSTART: u32 = 3;
const CMD_RESET_POS: u32 = 4;
const CMD_SETTINGS: u32 = 5;
const CMD_EXIT: u32 = 6;
const CMD_UPDATE: u32 = 7;
const CMD_CHECK_UPDATES: u32 = 8;
const CMD_TOGGLE_BASE: u32 = 100;

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// State shared with the polling and update threads.
#[derive(Default)]
struct Shared {
    usage: Option<Usage>,
    last_ok: Option<i64>,
    last_err: Option<String>,
    last_short: &'static str,
    last_attempt: i64,
    version: u64,
    /// Mirrors `Config::check_updates` for the update thread.
    check_updates: bool,
    /// When the update thread checks next (ms); 0 = as soon as possible.
    next_update_check: i64,
    /// A newer release, if one was found.
    update: Option<Release>,
    /// An install is downloading.
    updating: bool,
    /// Result of the last install, picked up on WM_APP_UPDATED.
    update_result: Option<Result<PathBuf, String>>,
}

struct App {
    cfg: Config,
    demo: bool,
    /// Fallback: topmost popup over the taskbar instead of a child window.
    overlay: bool,
    hinst: HINSTANCE,
    host: HWND,
    widget: HWND,
    tooltip: HWND,
    tray: HWND,
    rend: Renderer,
    locale: WinLocale,
    hover: bool,
    tracking: bool,
    last_key: String,
    last_rect: RECT,
    tip_text: String,
    drag: Drag,
    shared: Arc<Mutex<Shared>>,
    /// Auto-reset event that wakes the poller for a manual refresh.
    refresh_event: HANDLE,
}

#[derive(Default)]
struct Drag {
    active: bool,
    moved: bool,
    start_x: i32,
    start_offset: i32,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Runs `f` on the app unless it is already borrowed further up the stack:
/// Win32 calls like SetWindowPos or DestroyWindow send messages back into
/// the window procedure synchronously, and those re-entrant messages simply
/// go to DefWindowProc.
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|cell| cell.try_borrow_mut().ok().and_then(|mut app| app.as_mut().map(f)))
}

pub fn run() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let demo = args.iter().any(|a| a == "--demo");
    let overlay = args.iter().any(|a| a == "--overlay");

    // After a self-update the previous version may still be shutting down,
    // so wait a little for it to release the single-instance mutex.
    let attempts = if args.iter().any(|a| a == "--after-update") { 40 } else { 1 };
    if !single_instance(attempts) {
        return;
    }

    // SAFETY: plain Win32 calls; every pointer passed is valid for the call.
    unsafe {
        log::init();
        log::write(&format!("started {VERSION} (demo={demo} overlay={overlay})"));

        let icc = INITCOMMONCONTROLSEX { dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32, dwICC: ICC_WIN95_CLASSES };
        InitCommonControlsEx(&icc);

        let hinst: HINSTANCE = GetModuleHandleW(std::ptr::null());
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            lpszClassName: CLASS,
            ..std::mem::zeroed()
        };
        if RegisterClassExW(&wc) == 0 {
            log::write("RegisterClassEx failed");
            return;
        }
        let host = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            CLASS,
            w!("Claude Taskbar Usage"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinst,
            std::ptr::null(),
        );
        if host.is_null() {
            log::write("CreateWindowEx(host) failed");
            return;
        }
        SetTimer(host, TIMER_HOUSEKEEPING, 1000, None);

        let cfg = Config::load();
        let shared = Arc::new(Mutex::new(Shared { check_updates: cfg.check_updates, ..Shared::default() }));
        let refresh_event = CreateEventW(std::ptr::null(), 0, 0, std::ptr::null());
        spawn_poller(shared.clone(), host as isize, refresh_event as isize, demo, &cfg);
        spawn_updater(shared.clone(), host as isize);

        let app = App {
            cfg,
            demo,
            overlay,
            hinst,
            host,
            widget: std::ptr::null_mut(),
            tooltip: std::ptr::null_mut(),
            tray: std::ptr::null_mut(),
            rend: Renderer::new(),
            locale: WinLocale,
            hover: false,
            tracking: false,
            last_key: String::new(),
            last_rect: RECT { left: 0, top: 0, right: 0, bottom: 0 },
            tip_text: String::new(),
            drag: Drag::default(),
            shared,
            refresh_event,
        };
        APP.with(|cell| *cell.borrow_mut() = Some(app));
        with_app(App::housekeeping);

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Holds the named mutex for the life of the process; false if another
/// instance still has it after `attempts` tries 250 ms apart.
fn single_instance(attempts: u32) -> bool {
    for i in 0..attempts {
        if i > 0 {
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        // SAFETY: plain mutex creation; the handle is intentionally kept open.
        unsafe {
            let mutex = CreateMutexW(std::ptr::null(), 0, w!(r"Local\ClaudeTaskbarUsage"));
            if mutex.is_null() || GetLastError() != ERROR_ALREADY_EXISTS {
                return true;
            }
            CloseHandle(mutex);
        }
    }
    false
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_RBUTTONUP && with_app(|a| a.widget == hwnd) == Some(true) {
        show_menu();
        return 0;
    }
    if msg == WM_APP_UPDATED {
        finish_update();
        return 0;
    }
    if let Some(Some(r)) = with_app(|a| a.handle(hwnd, msg)) {
        return r;
    }
    // SAFETY: forwarding the original arguments.
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

/// The context menu runs a modal loop, so it's shown without holding the
/// app borrow (timers keep firing meanwhile).
fn show_menu() {
    let Some((menu, host)) = with_app(|a| (a.build_menu(), a.host)) else { return };
    // SAFETY: menu and host are live handles owned by this thread.
    let cmd = unsafe {
        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        SetForegroundWindow(host);
        let cmd =
            TrackPopupMenu(menu, TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_BOTTOMALIGN | TPM_RIGHTALIGN, pt.x, pt.y, 0, host, std::ptr::null());
        PostMessageW(host, WM_NULL, 0, 0);
        DestroyMenu(menu); // also destroys the submenu
        cmd as u32
    };
    with_app(|a| a.on_command(cmd));
}

/// Restarts into the new version, or explains why the update failed. Runs
/// outside the app borrow because MessageBox has its own modal loop.
fn finish_update() {
    let Some((result, page)) = with_app(|a| {
        let mut s = a.shared.lock().unwrap_or_else(|e| e.into_inner());
        s.updating = false;
        let page = s.update.as_ref().map(|r| r.page_url.clone()).unwrap_or_default();
        (s.update_result.take(), page)
    }) else {
        return;
    };
    match result {
        Some(Ok(exe)) => {
            log::write("update installed, restarting");
            if std::process::Command::new(&exe).arg("--after-update").spawn().is_ok() {
                with_app(App::quit);
            }
        }
        Some(Err(e)) => {
            log::write(&format!("update failed: {e}"));
            let text = wide(&format!("Couldn't install the update: {e}.\n\nThe release page will open so you can download it manually."));
            // SAFETY: NUL-terminated strings valid for the call.
            unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), w!("Claude Taskbar Usage"), MB_OK | MB_ICONWARNING) };
            if !page.is_empty() {
                open(page.as_ref());
            }
        }
        None => {}
    }
    with_app(|a| a.refresh_view(true));
}

fn cursor_x() -> i32 {
    let mut pt = POINT { x: 0, y: 0 };
    // SAFETY: valid out pointer.
    unsafe { GetCursorPos(&mut pt) };
    pt.x
}

impl App {
    fn handle(&mut self, hwnd: HWND, msg: u32) -> Option<LRESULT> {
        if hwnd == self.host {
            match msg {
                WM_TIMER => self.housekeeping(),
                WM_APP_DATA => self.refresh_view(true),
                WM_SETTINGCHANGE => {
                    self.refresh_view(false);
                    return None;
                }
                WM_CLOSE => self.quit(),
                _ => return None,
            }
            return Some(0);
        }
        if hwnd.is_null() || hwnd != self.widget {
            return None;
        }
        match msg {
            WM_MOUSEMOVE => {
                if !self.tracking {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: self.widget,
                        dwHoverTime: 0,
                    };
                    // SAFETY: valid struct for our own window.
                    unsafe { TrackMouseEvent(&mut tme) };
                    self.tracking = true;
                    self.hover = true;
                    self.refresh_view(false);
                }
                if self.drag.active {
                    let dx = cursor_x() - self.drag.start_x;
                    let scale = self.rend.scale;
                    if !self.drag.moved && f64::from(dx.abs()) > 4.0 * scale {
                        self.drag.moved = true;
                    }
                    if self.drag.moved {
                        self.cfg.offset = self.drag.start_offset - (f64::from(dx) / scale).round() as i32;
                        self.refresh_view(false);
                    }
                }
            }
            WM_MOUSELEAVE => {
                self.tracking = false;
                self.hover = false;
                self.refresh_view(false);
            }
            WM_LBUTTONDOWN => {
                // SAFETY: capturing our own window.
                unsafe { SetCapture(self.widget) };
                self.drag = Drag { active: true, moved: false, start_x: cursor_x(), start_offset: self.cfg.offset };
            }
            WM_LBUTTONUP => {
                // SAFETY: releases our own capture.
                unsafe { ReleaseCapture() };
                let was_drag = self.drag.moved;
                self.drag = Drag::default();
                if was_drag {
                    self.save_config();
                } else {
                    self.request_refresh();
                }
            }
            WM_DESTROY => {
                // The taskbar destroys its children when Explorer restarts;
                // housekeeping re-creates the widget once a taskbar is back.
                self.forget_widget();
            }
            _ => return None,
        }
        Some(0)
    }

    fn forget_widget(&mut self) {
        if !self.tooltip.is_null() {
            // SAFETY: our tooltip window.
            unsafe { DestroyWindow(self.tooltip) };
        }
        self.widget = std::ptr::null_mut();
        self.tooltip = std::ptr::null_mut();
        self.last_key.clear();
    }

    fn destroy_widget(&mut self) {
        if !self.widget.is_null() {
            // SAFETY: our widget window. Its WM_DESTROY arrives re-entrantly
            // and is ignored, so clean up here.
            unsafe { DestroyWindow(self.widget) };
        }
        self.forget_widget();
    }

    /// Runs every second: keeps the widget attached to the current taskbar,
    /// follows tray width / DPI / theme changes and advances the clock.
    fn housekeeping(&mut self) {
        // SAFETY: plain window lookups.
        let tray = unsafe { FindWindowW(w!("Shell_TrayWnd"), std::ptr::null()) };
        if tray.is_null() {
            return;
        }
        // SAFETY: IsWindow accepts any handle value.
        if !self.widget.is_null() && (unsafe { IsWindow(self.widget) } == 0 || self.tray != tray) {
            self.destroy_widget();
        }
        if self.widget.is_null() && !self.create_widget(tray) {
            return;
        }
        self.refresh_view(false);
    }

    fn create_widget(&mut self, tray: HWND) -> bool {
        let (ex, style) = if self.overlay {
            (WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST, WS_POPUP)
        } else {
            (WS_EX_LAYERED | WS_EX_NOACTIVATE, WS_CHILD | WS_CLIPSIBLINGS)
        };
        // SAFETY: `tray` is the parent (child mode) or owner (overlay mode).
        let h = unsafe {
            CreateWindowExW(ex, CLASS, std::ptr::null(), style, 0, 0, 1, 1, tray, std::ptr::null_mut(), self.hinst, std::ptr::null())
        };
        if h.is_null() {
            if !self.overlay {
                log::write("child window failed, falling back to overlay mode");
                self.overlay = true;
                return self.create_widget(tray);
            }
            log::write("CreateWindowEx(widget) failed");
            return false;
        }
        self.widget = h;
        self.tray = tray;
        self.last_key.clear();
        self.last_rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        self.create_tooltip();
        log::write(&format!("widget created (overlay={})", self.overlay));
        true
    }

    fn create_tooltip(&mut self) {
        // SAFETY: standard tooltip creation; the tool info outlives each call.
        unsafe {
            let tt = CreateWindowExW(
                WS_EX_TOPMOST,
                TOOLTIPS_CLASSW,
                std::ptr::null(),
                WS_POPUP | TTS_ALWAYSTIP | TTS_NOPREFIX,
                0,
                0,
                0,
                0,
                self.host,
                std::ptr::null_mut(),
                self.hinst,
                std::ptr::null(),
            );
            if tt.is_null() {
                return;
            }
            self.tooltip = tt;
            let mut text = wide(" ");
            let ti = self.tool_info(text.as_mut_ptr());
            SendMessageW(tt, TTM_ADDTOOLW, 0, &raw const ti as LPARAM);
            SendMessageW(tt, TTM_SETMAXTIPWIDTH, 0, 600);
            SendMessageW(tt, TTM_SETDELAYTIME, TTDT_AUTOPOP as WPARAM, 30_000);
        }
        self.tip_text.clear();
    }

    fn tool_info(&self, text: *mut u16) -> TTTOOLINFOW {
        TTTOOLINFOW {
            cbSize: size_of::<TTTOOLINFOW>() as u32,
            uFlags: TTF_IDISHWND | TTF_SUBCLASS,
            hwnd: self.host,
            uId: self.widget as usize,
            rect: RECT { left: 0, top: 0, right: 0, bottom: 0 },
            hinst: std::ptr::null_mut(),
            lpszText: text,
            lParam: 0,
            lpReserved: std::ptr::null_mut(),
        }
    }

    fn set_tooltip(&mut self, text: String) {
        if self.tooltip.is_null() || text == self.tip_text {
            return;
        }
        let mut w = wide(&text);
        let ti = self.tool_info(w.as_mut_ptr());
        // SAFETY: the tooltip copies the text during the call.
        unsafe { SendMessageW(self.tooltip, TTM_UPDATETIPTEXTW, 0, &raw const ti as LPARAM) };
        self.tip_text = text;
    }

    /// Re-lays out, repositions and repaints the widget when anything
    /// visible changed (or always, with `force`).
    fn refresh_view(&mut self, force: bool) {
        if self.widget.is_null() {
            return;
        }
        let mut tr = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        // SAFETY: valid out pointer; tray may be stale, which just fails.
        let ok = unsafe { GetWindowRect(self.tray, &mut tr) } != 0;
        let (tw, th) = (tr.right - tr.left, tr.bottom - tr.top);
        if !ok || tw <= th {
            // Vertical taskbars are not supported.
            // SAFETY: our window.
            unsafe { ShowWindow(self.widget, SW_HIDE) };
            return;
        }
        // SAFETY: tray is a live window.
        self.rend.set_scale(f64::from(unsafe { GetDpiForWindow(self.tray) }.max(96)) / 96.0);
        let light = self.use_light_palette();

        let shared = self.shared.clone();
        let s = shared.lock().unwrap_or_else(|e| e.into_inner());
        let now = now_ms();
        let msg = if s.last_short.is_empty() { "Claude: loading…".to_string() } else { format!("Claude: {}", s.last_short) };
        let stale = s.usage.is_some() && s.last_err.is_some() && s.last_ok.is_some_and(|t| now - t > 10 * timefmt::MINUTE);
        let update_dot = s.update.is_some();
        let view = View {
            usage: s.usage.as_ref(),
            now,
            msg: &msg,
            stale,
            hover: self.hover,
            light,
            update_dot,
            cfg: &self.cfg,
            locale: &self.locale,
        };
        let tip = self.tooltip_text(&s, now);
        let l = self.rend.layout(&view, th);

        let mut right = tr.right - (220.0 * self.rend.scale).round() as i32;
        // SAFETY: plain window lookups.
        unsafe {
            let notify = FindWindowExW(self.tray, std::ptr::null_mut(), w!("TrayNotifyWnd"), std::ptr::null());
            let mut nr = RECT { left: 0, top: 0, right: 0, bottom: 0 };
            if !notify.is_null() && GetWindowRect(notify, &mut nr) != 0 && nr.left > tr.left {
                right = nr.left;
            }
        }
        let scale = self.rend.scale;
        let mut x = right - (f64::from(4 + self.cfg.offset) * scale).round() as i32 - l.w;
        x = x.clamp(tr.left, (right - l.w).max(tr.left));
        // While dragging, keep the stored offset consistent with the clamped position.
        let drag_offset = self.drag.active.then(|| (f64::from(right - l.w - x) / scale).round() as i32 - 4);
        let screen = RECT { left: x, top: tr.top, right: x + l.w, bottom: tr.bottom };
        let key =
            format!("{}|{}|{}|{}|{}|{}|{}|{}|{}", s.version, now / timefmt::MINUTE, self.hover, light, stale, update_dot, msg, scale, l.w);
        let repaint = force || !rect_eq(&screen, &self.last_rect) || key != self.last_key;
        let dc = repaint.then(|| self.rend.draw(&view, &l));
        drop(s);
        if let Some(o) = drag_offset {
            self.cfg.offset = o;
        }
        if let Some(dc) = dc {
            self.present(dc, &l, &screen);
            self.last_key = key;
        }
        self.last_rect = screen;
        self.set_tooltip(tip);
    }

    /// Moves the widget into place and hands it the rendered bitmap.
    fn present(&mut self, dc: windows_sys::Win32::Graphics::Gdi::HDC, l: &Layout, screen: &RECT) {
        let size = SIZE { cx: l.w, cy: l.h };
        let src = POINT { x: 0, y: 0 };
        let blend = windows_sys::Win32::Graphics::Gdi::BLENDFUNCTION {
            BlendOp: windows_sys::Win32::Graphics::Gdi::AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: windows_sys::Win32::Graphics::Gdi::AC_SRC_ALPHA as u8,
        };
        let mut pos = POINT { x: screen.left, y: screen.top };
        // SAFETY: all pointers are to locals valid for the calls.
        unsafe {
            let (after, dst) = if self.overlay {
                (HWND_TOPMOST, &raw const pos)
            } else {
                ScreenToClient(self.tray, &mut pos);
                (HWND_TOP, std::ptr::null())
            };
            SetWindowPos(self.widget, after, pos.x, pos.y, size.cx, size.cy, SWP_NOACTIVATE | SWP_SHOWWINDOW);
            let ok = UpdateLayeredWindow(self.widget, std::ptr::null_mut(), dst, &size, dc, &src, 0, &blend, ULW_ALPHA);
            if ok == 0 && !self.overlay {
                // Layered child windows need Windows 8+ and the compatibility manifest.
                log::write("UpdateLayeredWindow on child failed, switching to overlay mode");
                self.overlay = true;
                self.destroy_widget();
            }
        }
    }

    fn use_light_palette(&self) -> bool {
        match self.cfg.theme.as_str() {
            "light" => return true,
            "dark" => return false,
            _ => {}
        }
        let mut value = 0u32;
        let mut size = 4u32;
        // SAFETY: value/size describe a DWORD buffer.
        let rc = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
                w!("SystemUsesLightTheme"),
                RRF_RT_REG_DWORD,
                std::ptr::null_mut(),
                (&raw mut value).cast(),
                &mut size,
            )
        };
        rc == ERROR_SUCCESS && value == 1
    }

    fn tooltip_text(&self, s: &Shared, now: i64) -> String {
        let mut b = String::new();
        if let Some(u) = &s.usage {
            for q in u.session.iter().chain(&u.weekly).chain(&u.others) {
                let _ = write!(b, "{}: {}", q.name, format_percent(q.percent));
                match q.resets_at {
                    Some(t) => {
                        let _ =
                            write!(b, " · resets in {} ({})", timefmt::countdown(t - now), timefmt::reset_at_long(t, now, &self.locale));
                    }
                    None => b.push_str(" · not started"),
                }
                b.push('\n');
            }
        }
        if let Some(e) = &s.last_err {
            let _ = writeln!(b, "Last refresh failed: {e}");
        }
        if let Some(t) = s.last_ok {
            let _ = write!(b, "Updated {}", self.locale.clock(&timefmt::to_local(t, &self.locale)));
            if self.demo {
                b.push_str(" (demo data)");
            }
            b.push('\n');
        }
        if s.updating {
            b.push_str("Installing update…\n");
        } else if let Some(r) = &s.update {
            let _ = writeln!(b, "Update available: v{} (right-click to install)", r.version);
        }
        b.push_str("Click to refresh · drag to move · right-click for options");
        b
    }

    fn build_menu(&self) -> HMENU {
        // SAFETY: menu construction; strings live until each call returns.
        unsafe {
            let menu = CreatePopupMenu();
            let add = |m: HMENU, id: usize, text: &str, flags: MENU_ITEM_FLAGS| {
                let t = wide(text);
                AppendMenuW(m, flags, id, if flags & MF_SEPARATOR != 0 { std::ptr::null() } else { t.as_ptr() });
            };
            let check = |b: bool| if b { MF_CHECKED } else { MF_UNCHECKED };

            let show = CreatePopupMenu();
            for (i, t) in TOGGLES.iter().enumerate() {
                if t.separator {
                    add(show, 0, "", MF_SEPARATOR);
                }
                let mut flags = MF_STRING | check(toggle_value(&self.cfg, i));
                if toggle_locked(&self.cfg, i) {
                    flags |= MF_GRAYED;
                }
                add(show, CMD_TOGGLE_BASE as usize + i, t.label, flags);
            }

            let (update, updating) = {
                let s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
                (s.update.as_ref().map(|r| r.version.clone()), s.updating)
            };
            if let Some(v) = update {
                let (label, flags) =
                    if updating { ("Installing update…".to_string(), MF_GRAYED) } else { (format!("Update to v{v}"), 0) };
                add(menu, CMD_UPDATE as usize, &label, MF_STRING | flags);
                if !updating {
                    SetMenuDefaultItem(menu, CMD_UPDATE, 0); // bold
                }
                add(menu, 0, "", MF_SEPARATOR);
            }
            add(menu, CMD_REFRESH as usize, "Refresh now", MF_STRING);
            add(menu, CMD_OPEN_USAGE as usize, "Open usage page on claude.ai", MF_STRING);
            add(menu, 0, "", MF_SEPARATOR);
            add(menu, show as usize, "Show", MF_STRING | MF_POPUP);
            add(menu, 0, "", MF_SEPARATOR);
            add(menu, CMD_AUTOSTART as usize, "Start with Windows", MF_STRING | check(autostart_enabled()));
            add(menu, CMD_CHECK_UPDATES as usize, "Check for updates daily", MF_STRING | check(self.cfg.check_updates));
            add(menu, CMD_RESET_POS as usize, "Move back next to the tray", MF_STRING);
            add(menu, CMD_SETTINGS as usize, "Edit settings file", MF_STRING);
            add(menu, 0, "", MF_SEPARATOR);
            add(menu, 0, &format!("{APP_NAME} {VERSION}"), MF_STRING | MF_GRAYED);
            add(menu, CMD_EXIT as usize, "Exit", MF_STRING);
            menu
        }
    }

    fn on_command(&mut self, cmd: u32) {
        if let Some(i) = cmd.checked_sub(CMD_TOGGLE_BASE).map(|i| i as usize).filter(|&i| i < TOGGLES.len()) {
            if !toggle_locked(&self.cfg, i) {
                let f = (TOGGLES[i].field)(&mut self.cfg);
                *f = !*f;
                self.save_config();
                self.refresh_view(true);
            }
            return;
        }
        match cmd {
            CMD_REFRESH => self.request_refresh(),
            CMD_OPEN_USAGE => open(USAGE_PAGE.as_ref()),
            CMD_AUTOSTART => set_autostart(!autostart_enabled()),
            CMD_RESET_POS => {
                self.cfg.offset = 0;
                self.save_config();
                self.refresh_view(true);
            }
            CMD_SETTINGS => {
                self.save_config(); // make sure the file exists with every key
                if let Some(p) = Config::path() {
                    open(p.as_os_str());
                }
            }
            CMD_EXIT => self.quit(),
            CMD_UPDATE => self.start_update(),
            CMD_CHECK_UPDATES => {
                self.cfg.check_updates = !self.cfg.check_updates;
                self.save_config();
                let mut s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
                s.check_updates = self.cfg.check_updates;
                s.next_update_check = 0;
                if !self.cfg.check_updates {
                    s.update = None;
                }
                drop(s);
                self.refresh_view(true);
            }
            _ => {}
        }
    }

    /// Downloads and installs the update on a background thread; the UI
    /// thread picks up the result in `finish_update`.
    fn start_update(&mut self) {
        let release = {
            let mut s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
            if s.updating {
                return;
            }
            let Some(r) = s.update.clone() else { return };
            s.updating = true;
            r
        };
        log::write(&format!("installing update v{}", release.version));
        let (shared, host) = (self.shared.clone(), self.host as isize);
        let _ = std::thread::Builder::new().stack_size(256 * 1024).spawn(move || {
            let result = updater::install(&release);
            shared.lock().unwrap_or_else(|e| e.into_inner()).update_result = Some(result);
            // SAFETY: posting to our host window; harmless if it's gone.
            unsafe { PostMessageW(host as HWND, WM_APP_UPDATED, 0, 0) };
        });
        self.refresh_view(true);
    }

    fn save_config(&self) {
        if let Err(e) = self.cfg.save() {
            log::write(&format!("save config: {e}"));
        }
    }

    fn request_refresh(&self) {
        // SAFETY: event handle owned by the app for its whole lifetime.
        unsafe { SetEvent(self.refresh_event) };
    }

    fn quit(&mut self) {
        self.destroy_widget();
        // SAFETY: ends this thread's message loop.
        unsafe { PostQuitMessage(0) };
    }
}

fn rect_eq(a: &RECT, b: &RECT) -> bool {
    (a.left, a.top, a.right, a.bottom) == (b.left, b.top, b.right, b.bottom)
}

/// Opens a URL or file with its default handler. Goes through explorer.exe
/// rather than ShellExecute so shell32 and COM never load into this process.
fn open(target: &std::ffi::OsStr) {
    let _ = std::process::Command::new("explorer.exe").arg(target).spawn();
}

fn autostart_enabled() -> bool {
    let mut size = 0u32;
    // SAFETY: size query only.
    unsafe {
        RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, RRF_RT_REG_SZ, std::ptr::null_mut(), std::ptr::null_mut(), &mut size)
            == ERROR_SUCCESS
    }
}

fn set_autostart(on: bool) {
    // SAFETY: registry writes with valid, NUL-terminated buffers.
    unsafe {
        if !on {
            RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE);
            return;
        }
        let Ok(exe) = std::env::current_exe() else { return };
        let value = wide(&format!("\"{}\"", exe.display()));
        RegSetKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, REG_SZ, value.as_ptr().cast(), (value.len() * 2) as u32);
    }
}

/// Checks for updates 30 s after start, then daily (sooner after errors).
fn spawn_updater(shared: Arc<Mutex<Shared>>, host: isize) {
    let _ = std::thread::Builder::new().name("updater".into()).stack_size(256 * 1024).spawn(move || {
        let lock = || shared.lock().unwrap_or_else(|e| e.into_inner());
        lock().next_update_check = now_ms() + 30_000;
        loop {
            std::thread::sleep(std::time::Duration::from_secs(15));
            let (enabled, due) = {
                let s = lock();
                (s.check_updates, now_ms() >= s.next_update_check && !s.updating)
            };
            if !enabled || !due {
                continue;
            }
            updater::cleanup_old();
            let result = updater::check();
            let mut s = lock();
            match result {
                Ok(found) => {
                    if let Some(r) = &found {
                        log::write(&format!("update available: v{}", r.version));
                    }
                    s.update = found;
                    s.next_update_check = now_ms() + timefmt::DAY;
                }
                Err(()) => s.next_update_check = now_ms() + timefmt::HOUR,
            }
            s.version += 1;
            drop(s);
            // SAFETY: posting to our host window; harmless if it's gone.
            unsafe { PostMessageW(host as HWND, WM_APP_DATA, 0, 0) };
        }
    });
}

// Polling thread: fetches, stores the result in `Shared`, and pokes the UI
// thread with WM_APP_DATA.

fn spawn_poller(shared: Arc<Mutex<Shared>>, host: isize, event: isize, demo: bool, cfg: &Config) {
    let (poll, idle_poll) = (cfg.poll_seconds, cfg.idle_poll_seconds);
    let _ = std::thread::Builder::new()
        .name("poller".into())
        .stack_size(256 * 1024)
        .spawn(move || Poller { shared, host, event, demo, poll, idle_poll }.run());
}

struct Poller {
    shared: Arc<Mutex<Shared>>,
    host: isize,
    event: isize,
    demo: bool,
    poll: u64,
    idle_poll: u64,
}

impl Poller {
    fn run(self) {
        let user_agent = if self.demo { cli::FALLBACK_USER_AGENT.to_string() } else { cli::user_agent() };
        let mut backoff = 0i64;
        let mut last_cli_auth: Option<i64> = None;
        let mut prev_session: Option<f64> = None;
        loop {
            self.shared.lock().unwrap_or_else(|e| e.into_inner()).last_attempt = now_ms();
            let mut result = if self.demo { Ok(demo_usage(now_ms())) } else { fetch(&user_agent) };
            if let Err(e) = &result
                && e.auth
                && read_token().is_some()
                && last_cli_auth.is_none_or(|t| now_ms() - t > 30 * timefmt::MINUTE)
            {
                // Expired token: let Claude Code renew it, then retry once.
                last_cli_auth = Some(now_ms());
                log::write("token rejected, running claude update");
                cli::update();
                result = fetch(&user_agent);
            }

            let delay = match &result {
                Ok(u) => {
                    backoff = 0;
                    let d = self.success_delay(prev_session, u);
                    prev_session = u.session.as_ref().map(|q| q.percent);
                    d
                }
                Err(e) if e.auth => 5 * timefmt::MINUTE,
                Err(e) => {
                    backoff = (backoff * 2).clamp(30_000, 15 * timefmt::MINUTE);
                    backoff.max(e.retry_after_secs as i64 * 1000)
                }
            };
            {
                let mut s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
                match result {
                    Ok(u) => {
                        s.usage = Some(u);
                        s.last_ok = Some(now_ms());
                        s.last_err = None;
                        s.last_short = "";
                    }
                    Err(e) => {
                        log::write(&format!("fetch: {}", e.msg));
                        s.last_err = Some(e.msg);
                        s.last_short = e.short;
                    }
                }
                s.version += 1;
            }
            // SAFETY: posting to our host window; harmless if it's gone.
            unsafe { PostMessageW(self.host as HWND, WM_APP_DATA, 0, 0) };
            self.wait(delay);
        }
    }

    fn success_delay(&self, prev_session: Option<f64>, u: &Usage) -> i64 {
        if self.demo {
            return timefmt::MINUTE;
        }
        let mut delay = self.poll as i64 * 1000;
        if idle_ms() > 5 * timefmt::MINUTE {
            delay = self.idle_poll as i64 * 1000;
        } else if let (Some(prev), Some(q)) = (prev_session, &u.session)
            && q.percent > prev
        {
            delay = delay.min(2 * timefmt::MINUTE); // actively used: follow more closely
        }
        // Pick up a reset right after it happens.
        for q in u.session.iter().chain(&u.weekly) {
            if let Some(t) = q.resets_at {
                let until = t - now_ms() + 5_000;
                if until > 0 && until < delay {
                    delay = until;
                }
            }
        }
        delay.max(5_000)
    }

    /// Sleeps until the deadline or a manual refresh, checking the wall
    /// clock in short steps so a PC waking from sleep catches up.
    fn wait(&self, delay: i64) {
        let deadline = now_ms() + delay;
        loop {
            let left = deadline - now_ms();
            if left <= 0 {
                return;
            }
            // SAFETY: event handle lives as long as the process.
            let r = unsafe { WaitForSingleObject(self.event as HANDLE, left.min(15_000) as u32) };
            if r == WAIT_OBJECT_0 {
                let last = self.shared.lock().unwrap_or_else(|e| e.into_inner()).last_attempt;
                if now_ms() - last >= 10_000 {
                    return;
                }
            }
        }
    }
}

fn idle_ms() -> i64 {
    let mut li = LASTINPUTINFO { cbSize: size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
    // SAFETY: valid out pointer.
    unsafe {
        GetLastInputInfo(&mut li);
        i64::from(GetTickCount().wrapping_sub(li.dwTime))
    }
}

fn claude_config_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return Some(d.into());
    }
    Some(PathBuf::from(std::env::var_os("USERPROFILE")?).join(".claude"))
}

/// The OAuth access token Claude Code stores locally. Never logged.
fn read_token() -> Option<String> {
    let raw = std::fs::read(claude_config_dir()?.join(".credentials.json")).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    let t = v.pointer("/claudeAiOauth/accessToken")?.as_str()?;
    (!t.is_empty()).then(|| t.to_string())
}

fn fetch(user_agent: &str) -> Result<Usage, FetchError> {
    let Some(token) = read_token() else {
        return Err(FetchError { auth: true, ..FetchError::new("not logged in", "Not logged in to Claude Code (run: claude auth login)") });
    };
    let headers = format!("Authorization: Bearer {token}\r\nContent-Type: application/json\r\nanthropic-beta: oauth-2025-04-20\r\n");
    drop(token);
    let resp = http::get(USAGE_URL, &headers, user_agent, 1 << 20).map_err(|()| FetchError::new("offline", "Connection error"))?;
    match resp.status {
        200 => parse_usage(&resp.body).ok_or_else(|| FetchError::new("bad response", "Unexpected API response")),
        401 => Err(FetchError { auth: true, ..FetchError::new("login expired", "Login expired (run Claude Code once to renew it)") }),
        429 => Err(FetchError {
            retry_after_secs: resp.retry_after,
            ..FetchError::new("rate limited", "Rate limited by the API, retrying later")
        }),
        s if s >= 500 => Err(FetchError::new("server error", format!("Server error {s}"))),
        s => Err(FetchError::new("http error", format!("HTTP error {s}"))),
    }
}

/// A log of the current run in `%LOCALAPPDATA%\ClaudeTaskbarUsage\widget.log`.
mod log {
    use std::io::Write;
    use std::path::PathBuf;

    fn path() -> Option<PathBuf> {
        Some(PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("ClaudeTaskbarUsage").join("widget.log"))
    }

    pub fn init() {
        if let Some(p) = path() {
            let _ = std::fs::create_dir_all(p.parent().unwrap_or(&p));
            let _ = std::fs::write(p, b"");
        }
    }

    pub fn write(msg: &str) {
        let Some(p) = path() else { return };
        let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(p) else { return };
        let t = crate::timefmt::split_local(super::now_ms());
        let _ = writeln!(f, "{}-{:02}-{:02} {:02}:{:02} UTC  {msg}", t.year, t.month, t.day, t.hour, t.minute);
    }
}
