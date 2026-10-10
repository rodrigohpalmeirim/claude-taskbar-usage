package main

import (
	"context"
	"flag"
	"fmt"
	"log"
	"math"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"sync"
	"syscall"
	"time"
	"unsafe"

	"golang.org/x/sys/windows"
	"golang.org/x/sys/windows/registry"
)

const (
	appName    = "Claude Taskbar Usage"
	className  = "ClaudeTaskbarUsageWidget"
	runKeyName = "ClaudeTaskbarUsage"
	usagePage  = "https://claude.ai/settings/usage"

	wmAppData = WM_APP + 1

	timerHousekeeping = 1
)

const (
	cmdRefresh = iota + 1
	cmdOpenUsage
	cmdAutostart
	cmdResetPos
	cmdSettings
	cmdExit
	cmdToggleBase = 100 // + index into displayToggles
)

// displayToggle is one entry of the right-click "Show" submenu.
type displayToggle struct {
	label     string
	field     func(*Config) *bool
	separator bool // draw a separator before this entry
}

var displayToggles = []displayToggle{
	{label: "5-hour session", field: func(c *Config) *bool { return &c.ShowSession }},
	{label: "Weekly", field: func(c *Config) *bool { return &c.ShowWeekly }},
	{label: "Labels (5h / 7d)", field: func(c *Config) *bool { return &c.ShowLabels }, separator: true},
	{label: "Bar", field: func(c *Config) *bool { return &c.ShowBar }},
	{label: "Hour / day ticks", field: func(c *Config) *bool { return &c.ShowTicks }},
	{label: "Current time marker", field: func(c *Config) *bool { return &c.ShowMarker }},
	{label: "Percentage", field: func(c *Config) *bool { return &c.ShowPercent }},
	{label: "Time until reset", field: func(c *Config) *bool { return &c.ShowCountdown }},
	{label: "Reset time", field: func(c *Config) *bool { return &c.ShowResetTime }},
	{label: "Red bar when ahead of the clock", field: func(c *Config) *bool { return &c.WarnWhenOutpacing }, separator: true},
}

// toggleLocked reports whether an entry must stay as it is: the last
// visible row or the last visible content column can't be hidden, and the
// bar decorations are meaningless without the bar.
func toggleLocked(c *Config, field *bool) bool {
	switch field {
	case &c.ShowSession, &c.ShowWeekly:
		return *field && c.ShowSession != c.ShowWeekly
	case &c.ShowBar, &c.ShowPercent, &c.ShowCountdown, &c.ShowResetTime:
		n := 0
		for _, b := range []bool{c.ShowBar, c.ShowPercent, c.ShowCountdown, c.ShowResetTime} {
			if b {
				n++
			}
		}
		return *field && n == 1
	case &c.ShowTicks, &c.ShowMarker, &c.WarnWhenOutpacing:
		return !c.ShowBar
	}
	return false
}

// Set at build time: VERSION=1.2.3 ./build.sh
var version = "dev"

func init() { runtime.LockOSThread() }

type App struct {
	cfg       Config
	demo      bool
	overlay   bool // fallback: topmost popup over the taskbar instead of a child window
	hinst     uintptr
	host      uintptr // hidden top-level window: timers, menus, broadcasts
	widget    uintptr
	tooltip   uintptr
	tray      uintptr
	rend      *Renderer
	light     bool
	hover     bool
	tracking  bool
	lastKey   string
	lastRect  RECT
	tipText   string
	refreshCh chan struct{}

	drag struct {
		active, moved bool
		startX        int32
		startOffset   int
	}

	mu          sync.Mutex
	usage       *Usage
	lastOK      time.Time
	lastErr     string
	lastShort   string
	lastAttempt time.Time
	version     int
}

var app *App

func main() {
	demo := flag.Bool("demo", false, "show sample data instead of calling the API")
	overlay := flag.Bool("overlay", false, "draw over the taskbar instead of docking into it")
	flag.Parse()

	if name, err := windows.UTF16PtrFromString(`Local\ClaudeTaskbarUsage`); err == nil {
		if _, err := windows.CreateMutex(nil, false, name); err == windows.ERROR_ALREADY_EXISTS {
			return
		}
	}
	setupLog()
	setupLocaleFormatters()

	app = &App{
		cfg:       loadConfig(),
		demo:      *demo,
		overlay:   *overlay,
		rend:      newRenderer(),
		refreshCh: make(chan struct{}, 1),
	}
	if err := app.run(); err != nil {
		log.Printf("fatal: %v", err)
		messageBox(err.Error(), appName, MB_OK|MB_ICONERROR)
		os.Exit(1)
	}
}

func setupLog() {
	dir, err := os.UserCacheDir() // %LOCALAPPDATA%
	if err != nil {
		return
	}
	dir = filepath.Join(dir, "ClaudeTaskbarUsage")
	_ = os.MkdirAll(dir, 0o755)
	f, err := os.Create(filepath.Join(dir, "widget.log"))
	if err != nil {
		return
	}
	log.SetOutput(f)
	log.SetFlags(log.LstdFlags)
}

func (a *App) run() error {
	h, _, _ := procGetModuleHandleW.Call(0)
	a.hinst = h

	icc := INITCOMMONCONTROLSEX{DwSize: uint32(unsafe.Sizeof(INITCOMMONCONTROLSEX{})), DwICC: ICC_WIN95_CLASSES}
	procInitCommonControlsEx.Call(uintptr(unsafe.Pointer(&icc)))

	cursor, _, _ := procLoadCursorW.Call(0, IDC_ARROW)
	wc := WNDCLASSEXW{
		CbSize:        uint32(unsafe.Sizeof(WNDCLASSEXW{})),
		LpfnWndProc:   syscall.NewCallback(wndProc),
		HInstance:     a.hinst,
		HCursor:       cursor,
		LpszClassName: u16(className),
	}
	if r, _, err := procRegisterClassExW.Call(uintptr(unsafe.Pointer(&wc))); r == 0 {
		return fmt.Errorf("RegisterClassEx: %v", err)
	}

	host, _, err := procCreateWindowExW.Call(WS_EX_TOOLWINDOW, uintptr(unsafe.Pointer(u16(className))),
		uintptr(unsafe.Pointer(u16(appName))), WS_POPUP, 0, 0, 0, 0, 0, 0, a.hinst, 0)
	if host == 0 {
		return fmt.Errorf("CreateWindowEx(host): %v", err)
	}
	a.host = host
	procSetTimer.Call(a.host, timerHousekeeping, 1000, 0)

	log.Printf("started %s (demo=%v overlay=%v)", version, a.demo, a.overlay)
	go a.poller()
	a.housekeeping()

	var msg MSG
	for {
		r, _, _ := procGetMessageW.Call(uintptr(unsafe.Pointer(&msg)), 0, 0, 0)
		if int32(r) <= 0 {
			break
		}
		procTranslateMessage.Call(uintptr(unsafe.Pointer(&msg)))
		procDispatchMessageW.Call(uintptr(unsafe.Pointer(&msg)))
	}
	return nil
}

func wndProc(hwnd uintptr, msg uint32, wparam, lparam uintptr) uintptr {
	a := app
	if a != nil && hwnd == a.widget && hwnd != 0 {
		if handled, ret := a.widgetMsg(msg, wparam, lparam); handled {
			return ret
		}
	} else if a != nil && hwnd == a.host {
		switch msg {
		case WM_TIMER:
			a.housekeeping()
			return 0
		case wmAppData:
			a.refreshView(true)
			return 0
		case WM_SETTINGCHANGE:
			a.refreshView(false)
		case WM_CLOSE:
			a.quit()
			return 0
		}
	}
	r, _, _ := procDefWindowProcW.Call(hwnd, uintptr(msg), wparam, lparam)
	return r
}

func (a *App) widgetMsg(msg uint32, wparam, lparam uintptr) (bool, uintptr) {
	switch msg {
	case WM_MOUSEMOVE:
		if !a.tracking {
			tme := TRACKMOUSEEVENT{CbSize: uint32(unsafe.Sizeof(TRACKMOUSEEVENT{})), DwFlags: TME_LEAVE, HwndTrack: a.widget}
			procTrackMouseEvent.Call(uintptr(unsafe.Pointer(&tme)))
			a.tracking = true
			a.hover = true
			a.refreshView(false)
		}
		if a.drag.active {
			dx := cursorPos().X - a.drag.startX
			scale := a.rend.scale
			if !a.drag.moved && math.Abs(float64(dx)) > 4*scale {
				a.drag.moved = true
			}
			if a.drag.moved {
				a.cfg.Offset = a.drag.startOffset - int(math.Round(float64(dx)/scale))
				a.refreshView(false)
			}
		}
		return true, 0
	case WM_MOUSELEAVE:
		a.tracking = false
		a.hover = false
		a.refreshView(false)
		return true, 0
	case WM_LBUTTONDOWN:
		procSetCapture.Call(a.widget)
		a.drag.active = true
		a.drag.moved = false
		a.drag.startX = cursorPos().X
		a.drag.startOffset = a.cfg.Offset
		return true, 0
	case WM_LBUTTONUP:
		procReleaseCapture.Call()
		wasDrag := a.drag.moved
		a.drag.active, a.drag.moved = false, false
		if wasDrag {
			a.saveConfig()
		} else {
			a.requestRefresh()
		}
		return true, 0
	case WM_RBUTTONUP:
		a.showMenu()
		return true, 0
	case WM_DESTROY:
		// The taskbar destroys its children when Explorer restarts;
		// housekeeping re-creates the widget once a taskbar is back.
		if a.tooltip != 0 {
			procDestroyWindow.Call(a.tooltip)
		}
		a.widget = 0
		a.tooltip = 0
		a.lastKey = ""
		return true, 0
	}
	return false, 0
}

// housekeeping runs every second: keep the widget attached to the current
// taskbar, follow tray width / DPI / theme changes, and advance the clock.
func (a *App) housekeeping() {
	tray := findWindow("Shell_TrayWnd")
	if tray == 0 {
		return
	}
	if a.widget != 0 && (!isWindow(a.widget) || a.tray != tray) {
		procDestroyWindow.Call(a.widget)
		a.widget = 0
	}
	if a.widget == 0 {
		if err := a.createWidget(tray); err != nil {
			log.Printf("create widget: %v", err)
			return
		}
	}
	a.refreshView(false)
}

func (a *App) createWidget(tray uintptr) error {
	exStyle := uintptr(WS_EX_LAYERED | WS_EX_NOACTIVATE)
	style := uintptr(WS_CHILD | WS_CLIPSIBLINGS)
	parent := tray
	if a.overlay {
		exStyle |= WS_EX_TOOLWINDOW | WS_EX_TOPMOST
		style = WS_POPUP
	}
	h, _, err := procCreateWindowExW.Call(exStyle, uintptr(unsafe.Pointer(u16(className))), 0, style,
		0, 0, 1, 1, parent, 0, a.hinst, 0)
	if h == 0 {
		if !a.overlay {
			log.Printf("child window failed (%v), falling back to overlay mode", err)
			a.overlay = true
			return a.createWidget(tray)
		}
		return err
	}
	a.widget = h
	a.tray = tray
	a.lastKey = ""
	a.lastRect = RECT{}
	a.createTooltip()
	log.Printf("widget created (overlay=%v)", a.overlay)
	return nil
}

func (a *App) createTooltip() {
	tt, _, _ := procCreateWindowExW.Call(WS_EX_TOPMOST, uintptr(unsafe.Pointer(u16("tooltips_class32"))), 0,
		WS_POPUP|TTS_ALWAYSTIP|TTS_NOPREFIX, 0, 0, 0, 0, a.host, 0, a.hinst, 0)
	if tt == 0 {
		return
	}
	a.tooltip = tt
	ti := a.toolInfo(u16(" "))
	sendMessage(tt, TTM_ADDTOOLW, 0, uintptr(unsafe.Pointer(&ti)))
	sendMessage(tt, TTM_SETMAXTIPWIDTH, 0, 600)
	sendMessage(tt, TTM_SETDELAYTIME, TTDT_AUTOPOP, 30000)
	a.tipText = ""
}

func (a *App) toolInfo(text *uint16) TOOLINFOW {
	return TOOLINFOW{
		CbSize:   uint32(unsafe.Sizeof(TOOLINFOW{})),
		UFlags:   TTF_IDISHWND | TTF_SUBCLASS,
		Hwnd:     a.host,
		UId:      a.widget,
		LpszText: text,
	}
}

func (a *App) setTooltip(text string) {
	if a.tooltip == 0 || text == a.tipText {
		return
	}
	a.tipText = text
	ti := a.toolInfo(u16(text))
	sendMessage(a.tooltip, TTM_UPDATETIPTEXTW, 0, uintptr(unsafe.Pointer(&ti)))
}

// refreshView re-lays out, repositions and repaints the widget when anything
// visible changed (or always, with force).
func (a *App) refreshView(force bool) {
	if a.widget == 0 {
		return
	}
	tr, ok := windowRect(a.tray)
	if !ok || tr.W() <= tr.H() {
		// Vertical taskbars are not supported.
		showWindow(a.widget, SW_HIDE)
		return
	}
	a.rend.setScale(float64(dpiForWindow(a.tray)) / 96)
	a.light = a.useLightPalette()

	a.mu.Lock()
	now := time.Now()
	v := View{
		Usage: a.usage,
		Now:   now,
		Hover: a.hover,
		Light: a.light,
		Cfg:   a.cfg,
	}
	if a.usage == nil {
		v.Msg = "Claude: loading…"
		if a.lastShort != "" {
			v.Msg = "Claude: " + a.lastShort
		}
	} else {
		v.Stale = a.lastErr != "" && now.Sub(a.lastOK) > 10*time.Minute
	}
	version := a.version
	tip := a.tooltipText(now)
	a.mu.Unlock()

	L := a.rend.layout(&v, int(tr.H()))

	right := tr.Right - int32(math.Round(220*a.rend.scale))
	if notify := findChild(a.tray, "TrayNotifyWnd"); notify != 0 {
		if nr, ok := windowRect(notify); ok && nr.Left > tr.Left {
			right = nr.Left
		}
	}
	x := right - int32(math.Round(float64(4+a.cfg.Offset)*a.rend.scale)) - int32(L.w)
	x = max(tr.Left, min(right-int32(L.w), x))
	if a.drag.active {
		// Keep the stored offset consistent with the clamped position.
		a.cfg.Offset = int(math.Round(float64(right-int32(L.w)-x)/a.rend.scale)) - 4
	}
	screen := RECT{x, tr.Top, x + int32(L.w), tr.Bottom}

	key := fmt.Sprintf("%d|%s|%v|%v|%v|%v|%.2f|%d", version, now.Format("15:04"), v.Hover, v.Light, v.Stale, v.Msg, a.rend.scale, L.w)
	if force || key != a.lastKey || screen != a.lastRect {
		a.paint(&v, L, screen)
		a.lastKey = key
	}
	if screen != a.lastRect {
		a.lastRect = screen
	}
	a.setTooltip(tip)
}

func (a *App) paint(v *View, L layout, screen RECT) {
	dc := a.rend.Draw(v, L)
	size := SIZE{int32(L.w), int32(L.h)}
	src := POINT{}
	bf := BLENDFUNCTION{BlendOp: AC_SRC_OVER, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA}

	pos := POINT{screen.Left, screen.Top}
	insertAfter := uintptr(HWND_TOPMOST)
	var ptDst uintptr
	if !a.overlay {
		pos = screenToClient(a.tray, pos)
		insertAfter = HWND_TOP
	} else {
		ptDst = uintptr(unsafe.Pointer(&pos))
	}
	setWindowPos(a.widget, insertAfter, pos.X, pos.Y, size.CX, size.CY, SWP_NOACTIVATE|SWP_SHOWWINDOW)
	r, _, err := procUpdateLayeredWindow.Call(a.widget, 0, ptDst, uintptr(unsafe.Pointer(&size)), dc,
		uintptr(unsafe.Pointer(&src)), 0, uintptr(unsafe.Pointer(&bf)), ULW_ALPHA)
	if r == 0 && !a.overlay {
		// Layered child windows need Windows 8+ and the compatibility manifest.
		log.Printf("UpdateLayeredWindow on child failed (%v), switching to overlay mode", err)
		a.overlay = true
		procDestroyWindow.Call(a.widget)
		a.widget = 0
	}
}

func (a *App) useLightPalette() bool {
	switch a.cfg.Theme {
	case "light":
		return true
	case "dark":
		return false
	}
	k, err := registry.OpenKey(registry.CURRENT_USER, `Software\Microsoft\Windows\CurrentVersion\Themes\Personalize`, registry.QUERY_VALUE)
	if err != nil {
		return false
	}
	defer k.Close()
	v, _, err := k.GetIntegerValue("SystemUsesLightTheme")
	return err == nil && v == 1
}

// tooltipText must be called with a.mu held.
func (a *App) tooltipText(now time.Time) string {
	var b strings.Builder
	if a.usage != nil {
		qs := []*Quota{a.usage.Session, a.usage.Weekly}
		qs = append(qs, a.usage.Others...)
		for _, q := range qs {
			if q == nil {
				continue
			}
			fmt.Fprintf(&b, "%s: %s", q.Name, formatPercent(q.Percent))
			if q.ResetsAt.IsZero() {
				b.WriteString(" · not started")
			} else {
				fmt.Fprintf(&b, " · resets in %s (%s)", fmtCountdown(q.ResetsAt.Sub(now)), fmtResetAtLong(q.ResetsAt, now))
			}
			b.WriteString("\n")
		}
	}
	if a.lastErr != "" {
		fmt.Fprintf(&b, "Last update failed: %s\n", a.lastErr)
	}
	if !a.lastOK.IsZero() {
		fmt.Fprintf(&b, "Updated %s", formatClock(a.lastOK))
		if a.demo {
			b.WriteString(" (demo data)")
		}
		b.WriteString("\n")
	}
	b.WriteString("Click to refresh · drag to move · right-click for options")
	return b.String()
}

func (a *App) showMenu() {
	m, _, _ := procCreatePopupMenu.Call()
	defer procDestroyMenu.Call(m)
	add := func(id int, text string, flags uint32) {
		procAppendMenuW.Call(m, uintptr(flags), uintptr(id), uintptr(unsafe.Pointer(u16(text))))
	}
	check := func(b bool) uint32 {
		if b {
			return MF_CHECKED
		}
		return 0
	}
	add(cmdRefresh, "Refresh now", MF_STRING)
	add(cmdOpenUsage, "Open usage page on claude.ai", MF_STRING)
	add(0, "", MF_SEPARATOR)
	show, _, _ := procCreatePopupMenu.Call()
	for i, t := range displayToggles {
		if t.separator {
			procAppendMenuW.Call(show, MF_SEPARATOR, 0, 0)
		}
		flags := MF_STRING | check(*t.field(&a.cfg))
		if toggleLocked(&a.cfg, t.field(&a.cfg)) {
			flags |= MF_GRAYED
		}
		procAppendMenuW.Call(show, uintptr(flags), uintptr(cmdToggleBase+i), uintptr(unsafe.Pointer(u16(t.label))))
	}
	add(int(show), "Show", MF_STRING|MF_POPUP) // destroyed together with the parent menu
	add(0, "", MF_SEPARATOR)
	add(cmdAutostart, "Start with Windows", MF_STRING|check(autostartEnabled()))
	add(cmdResetPos, "Move back next to the tray", MF_STRING)
	add(cmdSettings, "Edit settings file", MF_STRING)
	add(0, "", MF_SEPARATOR)
	add(0, appName+" "+version, MF_STRING|MF_GRAYED)
	add(cmdExit, "Exit", MF_STRING)

	pt := cursorPos()
	procSetForegroundWindow.Call(a.host)
	cmd, _, _ := procTrackPopupMenu.Call(m, TPM_RIGHTBUTTON|TPM_RETURNCMD|TPM_BOTTOMALIGN|TPM_RIGHTALIGN,
		uintptr(pt.X), uintptr(pt.Y), 0, a.host, 0)
	postMessage(a.host, WM_NULL, 0, 0)

	if i := int(cmd) - cmdToggleBase; i >= 0 && i < len(displayToggles) {
		if f := displayToggles[i].field(&a.cfg); !toggleLocked(&a.cfg, f) {
			*f = !*f
			a.saveConfig()
			a.refreshView(true)
		}
		return
	}
	switch cmd {
	case cmdRefresh:
		a.requestRefresh()
	case cmdOpenUsage:
		openURL(usagePage)
	case cmdAutostart:
		setAutostart(!autostartEnabled())
	case cmdResetPos:
		a.cfg.Offset = 0
		a.saveConfig()
		a.refreshView(true)
	case cmdSettings:
		a.saveConfig() // make sure the file exists with every key
		openURL(configPath())
	case cmdExit:
		a.quit()
	}
}

func (a *App) saveConfig() {
	if err := saveConfig(a.cfg); err != nil {
		log.Printf("save config: %v", err)
	}
}

func (a *App) quit() {
	if a.widget != 0 {
		procDestroyWindow.Call(a.widget)
		a.widget = 0
	}
	procPostQuitMessage.Call(0)
}

func openURL(target string) {
	_ = windows.ShellExecute(0, u16("open"), u16(target), nil, nil, windows.SW_SHOWNORMAL)
}

const runKey = `Software\Microsoft\Windows\CurrentVersion\Run`

func autostartEnabled() bool {
	k, err := registry.OpenKey(registry.CURRENT_USER, runKey, registry.QUERY_VALUE)
	if err != nil {
		return false
	}
	defer k.Close()
	_, _, err = k.GetStringValue(runKeyName)
	return err == nil
}

func setAutostart(on bool) {
	k, err := registry.OpenKey(registry.CURRENT_USER, runKey, registry.SET_VALUE)
	if err != nil {
		log.Printf("autostart: %v", err)
		return
	}
	defer k.Close()
	if !on {
		_ = k.DeleteValue(runKeyName)
		return
	}
	exe, err := os.Executable()
	if err != nil {
		return
	}
	_ = k.SetStringValue(runKeyName, `"`+exe+`"`)
}

// Data polling (runs on its own goroutine; hands results to the UI thread
// through wmAppData).

func (a *App) requestRefresh() {
	select {
	case a.refreshCh <- struct{}{}:
	default:
	}
}

func (a *App) poller() {
	userAgent := fallbackUserAgent
	if !a.demo {
		userAgent = detectUserAgent()
	}
	var (
		backoff     time.Duration
		lastCLIAuth time.Time
		prev        *Usage
	)
	for {
		a.mu.Lock()
		a.lastAttempt = time.Now()
		a.mu.Unlock()

		var u *Usage
		var fe *FetchError
		if a.demo {
			u = demoUsage()
		} else {
			ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
			u, fe = fetchUsage(ctx, userAgent)
			if fe != nil && fe.Auth && time.Since(lastCLIAuth) > 30*time.Minute {
				if _, err := readAccessToken(); err == nil {
					// Expired token: let Claude Code renew it, then retry once.
					lastCLIAuth = time.Now()
					log.Printf("token rejected, running claude update")
					if err := refreshTokenViaCLI(); err != nil {
						log.Printf("claude update: %v", err)
					}
					u, fe = fetchUsage(ctx, userAgent)
				}
			}
			cancel()
		}

		a.mu.Lock()
		if fe == nil {
			a.usage, a.lastOK, a.lastErr, a.lastShort = u, time.Now(), "", ""
		} else {
			a.lastErr, a.lastShort = fe.Msg, fe.Short
			log.Printf("fetch: %s", fe.Msg)
		}
		a.version++
		a.mu.Unlock()
		postMessage(a.host, wmAppData, 0, 0)

		var delay time.Duration
		switch {
		case fe == nil:
			backoff = 0
			delay = a.successDelay(prev, u)
			prev = u
		case fe.Auth:
			delay = 5 * time.Minute
		default:
			backoff = min(max(2*backoff, 30*time.Second), 15*time.Minute)
			delay = max(backoff, fe.RetryAfter)
		}
		a.wait(delay)
	}
}

func (a *App) successDelay(prev, cur *Usage) time.Duration {
	if a.demo {
		return time.Minute
	}
	delay := time.Duration(a.cfg.PollSeconds) * time.Second
	if idleDuration() > 5*60*1000 {
		delay = time.Duration(a.cfg.IdlePollSeconds) * time.Second
	} else if prev != nil && prev.Session != nil && cur.Session != nil && cur.Session.Percent > prev.Session.Percent {
		delay = min(delay, 2*time.Minute) // actively used: follow more closely
	}
	// Pick up a reset right after it happens.
	for _, q := range []*Quota{cur.Session, cur.Weekly} {
		if q == nil || q.ResetsAt.IsZero() {
			continue
		}
		if until := time.Until(q.ResetsAt) + 5*time.Second; until > 0 && until < delay {
			delay = until
		}
	}
	return max(delay, 5*time.Second)
}

// wait sleeps until the deadline or a manual refresh. It checks the wall
// clock in short steps so a sleeping/hibernating PC catches up on resume.
func (a *App) wait(d time.Duration) {
	deadline := time.Now().Round(0).Add(d)
	for {
		left := time.Until(deadline)
		if time.Now().Round(0).After(deadline) || left <= 0 {
			return
		}
		select {
		case <-a.refreshCh:
			a.mu.Lock()
			recent := time.Since(a.lastAttempt) < 10*time.Second
			a.mu.Unlock()
			if !recent {
				return
			}
		case <-time.After(min(left, 15*time.Second)):
		}
	}
}

// Locale-aware clock and weekday formatting through the Win32 NLS API, so
// 12/24-hour and weekday names follow the user's regional settings.
func setupLocaleFormatters() {
	toST := func(t time.Time) SYSTEMTIME {
		return SYSTEMTIME{Year: uint16(t.Year()), Month: uint16(t.Month()), DayOfWeek: uint16(t.Weekday()),
			Day: uint16(t.Day()), Hour: uint16(t.Hour()), Minute: uint16(t.Minute())}
	}
	const timeNoSeconds = 0x2
	formatClock = func(t time.Time) string {
		st := toST(t.Local())
		buf := make([]uint16, 64)
		n, _, _ := procGetTimeFormatEx.Call(0, timeNoSeconds, uintptr(unsafe.Pointer(&st)), 0,
			uintptr(unsafe.Pointer(&buf[0])), uintptr(len(buf)))
		if n == 0 {
			return t.Local().Format("15:04")
		}
		return windows.UTF16ToString(buf)
	}
	formatWeekday = func(t time.Time) string {
		st := toST(t.Local())
		buf := make([]uint16, 64)
		n, _, _ := procGetDateFormatEx.Call(0, 0, uintptr(unsafe.Pointer(&st)), uintptr(unsafe.Pointer(u16("ddd"))),
			uintptr(unsafe.Pointer(&buf[0])), uintptr(len(buf)), 0)
		if n == 0 {
			return t.Local().Format("Mon")
		}
		return windows.UTF16ToString(buf)
	}
}
