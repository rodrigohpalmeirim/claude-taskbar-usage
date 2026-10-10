package main

import "testing"

func TestToggleLocked(t *testing.T) {
	c := defaultConfig()
	if toggleLocked(&c, &c.ShowSession) || toggleLocked(&c, &c.ShowBar) {
		t.Fatal("nothing should be locked with everything visible")
	}
	c.ShowWeekly = false
	if !toggleLocked(&c, &c.ShowSession) {
		t.Error("last visible row must be locked")
	}
	if toggleLocked(&c, &c.ShowWeekly) {
		t.Error("a hidden row can always be shown again")
	}
	c.ShowPercent, c.ShowCountdown, c.ShowResetTime = false, false, false
	if !toggleLocked(&c, &c.ShowBar) {
		t.Error("last visible column must be locked")
	}
	c.ShowPercent, c.ShowBar = true, false
	if !toggleLocked(&c, &c.ShowTicks) || !toggleLocked(&c, &c.ShowMarker) {
		t.Error("bar decorations must be locked while the bar is hidden")
	}
}
