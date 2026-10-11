//! Weather via Open-Meteo (no API key; CC BY 4.0, attribution shown on the card).
//! <https://open-meteo.com/en/docs>
//!
//! The provider boundary is `parse_open_meteo`: another provider only needs to
//! produce a `WeatherData`.

use chrono::{NaiveDate, NaiveDateTime};
use serde::Deserialize;

use super::Context;
use crate::model::{CurrentWeather, DailyForecast, HourlyForecast, WeatherData};
use crate::status::SourceError;

pub const ATTRIBUTION: &str = "Weather data by Open-Meteo.com (CC BY 4.0)";

pub fn request_url(ctx: &Context) -> String {
    let loc = &ctx.cfg.location;
    let place = *ctx.place.borrow();
    format!(
        "{base}?latitude={lat:.4}&longitude={lon:.4}\
         &current=temperature_2m,apparent_temperature,relative_humidity_2m,weather_code,is_day\
         &hourly=temperature_2m,precipitation_probability,weather_code,is_day\
         &daily=weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max,sunrise,sunset\
         &timezone={tz}&forecast_days=3&forecast_hours=36",
        base = ctx.cfg.weather.base_url,
        lat = place.latitude,
        lon = place.longitude,
        tz = loc.timezone.replace('/', "%2F"),
    )
}

pub async fn fetch(ctx: &Context) -> Result<WeatherData, SourceError> {
    let body = ctx.http.get_bytes(&request_url(ctx), 2 * 1024 * 1024).await?;
    parse_open_meteo(&body)
}

#[derive(Deserialize)]
struct Resp {
    current: Current,
    hourly: Hourly,
    daily: Daily,
}

#[derive(Deserialize)]
struct Current {
    time: String,
    temperature_2m: f32,
    apparent_temperature: Option<f32>,
    relative_humidity_2m: Option<f32>,
    weather_code: i32,
    is_day: Option<i32>,
}

#[derive(Deserialize)]
struct Hourly {
    time: Vec<String>,
    temperature_2m: Vec<Option<f32>>,
    precipitation_probability: Vec<Option<f32>>,
    weather_code: Vec<Option<i32>>,
    #[serde(default)]
    is_day: Vec<Option<i32>>,
}

#[derive(Deserialize)]
struct Daily {
    time: Vec<String>,
    weather_code: Vec<Option<i32>>,
    temperature_2m_max: Vec<Option<f32>>,
    temperature_2m_min: Vec<Option<f32>>,
    precipitation_probability_max: Vec<Option<f32>>,
    #[serde(default)]
    sunrise: Vec<Option<String>>,
    #[serde(default)]
    sunset: Vec<Option<String>>,
}

fn dt(s: &str) -> Result<NaiveDateTime, SourceError> {
    NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M").map_err(|_| SourceError::parse(format!("bad time {s:?}")))
}

fn pct(v: Option<f32>) -> Option<u8> {
    v.map(|p| p.round().clamp(0.0, 100.0) as u8)
}

pub fn parse_open_meteo(body: &[u8]) -> Result<WeatherData, SourceError> {
    let r: Resp = serde_json::from_slice(body).map_err(|e| SourceError::parse(format!("Open-Meteo JSON: {e}")))?;

    let current = CurrentWeather {
        time: dt(&r.current.time)?,
        temperature: r.current.temperature_2m,
        apparent: r.current.apparent_temperature,
        humidity: r.current.relative_humidity_2m,
        code: r.current.weather_code,
        is_day: r.current.is_day.unwrap_or(1) != 0,
    };

    let h = &r.hourly;
    let mut hourly = Vec::with_capacity(h.time.len());
    for (i, t) in h.time.iter().enumerate() {
        let (Some(Some(temp)), Some(Some(code))) = (h.temperature_2m.get(i), h.weather_code.get(i)) else {
            continue;
        };
        hourly.push(HourlyForecast {
            time: dt(t)?,
            temperature: *temp,
            precip_probability: pct(h.precipitation_probability.get(i).copied().flatten()),
            code: *code,
            is_day: h.is_day.get(i).copied().flatten().unwrap_or(1) != 0,
        });
    }

    let d = &r.daily;
    let mut daily = Vec::with_capacity(d.time.len());
    for (i, t) in d.time.iter().enumerate() {
        let date =
            NaiveDate::parse_from_str(t, "%Y-%m-%d").map_err(|_| SourceError::parse(format!("bad date {t:?}")))?;
        let (Some(Some(code)), Some(Some(tmax)), Some(Some(tmin))) = (
            d.weather_code.get(i),
            d.temperature_2m_max.get(i),
            d.temperature_2m_min.get(i),
        ) else {
            continue;
        };
        let opt_dt = |v: Option<&Option<String>>| v.and_then(|s| s.as_deref()).and_then(|s| dt(s).ok());
        daily.push(DailyForecast {
            date,
            code: *code,
            t_max: *tmax,
            t_min: *tmin,
            precip_probability: pct(d.precipitation_probability_max.get(i).copied().flatten()),
            sunrise: opt_dt(d.sunrise.get(i)),
            sunset: opt_dt(d.sunset.get(i)),
        });
    }

    if daily.is_empty() {
        return Err(SourceError::parse("Open-Meteo: daily forecast is empty"));
    }
    Ok(WeatherData { current, hourly, daily })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_live_shaped_response() {
        let body = br#"{"latitude":35.7,"longitude":139.75,"timezone":"Asia/Tokyo",
            "current":{"time":"2026-10-11T05:30","interval":900,"temperature_2m":16.1,"weather_code":0,"is_day":0,"relative_humidity_2m":88,"apparent_temperature":16.6},
            "hourly":{"time":["2026-10-11T05:00","2026-10-11T06:00"],"temperature_2m":[16.3,null],"precipitation_probability":[0,10],"weather_code":[0,1],"is_day":[0,1]},
            "daily":{"time":["2026-10-11"],"weather_code":[1],"temperature_2m_max":[26.0],"temperature_2m_min":[16.0],"precipitation_probability_max":[4],
                     "sunrise":["2026-10-11T05:41"],"sunset":["2026-10-11T17:12"]}}"#;
        let w = parse_open_meteo(body).unwrap();
        assert_eq!(w.current.code, 0);
        assert!(!w.current.is_day);
        assert_eq!(w.hourly.len(), 1, "rows with null values are skipped");
        assert_eq!(w.daily[0].precip_probability, Some(4));
        assert!(w.daily[0].sunrise.is_some());
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_open_meteo(b"{\"error\":true,\"reason\":\"x\"}").is_err());
        assert!(parse_open_meteo(b"<html>").is_err());
    }
}
