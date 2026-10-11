//! Snapshot + time → display models. Pure functions with plain Rust types
//! (`PartialEq`), so the UI bridge can skip unchanged updates and the
//! formatting rules are unit-testable without a window.

use chrono::{DateTime, Datelike, Days, Duration, NaiveDate, Timelike, Utc};
use chrono_tz::Tz;

use crate::config::{AppConfig, Profile, ProfileSettings};
use crate::model::{CalendarEvent, WeatherKind};
use crate::repository::Snapshot;
use crate::services::{calendar::local_midnight, earthquake, weather};
use crate::status::{Health, Origin, SourceId, SourceStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Panel {
    Ready,
    Empty,
    #[default]
    Loading,
    Error,
    Unconfigured,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct HeaderView {
    pub date: String,
    pub weekday: String,
    pub year: String,
    pub holiday: String,
    pub red_day: bool,
    pub status_text: String,
    pub status_health: Option<Health>,
    pub updated: String,
    pub sample: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ClockView {
    pub time: String,
    pub seconds: String,
    pub subline: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct EventRowView {
    pub start: String,
    pub end: String,
    pub title: String,
    pub location: String,
    pub all_day: bool,
    pub ongoing: bool,
    pub relative: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CalendarView {
    pub state: Panel,
    pub message: String,
    pub detail: String,
    pub count_text: String,
    pub next_label: String,
    pub next: Option<EventRowView>,
    pub all_day: Vec<EventRowView>,
    pub rows: Vec<EventRowView>,
    pub more_text: String,
    pub finished_text: String,
    pub tomorrow_label: String,
    pub tomorrow: Vec<EventRowView>,
    pub stale: bool,
    pub stale_text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HourView {
    pub label: String,
    pub kind: WeatherKind,
    pub is_day: bool,
    pub temp: String,
    pub pop: String,
    pub pop_high: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DayView {
    pub label: String,
    pub kind: WeatherKind,
    pub high: String,
    pub low: String,
    pub pop: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WeatherView {
    pub state: Panel,
    pub message: String,
    pub detail: String,
    pub location: String,
    pub kind: WeatherKind,
    pub is_day: bool,
    pub temp: String,
    pub condition: String,
    pub feels: String,
    pub humidity: String,
    pub pop: String,
    pub high: String,
    pub low: String,
    pub sun_text: String,
    pub as_of: String,
    pub hours: Vec<HourView>,
    pub days: Vec<DayView>,
    pub stale: bool,
    pub stale_text: String,
    /// Data is past its freshness limit: de-emphasise current conditions.
    pub dim: bool,
    pub attribution: String,
}

impl Default for WeatherView {
    fn default() -> Self {
        WeatherView {
            state: Panel::Loading,
            message: String::new(),
            detail: String::new(),
            location: String::new(),
            kind: WeatherKind::Unknown,
            is_day: true,
            temp: String::new(),
            condition: String::new(),
            feels: String::new(),
            humidity: String::new(),
            pop: String::new(),
            high: String::new(),
            low: String::new(),
            sun_text: String::new(),
            as_of: String::new(),
            hours: vec![],
            days: vec![],
            stale: false,
            stale_text: String::new(),
            dim: false,
            attribution: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct QuakeRowView {
    pub time: String,
    pub place: String,
    pub magnitude: String,
    pub shindo: String,
    pub level: i32,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct QuakeView {
    pub state: Panel,
    pub message: String,
    pub detail: String,
    pub has_latest: bool,
    pub shindo: String,
    pub level: i32,
    pub place: String,
    pub occurred: String,
    pub relative: String,
    pub magnitude: String,
    pub depth: String,
    pub tsunami: String,
    pub tsunami_alert: bool,
    pub emphasis: i32,
    pub history: Vec<QuakeRowView>,
    pub updated: String,
    pub stale: bool,
    pub stale_text: String,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChipView {
    pub name: String,
    pub health: Health,
    pub time: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct FooterView {
    pub sources: Vec<ChipView>,
    pub right_text: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct RadarView {
    pub state: Panel,
    pub message: String,
    pub detail: String,
    /// "HH:MM" per frame.
    pub frame_times: Vec<String>,
    /// Per frame: forecast (true) or observation.
    pub frame_forecast: Vec<bool>,
    /// Per frame: "-30分" / "現在" / "+40分" relative to the latest observation.
    pub frame_offsets: Vec<String>,
    /// Sparse timeline labels ("" for frames without a label).
    pub tick_labels: Vec<String>,
    pub now_index: i32,
    /// e.g. "気象庁" or "RainViewer（代替）".
    pub source_note: String,
    /// 0.0..=1.0 while the first download is running, -1 otherwise.
    pub progress: f32,
    pub latest_label: String,
    pub age_text: String,
    pub sample: bool,
    pub stale: bool,
    pub attribution: String,
    pub location: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiagRowView {
    pub name: String,
    pub health: Health,
    pub origin: String,
    pub last_success: String,
    pub next: String,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SettingsView {
    pub profile: Profile,
    pub profile_detail: String,
    pub rotation_enabled: bool,
    pub rotation_paused: bool,
    pub overview_seconds: u32,
    /// 0 = automatic.
    pub radar_seconds: u32,
    /// Current automatic dwell (shown when `radar_seconds` is 0).
    pub radar_auto_seconds: u32,
    pub radar_zoom: u32,
    /// e.g. "9 · 約340km" (map width).
    pub radar_zoom_text: String,
    pub mode_text: String,
    pub config_path: String,
    pub overrides_path: String,
    pub cache_dir: String,
    pub version: String,
    pub radar_memory: String,
    pub save_message: String,
    /// Calendar (iCal) status line.
    pub ical_text: String,
    pub sources: Vec<DiagRowView>,
    pub logs: Vec<String>,
}

// ───────────────────────────── formatting helpers ─────────────────────────────

const WEEKDAYS: [&str; 7] = ["月", "火", "水", "木", "金", "土", "日"];

fn weekday_short(d: NaiveDate) -> &'static str {
    WEEKDAYS[d.weekday().num_days_from_monday() as usize]
}

pub fn hm(t: DateTime<Tz>) -> String {
    t.format("%H:%M").to_string()
}

/// "06:40" today, otherwise "10/18 06:40".
fn day_hm(t: DateTime<Utc>, tz: Tz, now: DateTime<Tz>) -> String {
    let local = t.with_timezone(&tz);
    if local.date_naive() == now.date_naive() {
        hm(local)
    } else {
        local.format("%-m/%-d %H:%M").to_string()
    }
}

fn hm_utc(t: DateTime<Utc>, tz: Tz) -> String {
    hm(t.with_timezone(&tz))
}

/// "たった今" / "12分前" / "3時間前" / "2日前".
pub fn ago(now: DateTime<Tz>, t: DateTime<Tz>) -> String {
    let d = now - t;
    if d < Duration::minutes(1) {
        "たった今".into()
    } else if d < Duration::hours(1) {
        format!("{}分前", d.num_minutes())
    } else if d < Duration::hours(24) {
        format!("{}時間前", d.num_hours())
    } else {
        format!("{}日前", d.num_days())
    }
}

/// "あと25分" / "あと2時間" / "あと1時間30分".
pub fn until(d: Duration) -> String {
    let mins = (d.num_seconds() + 59) / 60;
    if mins < 60 {
        format!("あと{}分", mins.max(1))
    } else if mins % 60 == 0 {
        format!("あと{}時間", mins / 60)
    } else {
        format!("あと{}時間{}分", mins / 60, mins % 60)
    }
}

fn temp(t: f32) -> String {
    let r = t.round();
    // avoid "-0°"
    format!("{}°", if r == 0.0 { 0.0 } else { r })
}

/// Status line for a source that has data but a problem.
fn staleness(st: Option<&SourceStatus>, now: DateTime<Tz>) -> (bool, String) {
    let Some(st) = st else { return (false, String::new()) };
    let Some(last) = st.last_success else {
        return (false, String::new());
    };
    let at = hm_utc(last, now.timezone());
    let when = if last.with_timezone(&now.timezone()).date_naive() == now.date_naive() {
        at
    } else {
        format!("{} {}", last.with_timezone(&now.timezone()).format("%-m/%-d"), at)
    };
    if st.is_stale(now.with_timezone(&Utc)) {
        (true, format!("{when} 時点の古いデータ"))
    } else if st.consecutive_failures > 0 {
        (true, format!("更新失敗 · {when} 時点"))
    } else {
        (false, String::new())
    }
}

fn error_detail(st: Option<&SourceStatus>) -> String {
    st.and_then(|s| s.last_error.as_ref())
        .map(|e| e.summary())
        .unwrap_or_default()
}

// ───────────────────────────── builders ─────────────────────────────

pub struct ViewContext<'a> {
    pub snap: &'a Snapshot,
    pub now: DateTime<Tz>,
    pub cfg: &'a AppConfig,
    pub profile: Profile,
    pub settings: &'a ProfileSettings,
    pub fake_clock: bool,
    /// Running radar download, if any (polled once per second).
    pub radar_progress: Option<crate::services::radar::ProgressSnapshot>,
}

impl ViewContext<'_> {
    fn utc(&self) -> DateTime<Utc> {
        self.now.with_timezone(&Utc)
    }
    fn status(&self, id: SourceId) -> Option<&SourceStatus> {
        self.snap.status(id)
    }
}

pub fn header(cx: &ViewContext) -> HeaderView {
    let today = cx.now.date_naive();
    let holiday = cx
        .snap
        .holidays
        .as_ref()
        .and_then(|h| h.name(today))
        .unwrap_or_default()
        .to_owned();
    let weekday = format!("{}曜日", weekday_short(today));
    let red_day = today.weekday() == chrono::Weekday::Sun || !holiday.is_empty();

    let now = cx.utc();
    let relevant: Vec<(SourceId, Health)> = SourceId::ALL
        .iter()
        .filter(|id| **id != SourceId::Holiday)
        .filter_map(|id| cx.status(*id).map(|s| (*id, s.health(now))))
        .filter(|(_, h)| !matches!(h, Health::Disabled | Health::Unconfigured))
        .collect();
    let count = |want: &[Health]| relevant.iter().filter(|(_, h)| want.contains(h)).count();
    let total = relevant.len();
    let (status_text, status_health) = if total == 0 {
        ("データソースなし".to_string(), Health::Disabled)
    } else if count(&[Health::Error]) == total {
        ("オフライン".into(), Health::Error)
    } else if count(&[Health::Error]) > 0 {
        ("一部取得できません".into(), Health::Error)
    } else if count(&[Health::Stale]) > 0 {
        ("一部データが古い".into(), Health::Stale)
    } else if count(&[Health::Retrying]) > 0 {
        ("再接続中".into(), Health::Retrying)
    } else if count(&[Health::Loading]) > 0 {
        ("取得中".into(), Health::Loading)
    } else if count(&[Health::Sample]) == total {
        ("サンプル".into(), Health::Sample)
    } else {
        ("オンライン".into(), Health::Ok)
    };
    let latest = SourceId::ALL
        .iter()
        .filter(|id| **id != SourceId::Holiday)
        .filter_map(|id| cx.status(*id))
        .filter(|s| matches!(s.origin, Origin::Live(_)))
        .filter_map(|s| s.last_success)
        .max();
    HeaderView {
        date: format!("{}月{}日", today.month(), today.day()),
        weekday,
        year: format!("{}年", today.year()),
        holiday,
        red_day,
        status_text,
        status_health: Some(status_health),
        updated: latest
            .map(|t| format!("{} 更新", hm_utc(t, cx.now.timezone())))
            .unwrap_or_default(),
        sample: cx.snap.any_sample(),
    }
}

pub fn clock(cx: &ViewContext) -> ClockView {
    ClockView {
        time: hm(cx.now),
        seconds: if cx.cfg.display.show_seconds {
            cx.now.format("%S").to_string()
        } else {
            String::new()
        },
        subline: if cx.fake_clock {
            "デモ用の時刻を表示中 (--now)".into()
        } else {
            String::new()
        },
    }
}

fn event_row(ev: &CalendarEvent, now: DateTime<Tz>, day_start: DateTime<Tz>, day_end: DateTime<Tz>) -> EventRowView {
    let ongoing = !ev.all_day && ev.start <= now && now < ev.end;
    let start = if ev.start < day_start {
        "前日〜".to_string()
    } else {
        hm(ev.start)
    };
    let end = if ev.all_day || ev.end <= ev.start {
        String::new()
    } else if ev.end > day_end {
        format!("翌{}", hm(ev.end))
    } else {
        hm(ev.end)
    };
    EventRowView {
        start,
        end,
        title: ev.title.clone(),
        location: ev.location.clone().unwrap_or_default(),
        all_day: ev.all_day,
        ongoing,
        relative: String::new(),
    }
}

pub fn calendar(cx: &ViewContext) -> CalendarView {
    let st = cx.status(SourceId::Calendar);
    let mut v = CalendarView::default();
    match st.map(|s| &s.origin) {
        Some(Origin::Disabled) | None => {
            v.state = Panel::Disabled;
            v.message = "カレンダーは無効です".into();
            return v;
        }
        Some(Origin::Unconfigured(_)) => {
            v.state = Panel::Unconfigured;
            v.message = "カレンダーが未設定です".into();
            v.detail = format!(
                "{} に iCal 形式の非公開URLを設定してください",
                crate::config::ICAL_URL_ENV
            );
            return v;
        }
        _ => {}
    }
    let Some(data) = cx.snap.calendar.as_ref() else {
        if st.is_some_and(|s| s.last_error.is_some()) {
            v.state = Panel::Error;
            v.message = "予定を取得できません".into();
            v.detail = error_detail(st);
            v.count_text = "取得失敗".into();
        } else {
            v.state = Panel::Loading;
            v.message = "予定を読み込み中…".into();
        }
        return v;
    };

    let tz = cx.now.timezone();
    let today = cx.now.date_naive();
    let day_start = local_midnight(tz, today);
    let day_end = local_midnight(tz, today + Days::new(1));
    let tomorrow_end = local_midnight(tz, today + Days::new(2));
    let overlaps = |e: &&CalendarEvent, s: DateTime<Tz>, en: DateTime<Tz>| {
        e.start < en && (e.end > s || (e.end <= e.start && e.start >= s))
    };

    let todays: Vec<&CalendarEvent> = data.events.iter().filter(|e| overlaps(e, day_start, day_end)).collect();
    let all_day: Vec<&CalendarEvent> = todays.iter().copied().filter(|e| e.all_day).collect();
    let timed: Vec<&CalendarEvent> = todays.iter().copied().filter(|e| !e.all_day).collect();
    let (finished, remaining): (Vec<&CalendarEvent>, Vec<&CalendarEvent>) = timed
        .iter()
        .copied()
        .partition(|e| e.end <= cx.now && e.end > e.start || (e.end <= e.start && e.start < cx.now));

    v.count_text = if todays.is_empty() {
        "予定なし".into()
    } else {
        format!("{}件", todays.len())
    };
    v.finished_text = if finished.is_empty() {
        String::new()
    } else {
        format!("終了 {}件", finished.len())
    };
    (v.stale, v.stale_text) = staleness(st, cx.now);

    const MAX_ALL_DAY: usize = 3;
    v.all_day = all_day
        .iter()
        .take(MAX_ALL_DAY)
        .map(|e| event_row(e, cx.now, day_start, day_end))
        .collect();
    let mut hidden = all_day.len().saturating_sub(MAX_ALL_DAY);

    let mut rest = remaining.iter();
    if let Some(first) = rest.next() {
        let mut row = event_row(first, cx.now, day_start, day_end);
        if row.ongoing {
            v.next_label = "進行中".into();
            row.relative = format!("残り{}", until(first.end - cx.now).trim_start_matches("あと"));
        } else {
            v.next_label = "次の予定".into();
            row.relative = until(first.start - cx.now);
        }
        v.next = Some(row);
    }
    let max_rows = cx.settings.max_events as usize;
    let rest: Vec<&&CalendarEvent> = rest.collect();
    v.rows = rest
        .iter()
        .take(max_rows)
        .map(|e| {
            let mut r = event_row(e, cx.now, day_start, day_end);
            if r.ongoing {
                r.relative = "進行中".into();
            }
            r
        })
        .collect();
    hidden += rest.len().saturating_sub(max_rows);
    if hidden > 0 {
        v.more_text = format!("ほか {hidden}件");
    }

    let today_empty = v.next.is_none() && v.all_day.is_empty();
    if today_empty {
        v.state = Panel::Empty;
        v.message = if finished.is_empty() {
            "今日の予定はありません".into()
        } else {
            "今日の予定はすべて終了しました".into()
        };
    } else {
        v.state = Panel::Ready;
    }

    // Tomorrow is always shown; fewer rows when today already fills the card.
    let max_tomorrow = if today_empty { 3 } else { 2 };
    let tomorrow: Vec<&CalendarEvent> = data
        .events
        .iter()
        .filter(|e| overlaps(e, day_end, tomorrow_end) && e.start >= day_end)
        .collect();
    let d = today + Days::new(1);
    let count = if tomorrow.is_empty() {
        "予定なし".to_string()
    } else {
        format!("{}件", tomorrow.len())
    };
    v.tomorrow_label = format!("明日 {}月{}日 ({}) · {count}", d.month(), d.day(), weekday_short(d));
    v.tomorrow = tomorrow
        .iter()
        .take(max_tomorrow)
        .map(|e| {
            let mut r = event_row(e, cx.now, day_end, tomorrow_end);
            if r.all_day {
                r.start = "終日".into();
            }
            r
        })
        .collect();
    v
}

pub fn weather(cx: &ViewContext) -> WeatherView {
    let st = cx.status(SourceId::Weather);
    let mut v = WeatherView {
        location: cx.cfg.location.name.clone(),
        ..Default::default()
    };
    if matches!(st.map(|s| &s.origin), Some(Origin::Disabled) | None) {
        v.state = Panel::Disabled;
        v.message = "天気は無効です".into();
        return v;
    }
    let sample = st.is_some_and(|s| s.origin == Origin::Fixture);
    v.attribution = if sample {
        "サンプルデータ (fixture)".into()
    } else {
        "Open-Meteo.com (CC BY 4.0)".into()
    };
    let _ = weather::ATTRIBUTION;
    let Some(w) = cx.snap.weather.as_ref() else {
        if st.is_some_and(|s| s.last_error.is_some()) {
            v.state = Panel::Error;
            v.message = "天気を取得できません".into();
            v.detail = error_detail(st);
        } else {
            v.state = Panel::Loading;
            v.message = "天気を読み込み中…".into();
        }
        return v;
    };

    let tz = cx.now.timezone();
    let today = cx.now.date_naive();
    v.state = Panel::Ready;
    v.kind = WeatherKind::from_wmo(w.current.code);
    v.is_day = w.current.is_day;
    v.temp = temp(w.current.temperature);
    v.condition = WeatherKind::label_ja(w.current.code).into();
    let mut feels = Vec::new();
    if let Some(a) = w.current.apparent {
        feels.push(format!("体感 {}", temp(a)));
    }
    if let Some(h) = w.current.humidity {
        feels.push(format!("湿度 {:.0}%", h));
        v.humidity = format!("{:.0}%", h);
    }
    v.feels = feels.join("  ·  ");

    let now_naive = cx.now.naive_local();
    let next_hour = now_naive
        .with_minute(0)
        .and_then(|t| t.with_second(0))
        .unwrap_or(now_naive)
        + Duration::hours(1);
    let upcoming: Vec<_> = w
        .hourly
        .iter()
        .filter(|h| h.time >= next_hour - Duration::hours(1))
        .collect();
    let pop_6h = upcoming.iter().take(6).filter_map(|h| h.precip_probability).max();
    let today_daily = w.daily.iter().find(|d| d.date == today);
    v.pop = pop_6h
        .or(today_daily.and_then(|d| d.precip_probability))
        .map(|p| format!("{p}%"))
        .unwrap_or_else(|| "—".into());
    if let Some(d) = today_daily {
        v.high = temp(d.t_max);
        v.low = temp(d.t_min);
        if let (Some(r), Some(s)) = (d.sunrise, d.sunset) {
            v.sun_text = format!("日の出 {}  ·  日の入 {}", r.format("%-H:%M"), s.format("%-H:%M"));
        }
    } else {
        v.high = "—".into();
        v.low = "—".into();
    }

    let step = 2;
    v.hours = w
        .hourly
        .iter()
        .filter(|h| h.time >= next_hour)
        .step_by(step)
        .take(cx.settings.hourly_cells as usize)
        .map(|h| HourView {
            label: if h.time.date() != today {
                format!("翌{}時", h.time.hour())
            } else {
                format!("{}時", h.time.hour())
            },
            kind: WeatherKind::from_wmo(h.code),
            is_day: h.is_day,
            temp: temp(h.temperature),
            pop: h.precip_probability.map(|p| format!("{p}%")).unwrap_or_default(),
            pop_high: h.precip_probability.is_some_and(|p| p >= 50),
        })
        .collect();
    v.days = w
        .daily
        .iter()
        .filter(|d| d.date > today)
        .take(2)
        .map(|d| DayView {
            label: format!(
                "{} {}/{} ({})",
                if d.date == today + Days::new(1) {
                    "明日"
                } else {
                    "明後日"
                },
                d.date.month(),
                d.date.day(),
                weekday_short(d.date)
            ),
            kind: WeatherKind::from_wmo(d.code),
            high: temp(d.t_max),
            low: temp(d.t_min),
            pop: d.precip_probability.map(|p| format!("☂ {p}%")).unwrap_or_default(),
        })
        .collect();

    let fetched = st.and_then(|s| s.last_success).map(|t| hm_utc(t, tz));
    v.as_of = match fetched {
        Some(f) if !sample => format!("{} 時点  ·  {} 取得", w.current.time.format("%H:%M"), f),
        _ => format!("{} 時点", w.current.time.format("%H:%M")),
    };
    (v.stale, v.stale_text) = staleness(st, cx.now);
    v.dim = st.is_some_and(|s| s.is_stale(cx.utc()));
    v
}

pub fn quake(cx: &ViewContext) -> QuakeView {
    let st = cx.status(SourceId::Earthquake);
    let mut v = QuakeView::default();
    if matches!(st.map(|s| &s.origin), Some(Origin::Disabled) | None) {
        v.state = Panel::Disabled;
        v.message = "地震情報は無効です".into();
        return v;
    }
    let sample = st.is_some_and(|s| s.origin == Origin::Fixture);
    v.source = if sample {
        "サンプルデータ — 実際の地震ではありません".into()
    } else {
        cx.snap
            .earthquake
            .as_ref()
            .map(|d| d.source.clone())
            .unwrap_or_else(|| earthquake::SOURCE_LABEL.into())
    };
    let Some(data) = cx.snap.earthquake.as_ref() else {
        if st.is_some_and(|s| s.last_error.is_some()) {
            v.state = Panel::Error;
            v.message = "地震情報を取得できません".into();
            v.detail = error_detail(st);
        } else {
            v.state = Panel::Loading;
            v.message = "地震情報を読み込み中…".into();
        }
        return v;
    };
    if let Some(t) = st.and_then(|s| s.last_success) {
        v.updated = format!("{} 確認", hm_utc(t, cx.now.timezone()));
    }
    (v.stale, v.stale_text) = staleness(st, cx.now);

    let Some(q) = data.quakes.first() else {
        v.state = Panel::Empty;
        v.message = "最近の地震情報はありません".into();
        return v;
    };
    v.state = Panel::Ready;
    v.has_latest = true;
    let age = cx.now - q.occurred_at;
    let recent = age <= Duration::hours(cx.cfg.earthquake.highlight_hours as i64);
    v.shindo = q
        .max_shindo
        .map(|s| s.label().to_string())
        .unwrap_or_else(|| "—".into());
    v.level = q.max_shindo.map(|s| s.level()).unwrap_or(0);
    v.place = match (&q.hypocenter, q.foreign) {
        (Some(h), true) => format!("{h}（遠地地震）"),
        (Some(h), false) => h.clone(),
        (None, _) => "震源は調査中".into(),
    };
    v.occurred = format!("{} 発生", q.occurred_at.format("%-m月%-d日 %H:%M"));
    v.relative = ago(cx.now, q.occurred_at);
    v.magnitude = q.magnitude.map(|m| format!("M{m:.1}")).unwrap_or_default();
    v.depth = match q.depth_km {
        Some(0) => "ごく浅い".into(),
        Some(d) => format!("深さ {d}km"),
        None => String::new(),
    };
    v.tsunami = q.tsunami.label().into();
    v.tsunami_alert = q.tsunami.is_alert() && recent;
    let level = v.level;
    v.emphasis = if !recent {
        0
    } else if level >= 5 || v.tsunami_alert {
        2
    } else if level >= 4 {
        1
    } else {
        0
    };
    v.history = data
        .quakes
        .iter()
        .skip(1)
        .take(3)
        .map(|q| QuakeRowView {
            time: q.occurred_at.format("%-m/%-d %H:%M").to_string(),
            place: q.hypocenter.clone().unwrap_or_else(|| "震源調査中".into()),
            magnitude: q.magnitude.map(|m| format!("M{m:.1}")).unwrap_or_default(),
            shindo: q
                .max_shindo
                .map(|s| s.label().to_string())
                .unwrap_or_else(|| "—".into()),
            level: q.max_shindo.map(|s| s.level()).unwrap_or(0),
        })
        .collect();
    v
}

pub fn footer(cx: &ViewContext) -> FooterView {
    let now = cx.utc();
    let tz = cx.now.timezone();
    let sources = SourceId::ALL
        .iter()
        .filter_map(|id| cx.status(*id).map(|s| (*id, s)))
        .filter(|(_, s)| s.origin != Origin::Disabled)
        .map(|(id, s)| {
            let mut health = s.health(now);
            if id == SourceId::Radar
                && let Some(p) = &cx.radar_progress
            {
                let has_data = cx.snap.radar.as_ref().is_some_and(|r| !r.frames.is_empty());
                if !has_data {
                    health = Health::Loading;
                }
                let verb = if has_data { "更新中" } else { "取得中" };
                let time = if p.planned {
                    format!("{verb} {}%", p.percent())
                } else {
                    format!("{verb}…")
                };
                return ChipView {
                    name: id.label().into(),
                    health,
                    time,
                };
            }
            let time = match health {
                Health::Unconfigured => "未設定".into(),
                Health::Sample => "サンプル".into(),
                Health::Loading => "取得中".into(),
                Health::Error => "取得失敗".into(),
                _ => s.last_success.map(|t| hm_utc(t, tz)).unwrap_or_default(),
            };
            ChipView {
                name: id.label().into(),
                health,
                time,
            }
        })
        .collect();
    let mut right = format!("Home Display v{}  ·  {}", env!("CARGO_PKG_VERSION"), cx.profile.name());
    if let Some(sc) = &cx.snap.fixture_scenario {
        right.push_str(&format!("  ·  fixture: {sc}"));
    }
    FooterView {
        sources,
        right_text: right,
    }
}

/// "42%" style progress text for the radar download.
pub fn radar_progress_text(p: &crate::services::radar::ProgressSnapshot) -> (String, String, f32) {
    if !p.planned {
        return ("雨雲の一覧を取得中…".into(), String::new(), 0.0);
    }
    let pct = p.percent();
    let detail = if p.frames_total == 0 {
        "地図を準備しています".to_string()
    } else {
        format!(
            "地図と雨雲画像をダウンロード中（雨雲 {}/{} コマ）",
            p.frames_done, p.frames_total
        )
    };
    (format!("雨雲を取得中… {pct}%"), detail, pct as f32 / 100.0)
}

pub fn radar(cx: &ViewContext) -> RadarView {
    let st = cx.status(SourceId::Radar);
    let mut v = RadarView {
        location: cx.cfg.radar_label(),
        progress: -1.0,
        ..Default::default()
    };
    if matches!(st.map(|s| &s.origin), Some(Origin::Disabled) | None) {
        v.state = Panel::Disabled;
        v.message = "雨雲レーダーは無効です".into();
        return v;
    }
    let Some(r) = cx.snap.radar.as_ref() else {
        if let Some(p) = &cx.radar_progress {
            v.state = Panel::Loading;
            (v.message, v.detail, v.progress) = radar_progress_text(p);
        } else if st.is_some_and(|s| s.last_error.is_some()) {
            v.state = Panel::Error;
            v.message = "レーダー画像を取得できません".into();
            v.detail = error_detail(st);
        } else {
            v.state = Panel::Loading;
            v.message = "レーダー画像を読み込み中…".into();
        }
        return v;
    };
    v.sample = r.sample;
    v.attribution = r.attribution.clone();
    (v.stale, _) = staleness(st, cx.now);
    if r.frames.is_empty() {
        v.state = Panel::Empty;
        v.message = "表示できるレーダー画像がありません".into();
        v.detail = "データ提供元に現在のフレームがありません".into();
        return v;
    }
    let tz = cx.now.timezone();
    v.state = Panel::Ready;
    let now_i = r.now_index.min(r.frames.len() - 1);
    let now_t = r.frames[now_i].time;
    v.now_index = now_i as i32;
    v.frame_times = r.frames.iter().map(|f| hm_utc(f.time, tz)).collect();
    v.frame_forecast = r.frames.iter().map(|f| f.forecast).collect();
    let offsets: Vec<i64> = r.frames.iter().map(|f| (f.time - now_t).num_minutes()).collect();
    v.frame_offsets = offsets
        .iter()
        .map(|m| match *m {
            0 => "現在".to_string(),
            m if m > 0 => format!("+{m}分"),
            m => format!("{m}分"),
        })
        .collect();
    let last = r.frames.len() - 1;
    v.tick_labels = offsets
        .iter()
        .enumerate()
        .map(|(i, m)| {
            if i == now_i {
                "現在".into()
            } else if i == 0 || i == last || m % 30 == 0 {
                if *m > 0 { format!("+{m}分") } else { format!("{m}分") }
            } else {
                String::new()
            }
        })
        .collect();
    let latest = now_t.with_timezone(&tz);
    v.latest_label = format!("{} 時点", hm(latest));
    v.age_text = ago(cx.now, latest);
    v.source_note = if r.fallback {
        format!("{}で代替表示中（気象庁から取得できません）", r.source)
    } else {
        String::new()
    };
    v
}

pub fn profile_detail(p: &ProfileSettings, cfg: &AppConfig) -> String {
    let (w, h) = p.radar_size();
    let step = p.radar_step_minutes.max(5);
    let anim = if p.radar_total_frames() <= 1 || p.radar_frame_ms == 0 {
        "最新の静止画".to_string()
    } else {
        let past = (p.radar_frames.max(1) - 1) * step;
        let ahead = p.radar_forecast_frames.min(12) * step;
        format!(
            "過去{past}分〜予測{ahead}分 ({step}分刻み {}コマ)",
            p.radar_total_frames()
        )
    };
    let mb_per_frame = p.radar_frame_bytes() as f64 * 0.75 / 1_048_576.0;
    format!(
        "レーダー: {anim} · {}分ごと更新 · {w}×{h}px · 展開時 約{:.0}MB · 非表示時は{} · 装飾効果 {}",
        cfg.radar_refresh(p).as_secs() / 60,
        mb_per_frame * p.radar_total_frames() as f64,
        if p.keep_radar_decoded { "保持" } else { "解放" },
        if p.visual_effects { "あり" } else { "なし" },
    )
}

pub fn diag_rows(cx: &ViewContext) -> Vec<DiagRowView> {
    let now = cx.utc();
    let tz = cx.now.timezone();
    SourceId::ALL
        .iter()
        .filter_map(|id| cx.status(*id).map(|s| (*id, s)))
        .map(|(id, s)| DiagRowView {
            name: id.label().into(),
            health: s.health(now),
            origin: s.origin.describe(),
            last_success: s
                .last_success
                .map(|t| day_hm(t, tz, cx.now))
                .unwrap_or_else(|| "—".into()),
            next: match (id, &cx.radar_progress) {
                (SourceId::Radar, Some(p)) => format!("取得中 {}%", p.percent()),
                _ => s
                    .next_attempt
                    .map(|t| day_hm(t, tz, cx.now))
                    .unwrap_or_else(|| "—".into()),
            },
            error: match (&s.origin, &s.last_error) {
                (Origin::Unconfigured(msg), _) => msg.clone(),
                (_, Some(e)) => format!("{} ({})", e.summary(), e.message),
                _ => String::new(),
            },
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{FixtureSet, Scenario};
    use crate::repository::{Payload, Repository};
    use crate::status::SourceStatus;
    use chrono::TimeZone;
    use std::sync::Arc;

    const TZ: Tz = chrono_tz::Asia::Tokyo;

    fn now() -> DateTime<Tz> {
        TZ.with_ymd_and_hms(2026, 10, 11, 10, 40, 0).unwrap()
    }

    fn snapshot(scenario: Scenario) -> Snapshot {
        let repo = Repository::new(Snapshot::default());
        let t = now().with_timezone(&Utc);
        for id in SourceId::ALL {
            repo.set_status(id, SourceStatus::new(Origin::Fixture, Duration::minutes(60)));
        }
        let set = FixtureSet::load(scenario, now());
        set.apply_calendar(&repo, t);
        set.apply_weather(&repo, t);
        set.apply_earthquake(&repo, t);
        set.apply_holidays(&repo, t);
        if scenario == Scenario::Offline {
            let e = crate::status::SourceError::new(crate::status::ErrorKind::Network, "x");
            repo.fail(SourceId::Radar, e, t, None);
        }
        repo.snapshot()
    }

    fn with<R>(snap: &Snapshot, f: impl FnOnce(&ViewContext) -> R) -> R {
        let cfg = AppConfig::default();
        let ps = ProfileSettings::defaults(Profile::Balanced);
        let cx = ViewContext {
            snap,
            now: now(),
            cfg: &cfg,
            profile: Profile::Balanced,
            settings: &ps,
            fake_clock: false,
            radar_progress: None,
        };
        f(&cx)
    }

    #[test]
    fn formatting_helpers() {
        assert_eq!(until(Duration::minutes(25)), "あと25分");
        assert_eq!(until(Duration::seconds(30)), "あと1分");
        assert_eq!(until(Duration::minutes(120)), "あと2時間");
        assert_eq!(until(Duration::minutes(95)), "あと1時間35分");
        assert_eq!(ago(now(), now() - Duration::minutes(35)), "35分前");
        assert_eq!(ago(now(), now() - Duration::hours(30)), "1日前");
        assert_eq!(temp(-0.3), "0°");
        assert_eq!(temp(16.6), "17°");
    }

    #[test]
    fn busy_day_has_ongoing_hero_and_finished_count() {
        let snap = snapshot(Scenario::Default);
        let v = with(&snap, calendar);
        assert_eq!(v.state, Panel::Ready);
        let next = v.next.unwrap();
        assert_eq!(next.title, "チーム定例ミーティング");
        assert!(next.ongoing);
        assert_eq!(v.next_label, "進行中");
        assert_eq!(next.relative, "残り20分");
        assert_eq!(v.all_day.len(), 1);
        assert_eq!(v.finished_text, "終了 2件");
        assert_eq!(v.count_text, "7件");
        assert!(v.rows.len() <= 3);
    }

    #[test]
    fn empty_day_shows_tomorrow() {
        let snap = snapshot(Scenario::Empty);
        let v = with(&snap, calendar);
        assert_eq!(v.state, Panel::Empty);
        assert_eq!(v.message, "今日の予定はありません");
        assert_eq!(v.tomorrow.len(), 2);
        assert!(v.tomorrow_label.starts_with("明日 10月12日"));
    }

    #[test]
    fn busy_day_also_shows_tomorrow() {
        let snap = snapshot(Scenario::Default);
        let v = with(&snap, calendar);
        assert_eq!(v.state, Panel::Ready);
        // Includes the daily recurring 子どもの送り; only 2 rows fit on a busy day.
        assert_eq!(v.tomorrow_label, "明日 10月12日 (月) · 3件");
        let titles: Vec<&str> = v.tomorrow.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(titles, ["燃えるゴミの日", "子どもの送り"]);
        assert_eq!(v.tomorrow[0].start, "終日");
    }

    #[test]
    fn long_day_is_truncated() {
        let snap = snapshot(Scenario::Long);
        let v = with(&snap, calendar);
        assert_eq!(v.all_day.len(), 3);
        assert!(v.rows.len() <= 3);
        assert!(v.more_text.starts_with("ほか "), "{}", v.more_text);
    }

    #[test]
    fn offline_distinguishes_error_from_empty() {
        let snap = snapshot(Scenario::Offline);
        let v = with(&snap, calendar);
        assert_eq!(v.state, Panel::Error);
        assert_eq!(v.message, "予定を取得できません");
        let w = with(&snap, weather);
        assert_eq!(w.state, Panel::Error);
        let q = with(&snap, quake);
        assert_eq!(q.state, Panel::Error);
        let h = with(&snap, header);
        assert_eq!(h.status_health, Some(Health::Error));
        assert_eq!(h.status_text, "オフライン");
    }

    #[test]
    fn stale_data_is_marked() {
        let snap = snapshot(Scenario::Stale);
        let v = with(&snap, calendar);
        assert_eq!(v.state, Panel::Ready);
        assert!(v.stale);
        assert!(v.stale_text.contains("古いデータ"), "{}", v.stale_text);
        let w = with(&snap, weather);
        assert!(w.stale && w.dim);
        assert_eq!(w.as_of, "07:30 時点", "stale fixture is rendered 3 h in the past");
        let q = with(&snap, quake);
        assert_eq!(q.relative, "3時間前");
    }

    #[test]
    fn holiday_and_sunday_are_red() {
        let snap = snapshot(Scenario::Holiday);
        let h = with(&snap, header);
        assert_eq!(h.holiday, "祝日（サンプル表示）");
        assert!(h.red_day);
        assert_eq!(h.date, "10月11日");
        assert_eq!(h.weekday, "日曜日");
        assert!(h.sample);
    }

    #[test]
    fn strong_quake_is_emphasised_and_old_quake_is_not() {
        let snap = snapshot(Scenario::Rain);
        let q = with(&snap, quake);
        assert_eq!(q.shindo, "5強");
        assert_eq!(q.emphasis, 2);
        assert!(q.tsunami_alert);
        assert_eq!(q.relative, "12分前");
        let snap = snapshot(Scenario::Holiday);
        let q = with(&snap, quake);
        assert_eq!(q.emphasis, 0);
        assert!(q.relative.ends_with("日前"));
    }

    #[test]
    fn no_quakes_is_empty_not_error() {
        let snap = snapshot(Scenario::Empty);
        let q = with(&snap, quake);
        assert_eq!(q.state, Panel::Empty);
        assert!(!q.has_latest);
    }

    #[test]
    fn weather_cells_follow_profile() {
        let snap = snapshot(Scenario::Default);
        let w = with(&snap, weather);
        assert_eq!(w.state, Panel::Ready);
        assert_eq!(w.hours.len(), 8);
        assert_eq!(w.hours[0].label, "11時");
        assert_eq!(w.days.len(), 2);
        assert!(w.days[0].label.starts_with("明日"));
    }

    #[test]
    fn radar_states() {
        let mut snap = snapshot(Scenario::Default);
        let r = with(&snap, radar);
        assert_eq!(r.state, Panel::Loading);
        let repo = Repository::new(snap.clone());
        repo.store(
            Payload::Radar(Arc::new(
                crate::services::radar::fixture_data(
                    crate::services::radar::FixtureRadar::Empty,
                    &ProfileSettings::defaults(Profile::Balanced),
                )
                .unwrap(),
            )),
            now().with_timezone(&Utc),
            None,
        );
        snap = repo.snapshot();
        assert_eq!(with(&snap, radar).state, Panel::Empty);

        // Past hour + forecast hour: ticks at -60/-30/now/+30/+60 only.
        repo.store(
            Payload::Radar(Arc::new(
                crate::services::radar::fixture_data(
                    crate::services::radar::FixtureRadar::Frames(0.5),
                    &ProfileSettings::defaults(Profile::Balanced),
                )
                .unwrap(),
            )),
            now().with_timezone(&Utc),
            None,
        );
        let v = with(&repo.snapshot(), radar);
        assert_eq!(v.state, Panel::Ready);
        assert_eq!(v.now_index, 6);
        assert_eq!(v.frame_forecast.iter().filter(|f| **f).count(), 6);
        let ticks: Vec<&str> = v
            .tick_labels
            .iter()
            .map(String::as_str)
            .filter(|t| !t.is_empty())
            .collect();
        assert_eq!(ticks, ["-60分", "-30分", "現在", "+30分", "+60分"]);
        assert_eq!(v.frame_offsets[8], "+20分");
        assert!(v.source_note.is_empty());
    }

    #[test]
    fn unconfigured_calendar() {
        let repo = Repository::new(Snapshot::default());
        repo.set_status(
            SourceId::Calendar,
            SourceStatus::new(Origin::Unconfigured("x".into()), Duration::minutes(1)),
        );
        let snap = repo.snapshot();
        let v = with(&snap, calendar);
        assert_eq!(v.state, Panel::Unconfigured);
        assert!(v.detail.contains("HOME_DISPLAY_ICAL_URL"));
    }
}
