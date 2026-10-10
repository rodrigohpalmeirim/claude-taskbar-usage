package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"time"
)

// The only network destination. The OAuth token never leaves the
// Authorization header and is never logged.
const usageURL = "https://api.anthropic.com/api/oauth/usage"

const fallbackUserAgent = "claude-code/2.1.204"

const (
	period5h = 5 * time.Hour
	period7d = 7 * 24 * time.Hour
)

// Quota is one rate-limit window as reported by the API.
type Quota struct {
	Name     string
	Percent  float64
	ResetsAt time.Time // zero when the window has not started yet
	Period   time.Duration
}

// Usage is the subset of /api/oauth/usage the widget shows.
type Usage struct {
	Session *Quota   // five_hour
	Weekly  *Quota   // seven_day
	Others  []*Quota // model-scoped and other quotas, tooltip only
}

// FetchError carries enough detail to choose a retry strategy.
type FetchError struct {
	Short       string // fits in the taskbar
	Msg         string // tooltip detail
	Auth        bool
	RateLimited bool
	RetryAfter  time.Duration
}

func (e *FetchError) Error() string { return e.Msg }

func claudeConfigDir() string {
	if d := os.Getenv("CLAUDE_CONFIG_DIR"); d != "" {
		return d
	}
	home, _ := os.UserHomeDir()
	return filepath.Join(home, ".claude")
}

func readAccessToken() (string, error) {
	raw, err := os.ReadFile(filepath.Join(claudeConfigDir(), ".credentials.json"))
	if err != nil {
		return "", err
	}
	var creds struct {
		ClaudeAiOauth struct {
			AccessToken string `json:"accessToken"`
		} `json:"claudeAiOauth"`
	}
	if err := json.Unmarshal(raw, &creds); err != nil {
		return "", err
	}
	if creds.ClaudeAiOauth.AccessToken == "" {
		return "", errors.New("no access token")
	}
	return creds.ClaudeAiOauth.AccessToken, nil
}

var httpClient = &http.Client{Timeout: 15 * time.Second}

func fetchUsage(ctx context.Context, userAgent string) (*Usage, *FetchError) {
	token, err := readAccessToken()
	if err != nil {
		return nil, &FetchError{Short: "not logged in", Msg: "Not logged in to Claude Code (run: claude auth login)", Auth: true}
	}

	req, _ := http.NewRequestWithContext(ctx, http.MethodGet, usageURL, nil)
	req.Header.Set("Authorization", "Bearer "+token)
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("User-Agent", userAgent)
	req.Header.Set("anthropic-beta", "oauth-2025-04-20")

	resp, err := httpClient.Do(req)
	if err != nil {
		return nil, &FetchError{Short: "offline", Msg: "Connection error"}
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(io.LimitReader(resp.Body, 1<<20))

	switch {
	case resp.StatusCode == 401:
		return nil, &FetchError{Short: "login expired", Msg: "Login expired (run Claude Code once to renew it)", Auth: true}
	case resp.StatusCode == 429:
		fe := &FetchError{Short: "rate limited", Msg: "Rate limited by the API, retrying later", RateLimited: true}
		if s, err := strconv.Atoi(resp.Header.Get("Retry-After")); err == nil && s > 0 {
			fe.RetryAfter = time.Duration(s) * time.Second
		}
		return nil, fe
	case resp.StatusCode >= 500:
		return nil, &FetchError{Short: "server error", Msg: fmt.Sprintf("Server error %d", resp.StatusCode)}
	case resp.StatusCode != 200:
		return nil, &FetchError{Short: fmt.Sprintf("error %d", resp.StatusCode), Msg: fmt.Sprintf("HTTP error %d", resp.StatusCode)}
	}

	u, err := parseUsage(body)
	if err != nil {
		return nil, &FetchError{Short: "bad response", Msg: "Unexpected API response"}
	}
	return u, nil
}

type rawQuota struct {
	Utilization *float64 `json:"utilization"`
	ResetsAt    *string  `json:"resets_at"`
}

type rawLimit struct {
	Kind     string  `json:"kind"`
	Group    string  `json:"group"`
	Percent  float64 `json:"percent"`
	ResetsAt *string `json:"resets_at"`
	Scope    *struct {
		Model string `json:"model"`
	} `json:"scope"`
}

// Friendly names for known per-model / per-product weekly quotas. Unknown
// code-named quotas are skipped: they have no public meaning yet.
var otherQuotaNames = map[string]string{
	"seven_day_sonnet":     "Weekly Sonnet",
	"seven_day_opus":       "Weekly Opus",
	"seven_day_cowork":     "Weekly Cowork",
	"seven_day_oauth_apps": "Weekly OAuth apps",
}

func parseUsage(body []byte) (*Usage, error) {
	var fields map[string]json.RawMessage
	if err := json.Unmarshal(body, &fields); err != nil {
		return nil, err
	}

	quota := func(key, name string, period time.Duration) *Quota {
		raw, ok := fields[key]
		if !ok {
			return nil
		}
		var rq *rawQuota
		if json.Unmarshal(raw, &rq) != nil || rq == nil || rq.Utilization == nil {
			return nil
		}
		q := &Quota{Name: name, Percent: *rq.Utilization, Period: period}
		if rq.ResetsAt != nil {
			q.ResetsAt = parseTime(*rq.ResetsAt)
		}
		return q
	}

	u := &Usage{
		Session: quota("five_hour", "Session (5h)", period5h),
		Weekly:  quota("seven_day", "Weekly (7d)", period7d),
	}
	if u.Session == nil && u.Weekly == nil {
		return nil, errors.New("no quota fields")
	}

	seen := map[string]bool{}
	keys := make([]string, 0, len(otherQuotaNames))
	for k := range otherQuotaNames {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	for _, k := range keys {
		if q := quota(k, otherQuotaNames[k], period7d); q != nil {
			u.Others = append(u.Others, q)
			seen[strings.ToLower(q.Name)] = true
		}
	}

	// Model-scoped weekly limits only appear in the "limits" array.
	var limits []rawLimit
	if raw, ok := fields["limits"]; ok && json.Unmarshal(raw, &limits) == nil {
		for _, l := range limits {
			if l.Scope == nil || l.Scope.Model == "" {
				continue
			}
			name := "Weekly " + titleCase(l.Scope.Model)
			if seen[strings.ToLower(name)] {
				continue
			}
			q := &Quota{Name: name, Percent: l.Percent, Period: period7d}
			if l.ResetsAt != nil {
				q.ResetsAt = parseTime(*l.ResetsAt)
			}
			u.Others = append(u.Others, q)
			seen[strings.ToLower(name)] = true
		}
	}
	return u, nil
}

func parseTime(s string) time.Time {
	t, err := time.Parse(time.RFC3339Nano, s)
	if err != nil {
		return time.Time{}
	}
	return t
}

func titleCase(s string) string {
	if s == "" {
		return s
	}
	return strings.ToUpper(s[:1]) + s[1:]
}

// demoUsage returns plausible data for screenshots and testing (--demo).
func demoUsage() *Usage {
	now := time.Now()
	return &Usage{
		Session: &Quota{Name: "Session (5h)", Percent: 48, ResetsAt: now.Add(2*time.Hour + 13*time.Minute), Period: period5h},
		Weekly:  &Quota{Name: "Weekly (7d)", Percent: 64, ResetsAt: now.Add(3*24*time.Hour + 4*time.Hour + 20*time.Minute), Period: period7d},
		Others: []*Quota{
			{Name: "Weekly Sonnet", Percent: 2, ResetsAt: now.Add(3*24*time.Hour + 4*time.Hour + 20*time.Minute), Period: period7d},
		},
	}
}
