package main

import (
	"math"
	"strconv"
	"strings"
	"time"
	"unsafe"

	"golang.org/x/sys/windows"
)

type RGBA struct{ R, G, B, A uint8 }

// Palette for one taskbar theme. "dark" means a dark taskbar (light text).
type Palette struct {
	Text, Dim, Track, Fill, Warn, Tick, Marker, Hover RGBA
}

var paletteDark = Palette{
	Text:   RGBA{255, 255, 255, 240},
	Dim:    RGBA{255, 255, 255, 160},
	Track:  RGBA{255, 255, 255, 48},
	Fill:   RGBA{74, 158, 255, 255},
	Warn:   RGBA{232, 88, 88, 255},
	Tick:   RGBA{0, 0, 0, 170},
	Marker: RGBA{255, 255, 255, 255},
	Hover:  RGBA{255, 255, 255, 18},
}

var paletteLight = Palette{
	Text:   RGBA{0, 0, 0, 230},
	Dim:    RGBA{0, 0, 0, 150},
	Track:  RGBA{0, 0, 0, 40},
	Fill:   RGBA{0, 103, 192, 255},
	Warn:   RGBA{196, 43, 28, 255},
	Tick:   RGBA{255, 255, 255, 210},
	Marker: RGBA{0, 0, 0, 235},
	Hover:  RGBA{0, 0, 0, 14},
}

// View is everything that determines what the widget looks like.
type View struct {
	Usage *Usage
	Now   time.Time
	Msg   string // shown instead of the bars when Usage is nil
	Stale bool
	Hover bool
	Light bool
	Cfg   Config
}

type textItem struct {
	x0, x1, cy float64
	s          string
	bold       bool
	align      uint32
	color      RGBA
}

// Renderer draws the widget into a premultiplied 32-bit DIB for
// UpdateLayeredWindow. Shapes are rasterised in Go (with supersampling for
// anti-aliasing); text is drawn by GDI in white on black into a second DIB
// whose brightness is then used as coverage, which gives correctly blended
// grayscale-antialiased text on a transparent background.
type Renderer struct {
	scale         float64
	font, bold    uintptr
	measureDC     uintptr
	w, h          int
	textDC, outDC uintptr
	textBmp       uintptr
	outBmp        uintptr
	oldText       uintptr
	oldOut        uintptr
	textBits      []uint32
	outBits       []uint32
}

func newRenderer() *Renderer {
	dc, _, _ := procCreateCompatibleDC.Call(0)
	r := &Renderer{measureDC: dc}
	r.setScale(1)
	return r
}

func (r *Renderer) setScale(scale float64) {
	if r.font != 0 && scale == r.scale {
		return
	}
	r.scale = scale
	if r.font != 0 {
		procDeleteObject.Call(r.font)
		procDeleteObject.Call(r.bold)
	}
	px := int32(math.Round(12 * scale))
	r.font = createFont(px, FW_NORMAL)
	r.bold = createFont(px, FW_SEMIBOLD)
}

var fontFace string

func createFont(px int32, weight int) uintptr {
	if fontFace == "" {
		fontFace = pickFontFace()
	}
	h, _, _ := procCreateFontW.Call(uintptr(-px), 0, 0, 0, uintptr(weight), 0, 0, 0,
		DEFAULT_CHARSET, 0, 0, ANTIALIASED_QUALITY, 0, uintptr(unsafe.Pointer(u16(fontFace))))
	return h
}

// pickFontFace prefers the Windows 11 taskbar font and falls back to Segoe UI.
func pickFontFace() string {
	want := "Segoe UI Variable Text"
	h, _, _ := procCreateFontW.Call(uintptr(^uint32(11)) /* -12 */, 0, 0, 0, FW_NORMAL, 0, 0, 0,
		DEFAULT_CHARSET, 0, 0, ANTIALIASED_QUALITY, 0, uintptr(unsafe.Pointer(u16(want))))
	defer procDeleteObject.Call(h)
	dc, _, _ := procCreateCompatibleDC.Call(0)
	defer procDeleteDC.Call(dc)
	old, _, _ := procSelectObject.Call(dc, h)
	defer procSelectObject.Call(dc, old)
	buf := make([]uint16, 64)
	procGetTextFaceW.Call(dc, uintptr(len(buf)), uintptr(unsafe.Pointer(&buf[0])))
	got := windows.UTF16ToString(buf)
	if strings.EqualFold(got, want) {
		return want
	}
	return "Segoe UI"
}

func (r *Renderer) textWidth(s string, bold bool) float64 {
	f := r.font
	if bold {
		f = r.bold
	}
	old, _, _ := procSelectObject.Call(r.measureDC, f)
	defer procSelectObject.Call(r.measureDC, old)
	if s == "" {
		return 0
	}
	p := windows.StringToUTF16(s)
	var sz SIZE
	procGetTextExtentPoint32W.Call(r.measureDC, uintptr(unsafe.Pointer(&p[0])), uintptr(len(p)-1), uintptr(unsafe.Pointer(&sz)))
	return float64(sz.CX)
}

// row holds the pre-formatted strings for one bar.
type row struct {
	label, pct, countdown, resetAt string
	q                              *Quota
}

func buildRows(v *View) []row {
	mk := func(label string, q *Quota) row {
		rw := row{label: label, q: q, pct: "–", countdown: "–"}
		if q == nil {
			return rw
		}
		rw.pct = formatPercent(q.Percent)
		if q.ResetsAt.IsZero() {
			rw.resetAt = "not started"
		} else {
			rw.countdown = fmtCountdown(q.ResetsAt.Sub(v.Now))
			rw.resetAt = fmtResetAt(q.ResetsAt, v.Now)
		}
		return rw
	}
	var rows []row
	if v.Cfg.ShowSession {
		rows = append(rows, mk("5h", v.Usage.Session))
	}
	if v.Cfg.ShowWeekly {
		rows = append(rows, mk("7d", v.Usage.Weekly))
	}
	return rows
}

func formatPercent(p float64) string {
	return strconv.Itoa(int(math.Round(p))) + "%"
}

type layout struct {
	w, h                                int
	pad, labelW, barX, barW, pctX, pctW float64
	cdX, cdW, rsX, rsW                  float64
	rowY                                []float64
}

func (r *Renderer) layout(v *View, height int) layout {
	s := r.scale
	c := v.Cfg
	L := layout{h: height, pad: math.Round(8 * s)}
	if v.Usage == nil {
		L.w = int(L.pad*2 + r.textWidth(v.Msg, false))
		return L
	}
	rows := buildRows(v)

	// Columns are laid out left to right; each one is preceded by its gap
	// unless it is the first visible column.
	x := L.pad
	first := true
	column := func(show bool, gap, width float64) float64 {
		if !show {
			return 0
		}
		if !first {
			x += math.Round(gap * s)
		}
		first = false
		start := x
		x += width
		return start
	}

	if c.ShowLabels {
		L.labelW = math.Max(r.textWidth("5h", false), r.textWidth("7d", false))
	}
	column(c.ShowLabels, 0, L.labelW)
	if c.ShowBar {
		L.barW = math.Round(float64(c.BarWidth) * s)
	}
	L.barX = column(c.ShowBar, 6, L.barW)
	if c.ShowPercent {
		L.pctW = r.textWidth("100%", true)
	}
	L.pctX = column(c.ShowPercent, 6, L.pctW)
	if c.ShowCountdown {
		L.cdW = math.Max(r.textWidth("23h 59m", false), r.textWidth("6d 23h", false))
		for _, rw := range rows {
			L.cdW = math.Max(L.cdW, r.textWidth(rw.countdown, false))
		}
	}
	L.cdX = column(c.ShowCountdown, 9, L.cdW)
	if c.ShowResetTime {
		sample := fmtResetAt(time.Date(2026, 9, 9, 22, 58, 0, 0, time.Local), time.Date(2026, 9, 1, 0, 0, 0, 0, time.Local))
		L.rsW = r.textWidth(sample, false)
		for _, rw := range rows {
			L.rsW = math.Max(L.rsW, r.textWidth(rw.resetAt, false))
		}
	}
	L.rsX = column(c.ShowResetTime, 5, L.rsW)
	x += L.pad
	L.w = int(math.Ceil(x))

	mid := float64(height) / 2
	if len(rows) == 1 {
		L.rowY = []float64{mid}
	} else {
		L.rowY = []float64{mid - math.Round(9*s), mid + math.Round(9*s)}
	}
	return L
}

func (r *Renderer) ensureBuffers(w, h int) {
	if w == r.w && h == r.h && r.outBmp != 0 {
		return
	}
	r.release()
	r.w, r.h = w, h
	r.textDC, _, _ = procCreateCompatibleDC.Call(0)
	r.outDC, _, _ = procCreateCompatibleDC.Call(0)
	r.textBmp, r.textBits = createDIB(w, h)
	r.outBmp, r.outBits = createDIB(w, h)
	r.oldText, _, _ = procSelectObject.Call(r.textDC, r.textBmp)
	r.oldOut, _, _ = procSelectObject.Call(r.outDC, r.outBmp)
	procSetBkMode.Call(r.textDC, TRANSPARENT)
	procSetTextColor.Call(r.textDC, 0x00FFFFFF)
}

func (r *Renderer) release() {
	if r.outBmp == 0 {
		return
	}
	procSelectObject.Call(r.textDC, r.oldText)
	procSelectObject.Call(r.outDC, r.oldOut)
	procDeleteObject.Call(r.textBmp)
	procDeleteObject.Call(r.outBmp)
	procDeleteDC.Call(r.textDC)
	procDeleteDC.Call(r.outDC)
	r.outBmp, r.textBmp = 0, 0
}

func createDIB(w, h int) (uintptr, []uint32) {
	bi := BITMAPINFOHEADER{
		BiSize:     uint32(unsafe.Sizeof(BITMAPINFOHEADER{})),
		BiWidth:    int32(w),
		BiHeight:   -int32(h), // top-down
		BiPlanes:   1,
		BiBitCount: 32,
	}
	var bits unsafe.Pointer
	hbmp, _, _ := procCreateDIBSection.Call(0, uintptr(unsafe.Pointer(&bi)), 0, uintptr(unsafe.Pointer(&bits)), 0, 0)
	return hbmp, unsafe.Slice((*uint32)(bits), w*h)
}

// Draw renders the view and returns the DC holding the finished bitmap.
func (r *Renderer) Draw(v *View, L layout) uintptr {
	r.ensureBuffers(L.w, L.h)
	pal := paletteDark
	if v.Light {
		pal = paletteLight
	}
	for i := range r.outBits {
		r.outBits[i] = 0
	}
	s := r.scale
	W, H := float64(L.w), float64(L.h)

	if v.Hover {
		inset := math.Round(3 * s)
		r.fillShape(1, inset, W-1, H-inset, 4*s, W, pal.Hover)
	}

	var texts []textItem
	if v.Usage == nil {
		texts = append(texts, textItem{x0: L.pad, x1: W - L.pad, cy: H / 2, s: v.Msg, align: DT_LEFT, color: pal.Dim})
	} else {
		for i, rw := range buildRows(v) {
			cy := L.rowY[i]
			c := v.Cfg
			if c.ShowLabels {
				texts = append(texts, textItem{x0: L.pad, x1: L.pad + L.labelW, cy: cy, s: rw.label, align: DT_LEFT, color: pal.Dim})
			}
			if c.ShowBar {
				r.drawBar(rw.q, v, L.barX, cy, L.barW, pal)
			}
			if c.ShowPercent {
				texts = append(texts, textItem{x0: L.pctX, x1: L.pctX + L.pctW, cy: cy, s: rw.pct, bold: true, align: DT_RIGHT, color: pal.Text})
			}
			if c.ShowCountdown {
				texts = append(texts, textItem{x0: L.cdX, x1: L.cdX + L.cdW, cy: cy, s: rw.countdown, align: DT_LEFT, color: pal.Text})
			}
			if c.ShowResetTime {
				texts = append(texts, textItem{x0: L.rsX, x1: L.rsX + L.rsW, cy: cy, s: rw.resetAt, align: DT_LEFT, color: pal.Dim})
			}
		}
	}
	r.drawTexts(texts, pal.Text)
	r.drawTexts(texts, pal.Dim)

	// Stale data is shown faded. Every pixel keeps a minimal alpha so the
	// whole rectangle stays clickable (fully transparent pixels let mouse
	// input fall through to the taskbar).
	fade := 1.0
	if v.Stale {
		fade = 0.5
	}
	for i, p := range r.outBits {
		if fade != 1 {
			p = scalePixel(p, fade)
		}
		if p>>24 == 0 {
			p = 0x01000000
		}
		r.outBits[i] = p
	}
	return r.outDC
}

func (r *Renderer) drawBar(q *Quota, v *View, x, cy, w float64, pal Palette) {
	s := r.scale
	h := math.Max(4, math.Round(6*s))
	y := math.Round(cy - h/2)
	rad := h / 2
	r.fillShape(x, y, x+w, y+h, rad, x+w, pal.Track)
	if q == nil {
		return
	}
	fill := pal.Fill
	if v.Cfg.WarnWhenOutpacing && outpacing(q, v.Now) {
		fill = pal.Warn
	}
	pct := math.Max(0, math.Min(100, q.Percent))
	if pct > 0 {
		r.fillShape(x, y, x+w, y+h, rad, x+w*pct/100, fill)
	}
	if v.Cfg.ShowTicks {
		tw := math.Max(1, math.Round(s))
		for _, t := range tickPositions(q) {
			tx := math.Round(x + w*t - tw/2)
			r.fillShape(tx, y, tx+tw, y+h, 0, tx+tw, pal.Tick)
		}
	}
	if f := elapsedFraction(q, v.Now); f >= 0 && v.Cfg.ShowMarker {
		mw := math.Max(2, math.Round(2*s))
		ext := math.Round(3 * s)
		mx := math.Round(x + w*f - mw/2)
		mx = math.Max(x-mw/2, math.Min(x+w-mw/2, mx))
		r.fillShape(mx, y-ext, mx+mw, y+h+ext, mw/2, mx+mw, pal.Marker)
	}
}

// fillShape fills a rounded rectangle, clipped at clipX, with 4x4
// supersampled coverage.
func (r *Renderer) fillShape(x0, y0, x1, y1, rad, clipX float64, c RGBA) {
	ix0 := int(math.Max(0, math.Floor(x0)))
	iy0 := int(math.Max(0, math.Floor(y0)))
	ix1 := int(math.Min(float64(r.w), math.Ceil(math.Min(x1, clipX))))
	iy1 := int(math.Min(float64(r.h), math.Ceil(y1)))
	inside := func(sx, sy float64) bool {
		if sx < x0 || sx >= x1 || sx >= clipX || sy < y0 || sy >= y1 {
			return false
		}
		if rad <= 0 {
			return true
		}
		cx := math.Max(x0+rad, math.Min(x1-rad, sx))
		cy := math.Max(y0+rad, math.Min(y1-rad, sy))
		dx, dy := sx-cx, sy-cy
		return dx*dx+dy*dy <= rad*rad
	}
	for py := iy0; py < iy1; py++ {
		for px := ix0; px < ix1; px++ {
			n := 0
			for sy := 0; sy < 4; sy++ {
				for sx := 0; sx < 4; sx++ {
					if inside(float64(px)+(float64(sx)+0.5)/4, float64(py)+(float64(sy)+0.5)/4) {
						n++
					}
				}
			}
			if n > 0 {
				i := py*r.w + px
				r.outBits[i] = blend(r.outBits[i], c, float64(n)/16)
			}
		}
	}
}

// drawTexts renders all items of one color through the GDI coverage buffer.
func (r *Renderer) drawTexts(items []textItem, color RGBA) {
	any := false
	for i := range r.textBits {
		r.textBits[i] = 0
	}
	for _, t := range items {
		if t.color != color || t.s == "" {
			continue
		}
		any = true
		f := r.font
		if t.bold {
			f = r.bold
		}
		old, _, _ := procSelectObject.Call(r.textDC, f)
		half := math.Round(10 * r.scale)
		rc := RECT{int32(math.Round(t.x0)), int32(math.Round(t.cy - half)), int32(math.Round(t.x1)), int32(math.Round(t.cy + half))}
		p := windows.StringToUTF16(t.s)
		procDrawTextW.Call(r.textDC, uintptr(unsafe.Pointer(&p[0])), uintptr(len(p)-1), uintptr(unsafe.Pointer(&rc)),
			uintptr(t.align|DT_VCENTER|DT_SINGLELINE|DT_NOPREFIX|DT_NOCLIP))
		procSelectObject.Call(r.textDC, old)
	}
	if !any {
		return
	}
	procGdiFlush.Call()
	for i, p := range r.textBits {
		if p&0xFFFFFF == 0 {
			continue
		}
		cov := float64((p>>16)&0xFF+(p>>8)&0xFF+p&0xFF) / (3 * 255)
		r.outBits[i] = blend(r.outBits[i], color, cov)
	}
}

// blend composites a straight-alpha color with coverage over a
// premultiplied BGRA pixel.
func blend(dst uint32, c RGBA, cov float64) uint32 {
	a := float64(c.A) / 255 * cov
	if a <= 0 {
		return dst
	}
	inv := 1 - a
	da := float64(dst>>24) * inv
	dr := float64((dst>>16)&0xFF) * inv
	dg := float64((dst>>8)&0xFF) * inv
	db := float64(dst&0xFF) * inv
	oa := clamp255(255*a + da)
	or := clamp255(float64(c.R)*a + dr)
	og := clamp255(float64(c.G)*a + dg)
	ob := clamp255(float64(c.B)*a + db)
	return oa<<24 | or<<16 | og<<8 | ob
}

func scalePixel(p uint32, f float64) uint32 {
	a := clamp255(float64(p>>24) * f)
	rr := clamp255(float64((p>>16)&0xFF) * f)
	g := clamp255(float64((p>>8)&0xFF) * f)
	b := clamp255(float64(p&0xFF) * f)
	return a<<24 | rr<<16 | g<<8 | b
}

func clamp255(v float64) uint32 {
	if v <= 0 {
		return 0
	}
	if v >= 255 {
		return 255
	}
	return uint32(v + 0.5)
}
