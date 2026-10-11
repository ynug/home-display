//! Normalized domain data produced by the services (live or fixture).
//! Everything here is `Send` so it can cross from the network runtime to the UI.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use chrono_tz::Tz;

// ───────────────────────────── Calendar ─────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct CalendarEvent {
    pub title: String,
    pub location: Option<String>,
    pub start: DateTime<Tz>,
    /// Exclusive end. For all-day events: midnight after the last day.
    pub end: DateTime<Tz>,
    pub all_day: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CalendarData {
    /// Expanded occurrences in the display window, sorted by start.
    pub events: Vec<CalendarEvent>,
}

// ───────────────────────────── Weather ─────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WeatherKind {
    Clear,
    PartlyCloudy,
    Cloudy,
    Fog,
    Drizzle,
    Rain,
    HeavyRain,
    Snow,
    Thunder,
    Unknown,
}

impl WeatherKind {
    /// WMO weather interpretation code (as used by Open-Meteo).
    pub fn from_wmo(code: i32) -> WeatherKind {
        match code {
            0 => WeatherKind::Clear,
            1 | 2 => WeatherKind::PartlyCloudy,
            3 => WeatherKind::Cloudy,
            45 | 48 => WeatherKind::Fog,
            51..=57 => WeatherKind::Drizzle,
            61 | 63 | 66 | 80 | 81 => WeatherKind::Rain,
            65 | 67 | 82 => WeatherKind::HeavyRain,
            71..=77 | 85 | 86 => WeatherKind::Snow,
            95..=99 => WeatherKind::Thunder,
            _ => WeatherKind::Unknown,
        }
    }

    pub fn label_ja(code: i32) -> &'static str {
        match code {
            0 => "快晴",
            1 => "晴れ",
            2 => "晴れ時々曇り",
            3 => "曇り",
            45 | 48 => "霧",
            51 | 53 | 55 => "霧雨",
            56 | 57 => "着氷性の霧雨",
            61 => "小雨",
            63 => "雨",
            65 => "強い雨",
            66 | 67 => "着氷性の雨",
            71 => "小雪",
            73 => "雪",
            75 => "大雪",
            77 => "霧雪",
            80 => "にわか雨",
            81 => "強いにわか雨",
            82 => "激しいにわか雨",
            85 | 86 => "にわか雪",
            95 => "雷雨",
            96 | 99 => "雹を伴う雷雨",
            _ => "不明",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CurrentWeather {
    /// Model time the values refer to (not the fetch time).
    pub time: NaiveDateTime,
    pub temperature: f32,
    pub apparent: Option<f32>,
    pub humidity: Option<f32>,
    pub code: i32,
    pub is_day: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HourlyForecast {
    pub time: NaiveDateTime,
    pub temperature: f32,
    pub precip_probability: Option<u8>,
    pub code: i32,
    pub is_day: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DailyForecast {
    pub date: NaiveDate,
    pub code: i32,
    pub t_max: f32,
    pub t_min: f32,
    pub precip_probability: Option<u8>,
    pub sunrise: Option<NaiveDateTime>,
    pub sunset: Option<NaiveDateTime>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WeatherData {
    pub current: CurrentWeather,
    pub hourly: Vec<HourlyForecast>,
    pub daily: Vec<DailyForecast>,
}

// ───────────────────────────── Earthquake ─────────────────────────────

/// JMA seismic intensity scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Shindo {
    S1,
    S2,
    S3,
    S4,
    S5Lower,
    S5Upper,
    S6Lower,
    S6Upper,
    S7,
}

impl Shindo {
    /// P2PQuake `maxScale` value → intensity. -1 and unknown values → None.
    pub fn from_p2p(v: i32) -> Option<Shindo> {
        Some(match v {
            10 => Shindo::S1,
            20 => Shindo::S2,
            30 => Shindo::S3,
            40 => Shindo::S4,
            45 | 46 => Shindo::S5Lower,
            50 => Shindo::S5Upper,
            55 => Shindo::S6Lower,
            60 => Shindo::S6Upper,
            70 => Shindo::S7,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Shindo::S1 => "1",
            Shindo::S2 => "2",
            Shindo::S3 => "3",
            Shindo::S4 => "4",
            Shindo::S5Lower => "5弱",
            Shindo::S5Upper => "5強",
            Shindo::S6Lower => "6弱",
            Shindo::S6Upper => "6強",
            Shindo::S7 => "7",
        }
    }

    /// 1..=9, used for colour coding.
    pub fn level(self) -> i32 {
        self as i32 + 1
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tsunami {
    None,
    Unknown,
    Checking,
    NonEffective,
    Watch,
    Warning,
}

impl Tsunami {
    pub fn from_p2p(s: &str) -> Tsunami {
        match s {
            "None" => Tsunami::None,
            "Checking" => Tsunami::Checking,
            "NonEffective" => Tsunami::NonEffective,
            "Watch" => Tsunami::Watch,
            "Warning" => Tsunami::Warning,
            _ => Tsunami::Unknown,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Tsunami::None => "津波の心配なし",
            Tsunami::Unknown => "",
            Tsunami::Checking => "津波の有無を調査中",
            Tsunami::NonEffective => "若干の海面変動",
            Tsunami::Watch => "津波注意報",
            Tsunami::Warning => "津波予報・警報",
        }
    }

    pub fn is_alert(self) -> bool {
        matches!(self, Tsunami::Watch | Tsunami::Warning | Tsunami::Checking)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Earthquake {
    pub id: String,
    /// Time as announced by JMA ("HH時MM分ころ"; JMA XML ArrivalTime).
    pub occurred_at: DateTime<Tz>,
    pub issued_at: DateTime<Tz>,
    pub hypocenter: Option<String>,
    pub magnitude: Option<f32>,
    /// km; 0 = "ごく浅い".
    pub depth_km: Option<i32>,
    pub max_shindo: Option<Shindo>,
    pub tsunami: Tsunami,
    pub foreign: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct EarthquakeData {
    /// Newest first, one entry per event.
    pub quakes: Vec<Earthquake>,
    /// Attribution line, e.g. "出典: 気象庁 防災情報XML".
    pub source: String,
}

// ───────────────────────────── Holidays ─────────────────────────────

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Holidays {
    pub days: BTreeMap<NaiveDate, String>,
}

impl Holidays {
    pub fn name(&self, d: NaiveDate) -> Option<&str> {
        self.days.get(&d).map(String::as_str)
    }
    pub fn last_date(&self) -> Option<NaiveDate> {
        self.days.keys().next_back().copied()
    }
}

// ───────────────────────────── Radar ─────────────────────────────

/// One composited radar frame, kept PNG-encoded; decoding to RGBA happens
/// only while the radar screen needs it (see `ui_bridge::RadarImages`).
#[derive(Clone, PartialEq)]
pub struct RadarFrame {
    /// Time the precipitation refers to (observation or forecast target).
    pub time: DateTime<Utc>,
    pub png: Arc<[u8]>,
    /// JMA nowcast forecast frame (not an observation).
    pub forecast: bool,
}

impl std::fmt::Debug for RadarFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RadarFrame")
            .field("time", &self.time)
            .field("forecast", &self.forecast)
            .field("png_bytes", &self.png.len())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RadarData {
    /// Oldest first: observations, then forecasts.
    pub frames: Vec<RadarFrame>,
    /// Index of the latest observation ("now").
    pub now_index: usize,
    /// Short provider name shown on screen ("気象庁", "RainViewer").
    pub source: String,
    /// The primary source failed and this is the fallback provider.
    pub fallback: bool,
    pub width: u32,
    pub height: u32,
    pub attribution: String,
    /// Synthetic fixture image: never present as a real observation.
    pub sample: bool,
}

impl RadarData {
    pub fn encoded_bytes(&self) -> usize {
        self.frames.iter().map(|f| f.png.len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shindo_mapping() {
        assert_eq!(Shindo::from_p2p(45), Some(Shindo::S5Lower));
        assert_eq!(Shindo::from_p2p(-1), None);
        assert_eq!(Shindo::S7.level(), 9);
        assert_eq!(Shindo::S1.level(), 1);
        assert_eq!(Shindo::S6Upper.label(), "6強");
    }

    #[test]
    fn wmo_mapping() {
        assert_eq!(WeatherKind::from_wmo(0), WeatherKind::Clear);
        assert_eq!(WeatherKind::from_wmo(65), WeatherKind::HeavyRain);
        assert_eq!(WeatherKind::from_wmo(95), WeatherKind::Thunder);
        assert_eq!(WeatherKind::from_wmo(1234), WeatherKind::Unknown);
        assert_eq!(WeatherKind::label_ja(3), "曇り");
    }

    #[test]
    fn tsunami_alerts() {
        assert!(Tsunami::from_p2p("Warning").is_alert());
        assert!(!Tsunami::from_p2p("None").is_alert());
        assert_eq!(Tsunami::from_p2p("garbage"), Tsunami::Unknown);
    }
}
