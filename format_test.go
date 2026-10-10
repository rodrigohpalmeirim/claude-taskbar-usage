package main

import (
	"testing"
	"time"
)

func TestFmtCountdown(t *testing.T) {
	cases := map[time.Duration]string{
		-time.Second:                    "now",
		30 * time.Second:                "1m",
		42*time.Minute + 10*time.Second: "43m",
		3*time.Hour + 5*time.Minute:     "3h 05m",
		2*24*time.Hour + 4*time.Hour:    "2d 4h",
	}
	for d, want := range cases {
		if got := fmtCountdown(d); got != want {
			t.Errorf("fmtCountdown(%v) = %q, want %q", d, got, want)
		}
	}
}

func TestFmtResetAtRoundsUp(t *testing.T) {
	now := time.Date(2026, 9, 5, 10, 0, 0, 0, time.Local)
	if got := fmtResetAt(time.Date(2026, 9, 5, 12, 59, 59, 900000000, time.Local), now); got != "13:00" {
		t.Errorf("same day: %q", got)
	}
	if got := fmtResetAt(time.Date(2026, 9, 11, 20, 59, 59, 0, time.Local), now); got != "Fri 21:00" {
		t.Errorf("other day: %q", got)
	}
}

func TestTicks(t *testing.T) {
	five := &Quota{Period: period5h, ResetsAt: time.Now().Add(time.Hour)}
	if got := tickPositions(five); len(got) != 4 || got[0] != 0.2 {
		t.Errorf("5h ticks: %v", got)
	}
	// Weekly window ending at a non-midnight local time has 7 midnights inside.
	end := time.Date(2026, 9, 11, 21, 0, 0, 0, time.Local)
	week := &Quota{Period: period7d, ResetsAt: end}
	ticks := tickPositions(week)
	if len(ticks) != 7 {
		t.Fatalf("weekly ticks: %v", ticks)
	}
	first := end.Add(-period7d)
	wantFirst := float64(time.Date(2026, 9, 5, 0, 0, 0, 0, time.Local).Sub(first)) / float64(period7d)
	if ticks[0] != wantFirst {
		t.Errorf("first tick %v, want %v", ticks[0], wantFirst)
	}
}

func TestOutpacing(t *testing.T) {
	now := time.Now()
	q := &Quota{Percent: 30, Period: period5h, ResetsAt: now.Add(4 * time.Hour)} // 20% elapsed
	if !outpacing(q, now) {
		t.Error("30% used at 20% elapsed should be outpacing")
	}
	q.Percent = 10
	if outpacing(q, now) {
		t.Error("10% used at 20% elapsed should not be outpacing")
	}
}
