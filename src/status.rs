//! Per-source fetch status, error classification and freshness.

use chrono::{DateTime, Duration, Utc};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SourceId {
    Calendar,
    Weather,
    Earthquake,
    Radar,
    Holiday,
}

impl SourceId {
    pub const ALL: [SourceId; 5] = [
        SourceId::Calendar,
        SourceId::Weather,
        SourceId::Earthquake,
        SourceId::Radar,
        SourceId::Holiday,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SourceId::Calendar => "カレンダー",
            SourceId::Weather => "天気",
            SourceId::Earthquake => "地震",
            SourceId::Radar => "レーダー",
            SourceId::Holiday => "祝日",
        }
    }
}

/// Where a source's data comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    Live(String),
    Fixture,
    /// Needs configuration (e.g. no iCal URL); nothing is fetched.
    Unconfigured(String),
    Disabled,
}

impl Origin {
    pub fn describe(&self) -> String {
        match self {
            Origin::Live(name) => name.clone(),
            Origin::Fixture => "サンプル (fixture)".into(),
            Origin::Unconfigured(_) => "未設定".into(),
            Origin::Disabled => "無効".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Network,
    Timeout,
    Http(u16),
    RateLimited,
    Parse,
    Config,
    Io,
}

/// A fetch failure. `message` must never contain secrets (URLs are stripped
/// before construction).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceError {
    pub kind: ErrorKind,
    pub message: String,
}

impl SourceError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        SourceError {
            kind,
            message: message.into(),
        }
    }

    pub fn parse(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Parse, message)
    }

    /// Short Japanese label for the UI.
    pub fn summary(&self) -> String {
        match self.kind {
            ErrorKind::Network => "ネットワークに接続できません".into(),
            ErrorKind::Timeout => "応答がタイムアウトしました".into(),
            ErrorKind::Http(code) => format!("サーバーエラー (HTTP {code})"),
            ErrorKind::RateLimited => "アクセス制限中 (HTTP 429)".into(),
            ErrorKind::Parse => "データ形式を解釈できません".into(),
            ErrorKind::Config => "設定に誤りがあります".into(),
            ErrorKind::Io => "ファイルを読み書きできません".into(),
        }
    }
}

impl std::fmt::Display for SourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.summary(), self.message)
    }
}

/// Health as presented in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    /// Fresh data, last attempt succeeded.
    Ok,
    /// Fresh data, but the last attempt failed and a retry is scheduled.
    Retrying,
    /// Data exists but is older than the source's freshness limit.
    Stale,
    /// No usable data and the last attempt failed.
    Error,
    /// First fetch still pending.
    Loading,
    Unconfigured,
    Disabled,
    /// Fixture data.
    Sample,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SourceStatus {
    pub origin: Origin,
    pub loading: bool,
    pub last_success: Option<DateTime<Utc>>,
    pub last_attempt: Option<DateTime<Utc>>,
    pub last_error: Option<SourceError>,
    pub consecutive_failures: u32,
    pub next_attempt: Option<DateTime<Utc>>,
    pub stale_after: Duration,
}

impl SourceStatus {
    pub fn new(origin: Origin, stale_after: Duration) -> Self {
        SourceStatus {
            origin,
            loading: false,
            last_success: None,
            last_attempt: None,
            last_error: None,
            consecutive_failures: 0,
            next_attempt: None,
            stale_after,
        }
    }

    pub fn is_stale(&self, now: DateTime<Utc>) -> bool {
        match self.last_success {
            Some(t) => now - t > self.stale_after,
            None => false,
        }
    }

    pub fn health(&self, now: DateTime<Utc>) -> Health {
        match &self.origin {
            Origin::Disabled => return Health::Disabled,
            Origin::Unconfigured(_) => return Health::Unconfigured,
            Origin::Fixture if self.last_error.is_none() => return Health::Sample,
            _ => {}
        }
        match self.last_success {
            None if self.last_error.is_some() => Health::Error,
            None => Health::Loading,
            Some(_) if self.is_stale(now) => Health::Stale,
            Some(_) if self.consecutive_failures > 0 => Health::Retrying,
            Some(_) => Health::Ok,
        }
    }

    pub fn record_success(&mut self, now: DateTime<Utc>) {
        self.loading = false;
        self.last_attempt = Some(now);
        self.last_success = Some(now);
        self.last_error = None;
        self.consecutive_failures = 0;
    }

    pub fn record_failure(&mut self, now: DateTime<Utc>, err: SourceError) {
        self.loading = false;
        self.last_attempt = Some(now);
        self.last_error = Some(err);
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
    }
}

/// Delay before the next attempt. Success → regular interval. Failure →
/// exponential backoff starting at 30 s, capped at the regular interval (but
/// never below 60 s and never above 30 min), so a dead network never causes
/// a tight retry loop.
pub fn next_delay(success: bool, consecutive_failures: u32, interval: std::time::Duration) -> std::time::Duration {
    use std::time::Duration as D;
    if success {
        return interval;
    }
    let cap = interval.clamp(D::from_secs(60), D::from_secs(30 * 60));
    let exp = consecutive_failures.saturating_sub(1).min(10);
    let backoff = D::from_secs(30).saturating_mul(1u32 << exp);
    backoff.min(cap).max(D::from_secs(30))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration as D;

    fn t(min: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + min * 60, 0).unwrap()
    }

    #[test]
    fn health_transitions() {
        let mut s = SourceStatus::new(Origin::Live("x".into()), Duration::minutes(30));
        assert_eq!(s.health(t(0)), Health::Loading);
        s.record_failure(t(0), SourceError::new(ErrorKind::Network, "down"));
        assert_eq!(s.health(t(0)), Health::Error);
        s.record_success(t(1));
        assert_eq!(s.health(t(2)), Health::Ok);
        s.record_failure(t(3), SourceError::new(ErrorKind::Timeout, "slow"));
        assert_eq!(s.health(t(4)), Health::Retrying);
        assert_eq!(s.health(t(40)), Health::Stale);
    }

    #[test]
    fn fixture_and_unconfigured() {
        let s = SourceStatus::new(Origin::Fixture, Duration::minutes(30));
        assert_eq!(s.health(t(0)), Health::Sample);
        let s = SourceStatus::new(Origin::Unconfigured("no url".into()), Duration::minutes(30));
        assert_eq!(s.health(t(0)), Health::Unconfigured);
    }

    #[test]
    fn backoff_is_bounded() {
        let iv = D::from_secs(600);
        assert_eq!(next_delay(true, 0, iv), iv);
        assert_eq!(next_delay(false, 1, iv), D::from_secs(30));
        assert_eq!(next_delay(false, 2, iv), D::from_secs(60));
        assert_eq!(next_delay(false, 3, iv), D::from_secs(120));
        assert_eq!(next_delay(false, 50, iv), iv);
        // A 60 s source never retries faster than 30 s and caps at 60 s.
        assert_eq!(next_delay(false, 9, D::from_secs(60)), D::from_secs(60));
        // Very long intervals are capped at 30 min while failing.
        assert_eq!(next_delay(false, 20, D::from_secs(7 * 86400)), D::from_secs(1800));
    }
}
