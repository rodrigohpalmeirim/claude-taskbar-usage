//go:build windows

package main

import (
	"syscall"
	"unsafe"

	"golang.org/x/sys/windows"
)

// Minimal Win32 bindings. Only what the widget needs.

var (
	user32   = windows.NewLazySystemDLL("user32.dll")
	gdi32    = windows.NewLazySystemDLL("gdi32.dll")
	kernel32 = windows.NewLazySystemDLL("kernel32.dll")
	comctl32 = windows.NewLazySystemDLL("comctl32.dll")

	procRegisterClassExW      = user32.NewProc("RegisterClassExW")
	procCreateWindowExW       = user32.NewProc("CreateWindowExW")
	procDefWindowProcW        = user32.NewProc("DefWindowProcW")
	procDestroyWindow         = user32.NewProc("DestroyWindow")
	procGetMessageW           = user32.NewProc("GetMessageW")
	procTranslateMessage      = user32.NewProc("TranslateMessage")
	procDispatchMessageW      = user32.NewProc("DispatchMessageW")
	procPostMessageW          = user32.NewProc("PostMessageW")
	procSendMessageW          = user32.NewProc("SendMessageW")
	procPostQuitMessage       = user32.NewProc("PostQuitMessage")
	procFindWindowW           = user32.NewProc("FindWindowW")
	procFindWindowExW         = user32.NewProc("FindWindowExW")
	procGetWindowRect         = user32.NewProc("GetWindowRect")
	procScreenToClient        = user32.NewProc("ScreenToClient")
	procSetWindowPos          = user32.NewProc("SetWindowPos")
	procShowWindow            = user32.NewProc("ShowWindow")
	procIsWindow              = user32.NewProc("IsWindow")
	procUpdateLayeredWindow   = user32.NewProc("UpdateLayeredWindow")
	procSetTimer              = user32.NewProc("SetTimer")
	procSetCapture            = user32.NewProc("SetCapture")
	procReleaseCapture        = user32.NewProc("ReleaseCapture")
	procGetCursorPos          = user32.NewProc("GetCursorPos")
	procTrackMouseEvent       = user32.NewProc("TrackMouseEvent")
	procCreatePopupMenu       = user32.NewProc("CreatePopupMenu")
	procAppendMenuW           = user32.NewProc("AppendMenuW")
	procTrackPopupMenu        = user32.NewProc("TrackPopupMenu")
	procDestroyMenu           = user32.NewProc("DestroyMenu")
	procSetForegroundWindow   = user32.NewProc("SetForegroundWindow")
	procLoadCursorW           = user32.NewProc("LoadCursorW")
	procGetDpiForWindow       = user32.NewProc("GetDpiForWindow")
	procGetLastInputInfo      = user32.NewProc("GetLastInputInfo")
	procMessageBoxW           = user32.NewProc("MessageBoxW")
	procCreateCompatibleDC    = gdi32.NewProc("CreateCompatibleDC")
	procCreateDIBSection      = gdi32.NewProc("CreateDIBSection")
	procSelectObject          = gdi32.NewProc("SelectObject")
	procDeleteObject          = gdi32.NewProc("DeleteObject")
	procDeleteDC              = gdi32.NewProc("DeleteDC")
	procCreateFontW           = gdi32.NewProc("CreateFontW")
	procSetTextColor          = gdi32.NewProc("SetTextColor")
	procSetBkMode             = gdi32.NewProc("SetBkMode")
	procDrawTextW             = user32.NewProc("DrawTextW")
	procGetTextExtentPoint32W = gdi32.NewProc("GetTextExtentPoint32W")
	procGetTextFaceW          = gdi32.NewProc("GetTextFaceW")
	procGdiFlush              = gdi32.NewProc("GdiFlush")
	procGetModuleHandleW      = kernel32.NewProc("GetModuleHandleW")
	procGetTickCount          = kernel32.NewProc("GetTickCount")
	procGetTimeFormatEx       = kernel32.NewProc("GetTimeFormatEx")
	procGetDateFormatEx       = kernel32.NewProc("GetDateFormatEx")
	procInitCommonControlsEx  = comctl32.NewProc("InitCommonControlsEx")
)

const (
	WS_POPUP        = 0x80000000
	WS_CHILD        = 0x40000000
	WS_VISIBLE      = 0x10000000
	WS_CLIPSIBLINGS = 0x04000000

	WS_EX_TOPMOST    = 0x00000008
	WS_EX_TOOLWINDOW = 0x00000080
	WS_EX_LAYERED    = 0x00080000
	WS_EX_NOACTIVATE = 0x08000000

	WM_DESTROY       = 0x0002
	WM_CLOSE         = 0x0010
	WM_SETTINGCHANGE = 0x001A
	WM_TIMER         = 0x0113
	WM_COMMAND       = 0x0111
	WM_MOUSEMOVE     = 0x0200
	WM_LBUTTONDOWN   = 0x0201
	WM_LBUTTONUP     = 0x0202
	WM_LBUTTONDBLCLK = 0x0203
	WM_RBUTTONUP     = 0x0205
	WM_MOUSELEAVE    = 0x02A3
	WM_NULL          = 0x0000
	WM_APP           = 0x8000
	WM_USER          = 0x0400

	SWP_NOSIZE        = 0x0001
	SWP_NOMOVE        = 0x0002
	SWP_NOACTIVATE    = 0x0010
	SWP_SHOWWINDOW    = 0x0040
	SWP_NOOWNERZORDER = 0x0200

	SW_HIDE           = 0
	SW_SHOWNOACTIVATE = 4

	ULW_ALPHA    = 0x2
	AC_SRC_OVER  = 0x0
	AC_SRC_ALPHA = 0x1

	TME_LEAVE = 0x2

	MF_STRING    = 0x0
	MF_SEPARATOR = 0x800
	MF_CHECKED   = 0x8
	MF_GRAYED    = 0x1
	MF_POPUP     = 0x10

	TPM_RIGHTBUTTON = 0x0002
	TPM_RETURNCMD   = 0x0100
	TPM_BOTTOMALIGN = 0x0020
	TPM_RIGHTALIGN  = 0x0008

	DT_LEFT       = 0x0
	DT_RIGHT      = 0x2
	DT_CENTER     = 0x1
	DT_VCENTER    = 0x4
	DT_SINGLELINE = 0x20
	DT_NOPREFIX   = 0x800
	DT_NOCLIP     = 0x100

	TRANSPARENT = 1

	FW_NORMAL           = 400
	FW_SEMIBOLD         = 600
	DEFAULT_CHARSET     = 1
	ANTIALIASED_QUALITY = 4

	IDC_ARROW = 32512

	HWND_TOP     = 0
	HWND_TOPMOST = ^uintptr(0) // -1

	GA_ROOT = 2

	TTS_ALWAYSTIP      = 0x01
	TTS_NOPREFIX       = 0x02
	TTF_IDISHWND       = 0x0001
	TTF_SUBCLASS       = 0x0010
	TTM_ADDTOOLW       = WM_USER + 50
	TTM_UPDATETIPTEXTW = WM_USER + 57
	TTM_SETMAXTIPWIDTH = WM_USER + 24
	TTM_SETDELAYTIME   = WM_USER + 3
	TTDT_AUTOPOP       = 2
	ICC_WIN95_CLASSES  = 0xFF

	MB_OK        = 0x0
	MB_ICONERROR = 0x10
)

type POINT struct{ X, Y int32 }
type SIZE struct{ CX, CY int32 }
type RECT struct{ Left, Top, Right, Bottom int32 }

func (r RECT) W() int32 { return r.Right - r.Left }
func (r RECT) H() int32 { return r.Bottom - r.Top }

type WNDCLASSEXW struct {
	CbSize        uint32
	Style         uint32
	LpfnWndProc   uintptr
	CbClsExtra    int32
	CbWndExtra    int32
	HInstance     uintptr
	HIcon         uintptr
	HCursor       uintptr
	HbrBackground uintptr
	LpszMenuName  *uint16
	LpszClassName *uint16
	HIconSm       uintptr
}

type MSG struct {
	Hwnd     uintptr
	Message  uint32
	WParam   uintptr
	LParam   uintptr
	Time     uint32
	Pt       POINT
	LPrivate uint32
}

type BITMAPINFOHEADER struct {
	BiSize          uint32
	BiWidth         int32
	BiHeight        int32
	BiPlanes        uint16
	BiBitCount      uint16
	BiCompression   uint32
	BiSizeImage     uint32
	BiXPelsPerMeter int32
	BiYPelsPerMeter int32
	BiClrUsed       uint32
	BiClrImportant  uint32
}

type BLENDFUNCTION struct {
	BlendOp             byte
	BlendFlags          byte
	SourceConstantAlpha byte
	AlphaFormat         byte
}

type TRACKMOUSEEVENT struct {
	CbSize      uint32
	DwFlags     uint32
	HwndTrack   uintptr
	DwHoverTime uint32
}

type TOOLINFOW struct {
	CbSize     uint32
	UFlags     uint32
	Hwnd       uintptr
	UId        uintptr
	Rect       RECT
	Hinst      uintptr
	LpszText   *uint16
	LParam     uintptr
	LpReserved uintptr
}

type INITCOMMONCONTROLSEX struct {
	DwSize uint32
	DwICC  uint32
}

type LASTINPUTINFO struct {
	CbSize uint32
	DwTime uint32
}

type SYSTEMTIME struct {
	Year, Month, DayOfWeek, Day, Hour, Minute, Second, Milliseconds uint16
}

func u16(s string) *uint16 {
	p, _ := syscall.UTF16PtrFromString(s)
	return p
}

func findWindow(class string) uintptr {
	h, _, _ := procFindWindowW.Call(uintptr(unsafe.Pointer(u16(class))), 0)
	return h
}

func findChild(parent uintptr, class string) uintptr {
	h, _, _ := procFindWindowExW.Call(parent, 0, uintptr(unsafe.Pointer(u16(class))), 0)
	return h
}

func windowRect(h uintptr) (RECT, bool) {
	var r RECT
	ok, _, _ := procGetWindowRect.Call(h, uintptr(unsafe.Pointer(&r)))
	return r, ok != 0
}

func isWindow(h uintptr) bool {
	if h == 0 {
		return false
	}
	r, _, _ := procIsWindow.Call(h)
	return r != 0
}

func setWindowPos(h, after uintptr, x, y, w, ht int32, flags uint32) {
	procSetWindowPos.Call(h, after, uintptr(x), uintptr(y), uintptr(w), uintptr(ht), uintptr(flags))
}

func showWindow(h uintptr, cmd int) {
	procShowWindow.Call(h, uintptr(cmd))
}

func postMessage(h uintptr, msg uint32, w, l uintptr) {
	procPostMessageW.Call(h, uintptr(msg), w, l)
}

func sendMessage(h uintptr, msg uint32, w, l uintptr) uintptr {
	r, _, _ := procSendMessageW.Call(h, uintptr(msg), w, l)
	return r
}

func cursorPos() POINT {
	var p POINT
	procGetCursorPos.Call(uintptr(unsafe.Pointer(&p)))
	return p
}

func screenToClient(h uintptr, p POINT) POINT {
	procScreenToClient.Call(h, uintptr(unsafe.Pointer(&p)))
	return p
}

func dpiForWindow(h uintptr) uint32 {
	if procGetDpiForWindow.Find() != nil {
		return 96
	}
	r, _, _ := procGetDpiForWindow.Call(h)
	if r == 0 {
		return 96
	}
	return uint32(r)
}

func idleDuration() uint32 {
	li := LASTINPUTINFO{CbSize: uint32(unsafe.Sizeof(LASTINPUTINFO{}))}
	procGetLastInputInfo.Call(uintptr(unsafe.Pointer(&li)))
	now, _, _ := procGetTickCount.Call()
	return uint32(now) - li.DwTime
}

func messageBox(text, title string, flags uint32) {
	procMessageBoxW.Call(0, uintptr(unsafe.Pointer(u16(text))), uintptr(unsafe.Pointer(u16(title))), uintptr(flags))
}

func loWord(l uintptr) int16 { return int16(l & 0xffff) }
func hiWord(l uintptr) int16 { return int16((l >> 16) & 0xffff) }
