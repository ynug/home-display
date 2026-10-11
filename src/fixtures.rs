//! Fixture scenarios for development and tests.
//!
//! Fixture files under `fixtures/` are real-format payloads (ICS, Open-Meteo
//! JSON, P2PQuake JSON) with date placeholders, and are run through the same
//! parsers as live data. All of them are sample data; the UI labels them as such.
//!
//! Placeholders: `{{D+n}}` → `YYYYMMDD`, `{{DATE+n}}` → `YYYY-MM-DD`
//! (n may be negative), `{{NOW15}}` → now floored to 15 min (`YYYY-MM-DDTHH:MM`),
//! `{{AGO:m}}` → JST `YYYY/MM/DD HH:MM:SS`, m minutes ago.

use std::sync::Arc;

use chrono::{DateTime, Days, Duration, Timelike, Utc};
use chrono_tz::Tz;

use crate::model::{CalendarData, EarthquakeData, Holidays, WeatherData};
use crate::repository::{Payload, Repository};
use crate::services::radar::FixtureRadar;
use crate::services::{calendar, earthquake, holiday, weather};
use crate::status::{ErrorKind, SourceError, SourceId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Scenario {
    /// Busy day, fair weather, a small recent quake, light rain on radar.
    #[default]
    Default,
    /// Nothing scheduled today, overcast, no quakes, empty radar.
    Empty,
    /// Today is treated as a holiday; all-day trip.
    Holiday,
    /// Rain, a strong quake with a tsunami advisory, heavy radar echoes.
    Rain,
    /// Many events with very long titles (truncation).
    Long,
    /// Every network source fails before any data arrives.
    Offline,
    /// Data present but hours old and the latest refresh failed.
    Stale,
}

impl Scenario {
    pub const ALL: [Scenario; 7] = [
        Scenario::Default,
        Scenario::Empty,
        Scenario::Holiday,
        Scenario::Rain,
        Scenario::Long,
        Scenario::Offline,
        Scenario::Stale,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Scenario::Default => "default",
            Scenario::Empty => "empty",
            Scenario::Holiday => "holiday",
            Scenario::Rain => "rain",
            Scenario::Long => "long",
            Scenario::Offline => "offline",
            Scenario::Stale => "stale",
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Scenario::Default => "予定が複数・晴れ・小さな地震・弱い雨雲",
            Scenario::Empty => "予定なし・曇り・地震情報なし・レーダーにデータなし",
            Scenario::Holiday => "祝日扱い・終日予定・晴れ・数日前の地震",
            Scenario::Rain => "雨・強い地震(震度5強/津波注意報)・強い雨雲",
            Scenario::Long => "長い予定名と多数の予定（省略表示）",
            Scenario::Offline => "すべての取得に失敗（ネットワークエラー）",
            Scenario::Stale => "データが古い（3時間前）＋更新失敗",
        }
    }

    pub fn parse(s: &str) -> Option<Scenario> {
        Scenario::ALL
            .into_iter()
            .find(|sc| sc.name().eq_ignore_ascii_case(s.trim()))
    }
}

const CAL_BUSY: &str = include_str!("../fixtures/calendar/busy.ics");
const CAL_EMPTY: &str = include_str!("../fixtures/calendar/empty.ics");
const CAL_HOLIDAY: &str = include_str!("../fixtures/calendar/holiday.ics");
const CAL_LONG: &str = include_str!("../fixtures/calendar/long.ics");
const WX_SUNNY: &str = include_str!("../fixtures/weather/sunny.json");
const WX_CLOUDY: &str = include_str!("../fixtures/weather/cloudy.json");
const WX_RAIN: &str = include_str!("../fixtures/weather/rain.json");
const EQ_RECENT: &str = include_str!("../fixtures/earthquake/recent.json");
const EQ_STRONG: &str = include_str!("../fixtures/earthquake/strong.json");
const EQ_OLD: &str = include_str!("../fixtures/earthquake/old.json");
const EQ_NONE: &str = include_str!("../fixtures/earthquake/none.json");

/// Expand date placeholders relative to `now`.
pub fn render(template: &str, now: DateTime<Tz>) -> String {
    let jst = now.with_timezone(&chrono_tz::Asia::Tokyo);
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start..].find("}}") else {
            out.push_str(&rest[start..]);
            return out;
        };
        let token = &rest[start + 2..start + end];
        out.push_str(&expand(token, now, jst).unwrap_or_else(|| format!("{{{{{token}}}}}")));
        rest = &rest[start + end + 2..];
    }
    out.push_str(rest);
    out
}

fn shift_days(now: DateTime<Tz>, n: i64) -> chrono::NaiveDate {
    let d = now.date_naive();
    if n >= 0 {
        d + Days::new(n as u64)
    } else {
        d - Days::new((-n) as u64)
    }
}

fn expand(token: &str, now: DateTime<Tz>, jst: DateTime<Tz>) -> Option<String> {
    if let Some(n) = token.strip_prefix("DATE") {
        return Some(shift_days(now, n.parse().ok()?).format("%Y-%m-%d").to_string());
    }
    if let Some(n) = token.strip_prefix("D") {
        return Some(shift_days(now, n.parse().ok()?).format("%Y%m%d").to_string());
    }
    if token == "NOW15" {
        let floored = now.with_minute(now.minute() / 15 * 15)?;
        return Some(floored.format("%Y-%m-%dT%H:%M").to_string());
    }
    if let Some(m) = token.strip_prefix("AGO:") {
        let t = jst - Duration::minutes(m.parse().ok()?);
        return Some(t.format("%Y/%m/%d %H:%M:%S").to_string());
    }
    None
}

/// Parsed fixture payloads for one scenario (or the error to simulate).
pub struct FixtureSet {
    pub scenario: Scenario,
    pub calendar: Result<CalendarData, SourceError>,
    pub weather: Result<WeatherData, SourceError>,
    pub earthquake: Result<EarthquakeData, SourceError>,
    pub holidays: Holidays,
}

fn offline() -> SourceError {
    SourceError::new(ErrorKind::Network, "fixture: simulated network error")
}

impl FixtureSet {
    pub fn load(scenario: Scenario, now: DateTime<Tz>) -> FixtureSet {
        let tz = now.timezone();
        // Stale data was fetched three hours ago: render it as of then.
        let now = if scenario == Scenario::Stale {
            now - Duration::hours(3)
        } else {
            now
        };
        let (cal, wx, eq) = match scenario {
            Scenario::Default | Scenario::Stale | Scenario::Offline => (CAL_BUSY, WX_SUNNY, EQ_RECENT),
            Scenario::Empty => (CAL_EMPTY, WX_CLOUDY, EQ_NONE),
            Scenario::Holiday => (CAL_HOLIDAY, WX_SUNNY, EQ_OLD),
            Scenario::Rain => (CAL_BUSY, WX_RAIN, EQ_STRONG),
            Scenario::Long => (CAL_LONG, WX_CLOUDY, EQ_RECENT),
        };
        let (ws, we) = calendar::window(now, 3);
        let mut set = FixtureSet {
            scenario,
            calendar: calendar::parse_ics(&render(cal, now), tz, ws, we),
            weather: weather::parse_open_meteo(render(wx, now).as_bytes()),
            earthquake: earthquake::parse_p2pquake(render(eq, now).as_bytes(), tz),
            holidays: holiday::bundled(),
        };
        if scenario == Scenario::Holiday {
            set.holidays
                .days
                .insert(now.date_naive(), "祝日（サンプル表示）".into());
        }
        if scenario == Scenario::Offline {
            set.calendar = Err(offline());
            set.weather = Err(offline());
            set.earthquake = Err(offline());
        }
        set
    }

    pub fn radar_spec(&self) -> FixtureRadar {
        match self.scenario {
            Scenario::Default | Scenario::Long => FixtureRadar::Frames(0.55),
            Scenario::Holiday => FixtureRadar::Frames(0.3),
            Scenario::Rain => FixtureRadar::Frames(1.0),
            Scenario::Empty => FixtureRadar::Empty,
            Scenario::Offline => FixtureRadar::Error,
            Scenario::Stale => FixtureRadar::Stale(0.6),
        }
    }

    fn apply(&self, repo: &Repository, id: SourceId, result: Result<Payload, SourceError>, now: DateTime<Utc>) {
        match result {
            Ok(p) if self.scenario == Scenario::Stale => {
                repo.store_cached(p, now - Duration::hours(3));
                repo.fail(id, offline(), now, None);
            }
            Ok(p) => repo.store(p, now, None),
            Err(e) => repo.fail(id, e, now, None),
        }
    }

    pub fn apply_calendar(&self, repo: &Repository, now: DateTime<Utc>) {
        let r = self.calendar.clone().map(|d| Payload::Calendar(Arc::new(d)));
        self.apply(repo, SourceId::Calendar, r, now);
    }

    pub fn apply_weather(&self, repo: &Repository, now: DateTime<Utc>) {
        let r = self.weather.clone().map(|d| Payload::Weather(Arc::new(d)));
        self.apply(repo, SourceId::Weather, r, now);
    }

    pub fn apply_earthquake(&self, repo: &Repository, now: DateTime<Utc>) {
        let r = self.earthquake.clone().map(|d| Payload::Earthquake(Arc::new(d)));
        self.apply(repo, SourceId::Earthquake, r, now);
    }

    pub fn apply_holidays(&self, repo: &Repository, now: DateTime<Utc>) {
        // Holidays come from the bundled official snapshot, so they stay
        // available even in the offline scenario.
        repo.store(Payload::Holidays(Arc::new(self.holidays.clone())), now, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Tz> {
        chrono_tz::Asia::Tokyo
            .with_ymd_and_hms(2026, 10, 11, 10, 40, 0)
            .unwrap()
    }

    #[test]
    fn render_placeholders() {
        let s = render("{{D+0}} {{D-1}} {{DATE+2}} {{NOW15}} {{AGO:30}} {{BOGUS}}", now());
        assert_eq!(
            s,
            "20261011 20261010 2026-10-13 2026-10-11T10:30 2026/10/11 10:10:00 {{BOGUS}}"
        );
    }

    #[test]
    fn every_scenario_parses() {
        for sc in Scenario::ALL {
            let set = FixtureSet::load(sc, now());
            if sc == Scenario::Offline {
                assert!(set.calendar.is_err() && set.weather.is_err() && set.earthquake.is_err());
                continue;
            }
            set.calendar.as_ref().unwrap_or_else(|e| panic!("{sc:?} calendar: {e}"));
            set.weather.as_ref().unwrap_or_else(|e| panic!("{sc:?} weather: {e}"));
            set.earthquake.as_ref().unwrap_or_else(|e| panic!("{sc:?} quake: {e}"));
        }
    }

    #[test]
    fn busy_calendar_expands_recurrence_and_drops_cancelled() {
        let set = FixtureSet::load(Scenario::Default, now());
        let events = set.calendar.unwrap().events;
        let today: Vec<_> = events
            .iter()
            .filter(|e| e.start.date_naive() == now().date_naive())
            .collect();
        assert!(today.iter().any(|e| e.title == "子どもの送り"), "daily RRULE expanded");
        assert!(!events.iter().any(|e| e.title.contains("キャンセル")));
        assert!(today.iter().any(|e| e.all_day));
    }

    #[test]
    fn scenario_names_round_trip() {
        for sc in Scenario::ALL {
            assert_eq!(Scenario::parse(sc.name()), Some(sc));
        }
        assert_eq!(Scenario::parse("nope"), None);
    }
}
