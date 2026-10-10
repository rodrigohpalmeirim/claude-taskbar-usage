package main

import (
	"context"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"syscall"
	"time"
)

// Claude Code CLI helpers: the User-Agent mirrors the installed version, and
// "claude update" renews an expired OAuth token (same approach as
// usage-monitor-for-claude).

func claudeCLIPath() string {
	if p, err := exec.LookPath("claude"); err == nil {
		return p
	}
	home, _ := os.UserHomeDir()
	for _, p := range []string{
		filepath.Join(home, ".local", "bin", "claude.exe"),
		filepath.Join(os.Getenv("APPDATA"), "npm", "claude.cmd"),
	} {
		if _, err := os.Stat(p); err == nil {
			return p
		}
	}
	return ""
}

const createNoWindow = 0x08000000

func runCLI(timeout time.Duration, args ...string) (string, error) {
	path := claudeCLIPath()
	if path == "" {
		return "", errors.New("claude CLI not found")
	}
	ctx, cancel := context.WithTimeout(context.Background(), timeout)
	defer cancel()
	cmd := exec.CommandContext(ctx, path, args...)
	cmd.SysProcAttr = &syscall.SysProcAttr{HideWindow: true, CreationFlags: createNoWindow}
	out, err := cmd.CombinedOutput()
	return string(out), err
}

var versionRe = regexp.MustCompile(`\d+\.\d+\.\d+`)

func detectUserAgent() string {
	out, err := runCLI(20*time.Second, "--version")
	if err != nil {
		return fallbackUserAgent
	}
	if v := versionRe.FindString(out); v != "" {
		return "claude-code/" + v
	}
	return fallbackUserAgent
}

func refreshTokenViaCLI() error {
	_, err := runCLI(90*time.Second, "update")
	return err
}
