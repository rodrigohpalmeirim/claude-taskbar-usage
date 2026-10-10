//! Settings in `%APPDATA%\ClaudeTaskbarUsage\config.json`. Every field is
//! optional; missing ones fall back to the defaults.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Distance in 96-DPI pixels between the widget and the tray icons.
    /// Set by dragging the widget along the taskbar.
    pub offset: i32,
    /// Width of each bar in 96-DPI pixels.
    pub bar_width: u32,
    /// Seconds between API polls while the computer is in use.
    pub poll_seconds: u64,
    /// Seconds between API polls after 5 minutes without input.
    pub idle_poll_seconds: u64,
    // Which elements to show. All can also be toggled from the widget's
    // right-click menu (Show submenu).
    pub show_session: bool,
    pub show_weekly: bool,
    pub show_labels: bool,
    pub show_bar: bool,
    pub show_ticks: bool,
    pub show_marker: bool,
    pub show_percent: bool,
    pub show_countdown: bool,
    pub show_reset_time: bool,
    /// Turn the bar red when usage runs ahead of the elapsed time.
    pub warn_when_outpacing: bool,
    /// "auto" (follow Windows), "dark" (light text) or "light" (dark text).
    pub theme: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            offset: 0,
            bar_width: 110,
            poll_seconds: 180,
            idle_poll_seconds: 900,
            show_session: true,
            show_weekly: true,
            show_labels: true,
            show_bar: true,
            show_ticks: true,
            show_marker: true,
            show_percent: true,
            show_countdown: true,
            show_reset_time: true,
            warn_when_outpacing: true,
            theme: "auto".into(),
        }
    }
}

impl Config {
    pub fn path() -> Option<PathBuf> {
        let dir = std::env::var_os("APPDATA")?;
        Some(PathBuf::from(dir).join("ClaudeTaskbarUsage").join("config.json"))
    }

    pub fn load() -> Self {
        let c =
            Self::path().and_then(|p| std::fs::read(p).ok()).and_then(|raw| serde_json::from_slice::<Self>(&raw).ok()).unwrap_or_default();
        c.sanitized()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let p = Self::path().ok_or(std::io::ErrorKind::NotFound)?;
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(p, serde_json::to_vec_pretty(self).unwrap_or_default())
    }

    fn sanitized(mut self) -> Self {
        self.bar_width = self.bar_width.clamp(40, 600);
        self.poll_seconds = self.poll_seconds.max(60);
        self.idle_poll_seconds = self.idle_poll_seconds.max(self.poll_seconds);
        // Never end up with an empty widget from a hand-edited file.
        if !self.show_session && !self.show_weekly {
            self.show_session = true;
            self.show_weekly = true;
        }
        if !(self.show_bar || self.show_percent || self.show_countdown || self.show_reset_time) {
            self.show_bar = true;
            self.show_percent = true;
        }
        self
    }
}

/// What a "Show" toggle controls; decides which entries may be switched off.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// The 5-hour / weekly rows: at least one stays visible.
    Row,
    /// Bar, percentage, countdown, reset time: at least one stays visible.
    Content,
    /// Only meaningful while the bar is shown.
    BarDetail,
    Other,
}

/// Entries of the right-click "Show" submenu.
pub struct Toggle {
    pub label: &'static str,
    pub field: fn(&mut Config) -> &mut bool,
    pub group: Group,
    /// Draw a separator before this entry.
    pub separator: bool,
}

const fn toggle(label: &'static str, field: fn(&mut Config) -> &mut bool, group: Group, separator: bool) -> Toggle {
    Toggle { label, field, group, separator }
}

pub const TOGGLES: &[Toggle] = &[
    toggle("5-hour session", |c| &mut c.show_session, Group::Row, false),
    toggle("Weekly", |c| &mut c.show_weekly, Group::Row, false),
    toggle("Labels (5h / 7d)", |c| &mut c.show_labels, Group::Other, true),
    toggle("Bar", |c| &mut c.show_bar, Group::Content, false),
    toggle("Hour / day ticks", |c| &mut c.show_ticks, Group::BarDetail, false),
    toggle("Current time marker", |c| &mut c.show_marker, Group::BarDetail, false),
    toggle("Percentage", |c| &mut c.show_percent, Group::Content, false),
    toggle("Time until reset", |c| &mut c.show_countdown, Group::Content, false),
    toggle("Reset time", |c| &mut c.show_reset_time, Group::Content, false),
    toggle("Red bar when ahead of the clock", |c| &mut c.warn_when_outpacing, Group::BarDetail, true),
];

pub fn toggle_value(c: &Config, i: usize) -> bool {
    *(TOGGLES[i].field)(&mut c.clone())
}

/// Whether toggle `i` must stay as it is: the last visible row or content
/// column can't be hidden, and bar details are meaningless without the bar.
pub fn toggle_locked(c: &Config, i: usize) -> bool {
    let group = TOGGLES[i].group;
    let visible_in_group = (0..TOGGLES.len()).filter(|&j| TOGGLES[j].group == group && toggle_value(c, j)).count();
    match group {
        Group::Row | Group::Content => toggle_value(c, i) && visible_in_group == 1,
        Group::BarDetail => !c.show_bar,
        Group::Other => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idx(label: &str) -> usize {
        TOGGLES.iter().position(|t| t.label == label).unwrap()
    }

    #[test]
    fn locking_rules() {
        let mut c = Config::default();
        assert!((0..TOGGLES.len()).all(|i| !toggle_locked(&c, i)));
        c.show_weekly = false;
        assert!(toggle_locked(&c, idx("5-hour session")));
        assert!(!toggle_locked(&c, idx("Weekly")));
        c.show_percent = false;
        c.show_countdown = false;
        c.show_reset_time = false;
        assert!(toggle_locked(&c, idx("Bar")));
        c.show_percent = true;
        c.show_bar = false;
        assert!(toggle_locked(&c, idx("Hour / day ticks")));
        assert!(toggle_locked(&c, idx("Current time marker")));
    }

    #[test]
    fn missing_fields_use_defaults_and_empty_widget_is_fixed() {
        let c: Config =
            serde_json::from_str(r#"{"show_bar": false, "show_percent": false, "show_countdown": false, "show_reset_time": false}"#)
                .unwrap();
        let c = c.sanitized();
        assert!(c.show_bar && c.show_percent && c.show_labels);
        assert_eq!(c.bar_width, 110);
    }
}
