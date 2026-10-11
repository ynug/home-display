//! Screen rotation and navigation — one owner, driven by a single UI tick.
//!
//! Pure logic (no Slint, no timers) so it is fully unit-testable: the UI
//! calls `tick(now)` once per second and applies the returned screen.

use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Overview,
    Radar,
    Settings,
}

#[derive(Debug, Clone)]
pub struct RotationSettings {
    pub enabled: bool,
    pub overview: Duration,
    /// `None` = automatic: use the dwell supplied by `set_radar_auto_dwell`.
    pub radar: Option<Duration>,
    pub settings_timeout: Duration,
}

/// Automatic radar dwell before the first frames have been measured.
pub const DEFAULT_RADAR_DWELL: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub struct ScreenManager {
    current: Screen,
    since: Instant,
    last_input: Instant,
    settings: RotationSettings,
    /// Temporarily hold the current screen (Space key); rotation stays enabled.
    paused: bool,
    /// Radar enabled in the configuration.
    radar_available: bool,
    /// Radar images exist (first fetch finished). Rotation waits for this.
    radar_ready: bool,
    radar_auto_dwell: Duration,
}

impl ScreenManager {
    pub fn new(settings: RotationSettings, now: Instant) -> Self {
        ScreenManager {
            current: Screen::Overview,
            since: now,
            last_input: now,
            settings,
            paused: false,
            radar_available: true,
            radar_ready: true,
            radar_auto_dwell: DEFAULT_RADAR_DWELL,
        }
    }

    pub fn current(&self) -> Screen {
        self.current
    }

    pub fn settings(&self) -> &RotationSettings {
        &self.settings
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    pub fn set_settings(&mut self, s: RotationSettings) {
        self.settings = s;
    }

    /// Radar disabled in config → rotation skips it.
    pub fn set_radar_available(&mut self, available: bool, now: Instant) {
        self.radar_available = available;
        if !available && self.current == Screen::Radar {
            self.go(Screen::Overview, now);
        }
    }

    /// While no radar image exists yet, automatic rotation stays on
    /// Overview (manual navigation still works and shows the progress).
    pub fn set_radar_ready(&mut self, ready: bool) {
        self.radar_ready = ready;
    }

    pub fn set_radar_auto_dwell(&mut self, d: Duration) {
        self.radar_auto_dwell = d;
    }

    /// Effective radar dwell.
    pub fn radar_dwell(&self) -> Duration {
        self.settings.radar.unwrap_or(self.radar_auto_dwell)
    }

    /// Restart the dwell of the current screen (e.g. when the radar
    /// animation actually starts after its images were decoded).
    pub fn restart_dwell(&mut self, now: Instant) {
        self.since = now;
    }

    fn go(&mut self, s: Screen, now: Instant) {
        self.current = s;
        self.since = now;
    }

    /// User navigation; returns the new screen if it changed.
    pub fn show(&mut self, s: Screen, now: Instant) -> Option<Screen> {
        self.last_input = now;
        if s == Screen::Radar && !self.radar_available {
            return None;
        }
        if s == self.current {
            return None;
        }
        self.go(s, now);
        Some(s)
    }

    /// Cycle Overview ↔ Radar (tap / arrow keys).
    pub fn next(&mut self, now: Instant) -> Option<Screen> {
        let target = match self.current {
            Screen::Overview if self.radar_available => Screen::Radar,
            _ => Screen::Overview,
        };
        self.show(target, now)
    }

    pub fn toggle_pause(&mut self, now: Instant) -> bool {
        self.paused = !self.paused;
        self.last_input = now;
        // Restart the dwell time so resuming doesn't jump immediately.
        self.since = now;
        self.paused
    }

    pub fn touch(&mut self, now: Instant) {
        self.last_input = now;
    }

    /// Advance automatic rotation. Never blocked by data state: a failing
    /// source only affects its own panel.
    pub fn tick(&mut self, now: Instant) -> Option<Screen> {
        let elapsed = now.saturating_duration_since(self.since);
        match self.current {
            Screen::Settings => {
                if now.saturating_duration_since(self.last_input) >= self.settings.settings_timeout {
                    self.go(Screen::Overview, now);
                    return Some(Screen::Overview);
                }
                None
            }
            _ if !self.settings.enabled || self.paused || !self.radar_available => None,
            Screen::Overview if elapsed >= self.settings.overview && self.radar_ready => {
                self.go(Screen::Radar, now);
                Some(Screen::Radar)
            }
            Screen::Radar if elapsed >= self.radar_dwell() => {
                self.go(Screen::Overview, now);
                Some(Screen::Overview)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> RotationSettings {
        RotationSettings {
            enabled: true,
            overview: Duration::from_secs(20),
            radar: Some(Duration::from_secs(10)),
            settings_timeout: Duration::from_secs(120),
        }
    }

    fn at(t0: Instant, s: u64) -> Instant {
        t0 + Duration::from_secs(s)
    }

    #[test]
    fn rotates_with_configured_durations() {
        let t0 = Instant::now();
        let mut m = ScreenManager::new(settings(), t0);
        assert_eq!(m.tick(at(t0, 19)), None);
        assert_eq!(m.tick(at(t0, 20)), Some(Screen::Radar));
        assert_eq!(m.tick(at(t0, 29)), None);
        assert_eq!(m.tick(at(t0, 30)), Some(Screen::Overview));
        assert_eq!(m.tick(at(t0, 50)), Some(Screen::Radar));
    }

    #[test]
    fn disabled_rotation_stays_put() {
        let t0 = Instant::now();
        let mut s = settings();
        s.enabled = false;
        let mut m = ScreenManager::new(s, t0);
        assert_eq!(m.tick(at(t0, 1000)), None);
        assert_eq!(m.current(), Screen::Overview);
    }

    #[test]
    fn pause_holds_and_resume_restarts_dwell() {
        let t0 = Instant::now();
        let mut m = ScreenManager::new(settings(), t0);
        assert!(m.toggle_pause(at(t0, 5)));
        assert_eq!(m.tick(at(t0, 100)), None);
        assert!(!m.toggle_pause(at(t0, 100)));
        assert_eq!(m.tick(at(t0, 119)), None);
        assert_eq!(m.tick(at(t0, 120)), Some(Screen::Radar));
    }

    #[test]
    fn settings_screen_times_out_to_overview() {
        let t0 = Instant::now();
        let mut m = ScreenManager::new(settings(), t0);
        assert_eq!(m.show(Screen::Settings, at(t0, 1)), Some(Screen::Settings));
        assert_eq!(m.tick(at(t0, 60)), None, "no rotation while in settings");
        m.touch(at(t0, 60));
        assert_eq!(m.tick(at(t0, 179)), None);
        assert_eq!(m.tick(at(t0, 180)), Some(Screen::Overview));
    }

    #[test]
    fn radar_unavailable_is_skipped() {
        let t0 = Instant::now();
        let mut m = ScreenManager::new(settings(), t0);
        m.set_radar_available(false, t0);
        assert_eq!(m.tick(at(t0, 100)), None);
        assert_eq!(m.next(at(t0, 101)), None);
        assert_eq!(m.show(Screen::Radar, at(t0, 102)), None);
    }

    #[test]
    fn waits_for_radar_images_before_rotating() {
        let t0 = Instant::now();
        let mut m = ScreenManager::new(settings(), t0);
        m.set_radar_ready(false);
        assert_eq!(m.tick(at(t0, 60)), None, "no radar yet: stay on overview");
        // Manual navigation is still allowed (shows the progress).
        assert_eq!(m.next(at(t0, 61)), Some(Screen::Radar));
        m.show(Screen::Overview, at(t0, 62));
        m.set_radar_ready(true);
        assert_eq!(m.tick(at(t0, 82)), Some(Screen::Radar));
    }

    #[test]
    fn automatic_radar_dwell_and_restart() {
        let t0 = Instant::now();
        let mut s = settings();
        s.radar = None;
        let mut m = ScreenManager::new(s, t0);
        m.set_radar_auto_dwell(Duration::from_millis(18_500));
        assert_eq!(m.tick(at(t0, 20)), Some(Screen::Radar));
        // Images were decoded 1 s later: the dwell counts from then.
        m.restart_dwell(at(t0, 21));
        assert_eq!(m.tick(at(t0, 39)), None);
        assert_eq!(m.tick(at(t0, 40)), Some(Screen::Overview));
    }

    #[test]
    fn manual_navigation_resets_dwell() {
        let t0 = Instant::now();
        let mut m = ScreenManager::new(settings(), t0);
        assert_eq!(m.next(at(t0, 15)), Some(Screen::Radar));
        assert_eq!(m.tick(at(t0, 24)), None);
        assert_eq!(m.tick(at(t0, 25)), Some(Screen::Overview));
    }
}
