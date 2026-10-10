package main

import (
	"encoding/json"
	"os"
	"path/filepath"
)

// Config lives in %APPDATA%\ClaudeTaskbarUsage\config.json. Every field is
// optional; missing ones fall back to the defaults below.
type Config struct {
	// Distance in 96-DPI pixels between the widget and the tray icons.
	// Set by dragging the widget along the taskbar.
	Offset int `json:"offset"`
	// Width of each bar in 96-DPI pixels.
	BarWidth int `json:"bar_width"`
	// Seconds between API polls while the computer is in use.
	PollSeconds int `json:"poll_seconds"`
	// Seconds between API polls after 5 minutes without input.
	IdlePollSeconds int `json:"idle_poll_seconds"`
	// Which elements to show. All can also be toggled from the widget's
	// right-click menu (Show submenu).
	ShowSession   bool `json:"show_session"`    // 5-hour row
	ShowWeekly    bool `json:"show_weekly"`     // 7-day row
	ShowLabels    bool `json:"show_labels"`     // "5h" / "7d" in front of the bars
	ShowBar       bool `json:"show_bar"`        // the progress bars
	ShowTicks     bool `json:"show_ticks"`      // hour / day marks on the bars
	ShowMarker    bool `json:"show_marker"`     // current-time marker on the bars
	ShowPercent   bool `json:"show_percent"`    // "48%"
	ShowCountdown bool `json:"show_countdown"`  // "2h 13m"
	ShowResetTime bool `json:"show_reset_time"` // "14:59" / "Fri 21:00"
	// Turn the bar red when usage runs ahead of the elapsed time.
	WarnWhenOutpacing bool `json:"warn_when_outpacing"`
	// "auto" (follow Windows), "dark" (light text) or "light" (dark text).
	Theme string `json:"theme"`
}

func defaultConfig() Config {
	return Config{
		BarWidth:          110,
		PollSeconds:       180,
		IdlePollSeconds:   900,
		ShowSession:       true,
		ShowWeekly:        true,
		ShowLabels:        true,
		ShowBar:           true,
		ShowTicks:         true,
		ShowMarker:        true,
		ShowPercent:       true,
		ShowCountdown:     true,
		ShowResetTime:     true,
		WarnWhenOutpacing: true,
		Theme:             "auto",
	}
}

func configPath() string {
	dir, err := os.UserConfigDir()
	if err != nil {
		return ""
	}
	return filepath.Join(dir, "ClaudeTaskbarUsage", "config.json")
}

func loadConfig() Config {
	c := defaultConfig()
	if raw, err := os.ReadFile(configPath()); err == nil {
		_ = json.Unmarshal(raw, &c)
	}
	if c.BarWidth < 40 {
		c.BarWidth = 40
	}
	if c.PollSeconds < 60 {
		c.PollSeconds = 60
	}
	if c.IdlePollSeconds < c.PollSeconds {
		c.IdlePollSeconds = c.PollSeconds
	}
	// Never end up with an empty widget from a hand-edited file.
	if !c.ShowSession && !c.ShowWeekly {
		c.ShowSession, c.ShowWeekly = true, true
	}
	if !c.ShowBar && !c.ShowPercent && !c.ShowCountdown && !c.ShowResetTime {
		c.ShowBar, c.ShowPercent = true, true
	}
	return c
}

func saveConfig(c Config) error {
	p := configPath()
	if err := os.MkdirAll(filepath.Dir(p), 0o755); err != nil {
		return err
	}
	raw, _ := json.MarshalIndent(c, "", "  ")
	return os.WriteFile(p, raw, 0o644)
}
