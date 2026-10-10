package main

import (
	"context"
	"os"
	"testing"
	"time"
)

const sampleResponse = `{
  "five_hour": {"utilization": 48.0, "resets_at": "2026-09-05T12:59:59.966454+00:00"},
  "seven_day": {"utilization": 64.0, "resets_at": "2026-09-11T20:59:59.966479+00:00"},
  "seven_day_opus": null,
  "seven_day_sonnet": {"utilization": 2.0, "resets_at": "2026-09-11T20:59:59.966479+00:00"},
  "nimbus_quill": {"utilization": 0.0, "resets_at": null},
  "limits": [
    {"kind": "session", "group": "session", "percent": 48, "resets_at": "2026-09-05T12:59:59.966454+00:00", "scope": null},
    {"kind": "weekly_model", "group": "weekly", "percent": 7, "resets_at": "2026-09-11T20:59:59.966479+00:00", "scope": {"model": "fable"}}
  ]
}`

func TestParseUsage(t *testing.T) {
	u, err := parseUsage([]byte(sampleResponse))
	if err != nil {
		t.Fatal(err)
	}
	if u.Session.Percent != 48 || u.Weekly.Percent != 64 {
		t.Fatalf("percent: %+v %+v", u.Session, u.Weekly)
	}
	want := time.Date(2026, 9, 11, 20, 59, 59, 966479000, time.UTC)
	if !u.Weekly.ResetsAt.Equal(want) {
		t.Fatalf("reset: %v", u.Weekly.ResetsAt)
	}
	if len(u.Others) != 2 || u.Others[0].Name != "Weekly Sonnet" || u.Others[1].Name != "Weekly Fable" {
		t.Fatalf("others: %+v", u.Others)
	}
}

func TestParseUsageSessionNotStarted(t *testing.T) {
	u, err := parseUsage([]byte(`{"five_hour": {"utilization": 0, "resets_at": null}, "seven_day": {"utilization": 10, "resets_at": "2026-09-11T20:59:59Z"}}`))
	if err != nil {
		t.Fatal(err)
	}
	if !u.Session.ResetsAt.IsZero() || elapsedFraction(u.Session, time.Now()) != -1 {
		t.Fatalf("session should be not started: %+v", u.Session)
	}
}

// Calls the real API with the local Claude Code login: REAL_API=1 go test -run Real -v
func TestRealAPI(t *testing.T) {
	if os.Getenv("REAL_API") == "" {
		t.Skip("set REAL_API=1")
	}
	u, fe := fetchUsage(context.Background(), fallbackUserAgent)
	if fe != nil {
		t.Fatal(fe.Msg)
	}
	now := time.Now()
	for _, q := range append([]*Quota{u.Session, u.Weekly}, u.Others...) {
		if q != nil {
			t.Logf("%s: %.0f%% resets %s (in %s) elapsed=%.2f ticks=%d", q.Name, q.Percent, fmtResetAtLong(q.ResetsAt, now), fmtCountdown(q.ResetsAt.Sub(now)), elapsedFraction(q, now), len(tickPositions(q)))
		}
	}
}
