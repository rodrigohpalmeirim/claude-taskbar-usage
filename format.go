package main

import (
	"fmt"
	"math"
	"time"
)

// Locale-aware formatters; replaced with Win32 implementations on Windows.
var (
	formatClock   = func(t time.Time) string { return t.Format("15:04") }
	formatWeekday = func(t time.Time) string { return t.Format("Mon") }
)

// fmtCountdown renders the time left until a reset: "42m", "3h 05m", "2d 4h".
func fmtCountdown(d time.Duration) string {
	if d <= 0 {
		return "now"
	}
	mins := int(math.Ceil(d.Minutes()))
	switch {
	case mins < 60:
		return fmt.Sprintf("%dm", mins)
	case mins < 24*60:
		return fmt.Sprintf("%dh %02dm", mins/60, mins%60)
	default:
		hours := mins / 60
		return fmt.Sprintf("%dd %dh", hours/24, hours%24)
	}
}

// fmtResetAt renders when a reset happens: "14:59" today, otherwise "Fri 21:00".
func fmtResetAt(reset, now time.Time) string {
	reset = roundMinute(reset).Local()
	now = now.Local()
	if sameDay(reset, now) {
		return formatClock(reset)
	}
	return formatWeekday(reset) + " " + formatClock(reset)
}

// fmtResetAtLong is the tooltip variant: "today 14:59", "tomorrow 09:00", "Fri 21:00".
func fmtResetAtLong(reset, now time.Time) string {
	reset = roundMinute(reset).Local()
	now = now.Local()
	switch {
	case sameDay(reset, now):
		return "today " + formatClock(reset)
	case sameDay(reset, now.AddDate(0, 0, 1)):
		return "tomorrow " + formatClock(reset)
	default:
		return formatWeekday(reset) + " " + formatClock(reset)
	}
}

// The API reports resets like 20:59:59.97; show them as 21:00.
func roundMinute(t time.Time) time.Time { return t.Add(30 * time.Second).Truncate(time.Minute) }

func sameDay(a, b time.Time) bool {
	ay, am, ad := a.Date()
	by, bm, bd := b.Date()
	return ay == by && am == bm && ad == bd
}

// elapsedFraction is how far "now" is into the quota window (0..1), or -1
// when the window has not started.
func elapsedFraction(q *Quota, now time.Time) float64 {
	if q == nil || q.ResetsAt.IsZero() || q.Period <= 0 {
		return -1
	}
	left := q.ResetsAt.Sub(now)
	f := 1 - float64(left)/float64(q.Period)
	return math.Max(0, math.Min(1, f))
}

// tickPositions returns divider positions (0..1, exclusive) inside a quota
// bar: hour marks for the 5-hour window, local midnights for windows of a
// day or longer.
func tickPositions(q *Quota) []float64 {
	if q == nil || q.Period <= 0 {
		return nil
	}
	if q.Period < 24*time.Hour {
		hours := int(q.Period / time.Hour)
		if hours < 2 || hours > 12 {
			return nil
		}
		ticks := make([]float64, 0, hours-1)
		for i := 1; i < hours; i++ {
			ticks = append(ticks, float64(i)/float64(hours))
		}
		return ticks
	}
	if q.ResetsAt.IsZero() {
		// Window not started: evenly spaced day marks.
		days := int(q.Period / (24 * time.Hour))
		ticks := make([]float64, 0, days)
		for i := 1; i < days; i++ {
			ticks = append(ticks, float64(i)/float64(days))
		}
		return ticks
	}
	end := q.ResetsAt.Local()
	start := end.Add(-q.Period)
	var ticks []float64
	// Build each midnight from the calendar date so DST changes stay on true local midnights.
	for d := 1; ; d++ {
		midnight := time.Date(start.Year(), start.Month(), start.Day()+d, 0, 0, 0, 0, time.Local)
		if !midnight.Before(end) {
			break
		}
		rel := float64(midnight.Sub(start)) / float64(q.Period)
		if rel > 0.003 && rel < 0.997 {
			ticks = append(ticks, rel)
		}
	}
	return ticks
}

// outpacing reports whether usage is ahead of the clock (or exhausted).
func outpacing(q *Quota, now time.Time) bool {
	if q == nil {
		return false
	}
	if q.Percent >= 100 {
		return true
	}
	f := elapsedFraction(q, now)
	return f >= 0 && q.Percent > f*100
}
