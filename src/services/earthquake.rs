//! Earthquake information (JMA 地震情報) via the P2P地震情報 JSON API v2.
//! <https://www.p2pquake.net/develop/json_api_v2/> — code 551, rate limit
//! 60 req/min per IP; we poll once per minute at most.
//!
//! JMA issues several reports per event (震度速報 → 震源情報 → 各地の震度).
//! They are merged per event, keeping the most recent report.

use chrono::{DateTime, NaiveDateTime, TimeZone};
use chrono_tz::Tz;
use serde::Deserialize;

use super::Context;
use crate::config::QuakeSource;
use crate::model::{Earthquake, EarthquakeData, Shindo, Tsunami};
use crate::status::SourceError;

pub const SOURCE_LABEL: &str = "出典: 気象庁 (P2P地震情報 API 経由)";
const JST: Tz = chrono_tz::Asia::Tokyo;

/// Official JMA XML by default; P2PQuake as configured source or fallback.
pub async fn fetch(
    ctx: &Context,
    jma: &tokio::sync::Mutex<super::jma_quake::JmaState>,
) -> Result<EarthquakeData, SourceError> {
    match ctx.cfg.earthquake.source {
        QuakeSource::Jma => match super::jma_quake::fetch(ctx, jma).await {
            Ok(d) => Ok(d),
            Err(e) if ctx.cfg.earthquake.fallback => {
                tracing::warn!("気象庁XMLを取得できないため P2P地震情報 で代替します: {e}");
                let mut d = fetch_p2pquake(ctx).await?;
                d.source = format!("{SOURCE_LABEL}（気象庁XMLの代替）");
                Ok(d)
            }
            Err(e) => Err(e),
        },
        QuakeSource::P2pquake => fetch_p2pquake(ctx).await,
    }
}

async fn fetch_p2pquake(ctx: &Context) -> Result<EarthquakeData, SourceError> {
    let url = format!("{}?codes=551&limit=20", ctx.cfg.earthquake.base_url);
    let body = ctx.http.get_bytes(&url, 4 * 1024 * 1024).await?;
    parse_p2pquake(&body, ctx.tz)
}

#[derive(Deserialize)]
struct Item {
    id: Option<String>,
    code: i32,
    earthquake: Option<Eq>,
    issue: Option<Issue>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Eq {
    time: String,
    hypocenter: Option<Hypo>,
    max_scale: Option<i32>,
    domestic_tsunami: Option<String>,
}

#[derive(Deserialize)]
struct Hypo {
    name: Option<String>,
    magnitude: Option<f32>,
    depth: Option<i32>,
}

#[derive(Deserialize)]
struct Issue {
    time: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
}

/// P2PQuake times are JST: "2026/10/10 19:00:00" (seconds optional).
fn parse_jst(s: &str, tz: Tz) -> Option<DateTime<Tz>> {
    let s = s.split('.').next()?;
    let naive = NaiveDateTime::parse_from_str(s, "%Y/%m/%d %H:%M:%S")
        .or_else(|_| NaiveDateTime::parse_from_str(s, "%Y/%m/%d %H:%M"))
        .ok()?;
    Some(JST.from_local_datetime(&naive).single()?.with_timezone(&tz))
}

pub fn parse_p2pquake(body: &[u8], tz: Tz) -> Result<EarthquakeData, SourceError> {
    let items: Vec<Item> =
        serde_json::from_slice(body).map_err(|e| SourceError::parse(format!("P2PQuake JSON: {e}")))?;

    let mut quakes: Vec<Earthquake> = Vec::new();
    for item in items {
        if item.code != 551 {
            continue;
        }
        let Some(eq) = item.earthquake else { continue };
        let Some(occurred_at) = parse_jst(&eq.time, tz) else {
            continue;
        };
        let issue_kind = item.issue.as_ref().and_then(|i| i.kind.clone()).unwrap_or_default();
        let issued_at = item
            .issue
            .as_ref()
            .and_then(|i| i.time.as_deref())
            .and_then(|t| parse_jst(t, tz))
            .unwrap_or(occurred_at);
        let hypo = eq.hypocenter.as_ref();
        let q = Earthquake {
            id: item.id.unwrap_or_default(),
            occurred_at,
            issued_at,
            hypocenter: hypo.and_then(|h| h.name.clone()).filter(|n| !n.trim().is_empty()),
            magnitude: hypo.and_then(|h| h.magnitude).filter(|m| *m >= 0.0),
            depth_km: hypo.and_then(|h| h.depth).filter(|d| *d >= 0),
            max_shindo: eq.max_scale.and_then(Shindo::from_p2p),
            tsunami: Tsunami::from_p2p(eq.domestic_tsunami.as_deref().unwrap_or("Unknown")),
            foreign: issue_kind == "Foreign",
        };

        // Merge reports for the same event (same origin time).
        if let Some(existing) = quakes.iter_mut().find(|e| e.occurred_at == q.occurred_at) {
            if q.issued_at > existing.issued_at {
                let keep_name = existing.hypocenter.clone();
                let keep_shindo = existing.max_shindo;
                *existing = q;
                existing.hypocenter = existing.hypocenter.clone().or(keep_name);
                existing.max_shindo = existing.max_shindo.max(keep_shindo);
            } else {
                existing.hypocenter = existing.hypocenter.clone().or(q.hypocenter);
                existing.magnitude = existing.magnitude.or(q.magnitude);
                existing.depth_km = existing.depth_km.or(q.depth_km);
                existing.max_shindo = existing.max_shindo.max(q.max_shindo);
            }
        } else {
            quakes.push(q);
        }
    }
    quakes.sort_by_key(|q| std::cmp::Reverse(q.occurred_at));
    quakes.truncate(10);
    Ok(EarthquakeData {
        quakes,
        source: SOURCE_LABEL.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_merges_reports() {
        let body = r#"[
          {"code":551,"id":"b","earthquake":{"domesticTsunami":"None","hypocenter":{"depth":10,"magnitude":2.6,"name":"熊本県天草・芦北地方"},"maxScale":10,"time":"2026/10/10 19:00:00"},
           "issue":{"time":"2026/10/10 19:02:58","type":"DetailScale"}},
          {"code":551,"id":"a","earthquake":{"domesticTsunami":"Checking","hypocenter":{"depth":-1,"magnitude":-1,"name":""},"maxScale":10,"time":"2026/10/10 19:00:00"},
           "issue":{"time":"2026/10/10 19:01:30","type":"ScalePrompt"}},
          {"code":551,"id":"c","earthquake":{"domesticTsunami":"Watch","hypocenter":{"depth":0,"magnitude":6.8,"name":"三陸沖"},"maxScale":50,"time":"2026/10/09 03:10:00"},
           "issue":{"time":"2026/10/09 03:15:00","type":"DetailScale"}},
          {"code":552,"id":"t"}
        ]"#;
        let d = parse_p2pquake(body.as_bytes(), JST).unwrap();
        assert_eq!(d.quakes.len(), 2);
        let first = &d.quakes[0];
        assert_eq!(first.hypocenter.as_deref(), Some("熊本県天草・芦北地方"));
        assert_eq!(first.magnitude, Some(2.6));
        assert_eq!(first.max_shindo, Some(Shindo::S1));
        assert_eq!(first.tsunami, Tsunami::None);
        let second = &d.quakes[1];
        assert_eq!(second.max_shindo, Some(Shindo::S5Upper));
        assert_eq!(second.depth_km, Some(0));
        assert!(second.tsunami.is_alert());
    }

    #[test]
    fn unknown_values_are_not_invented() {
        let body = r#"[{"code":551,"earthquake":{"hypocenter":{"depth":-1,"magnitude":-1,"name":""},"maxScale":-1,"time":"2026/10/10 19:00:00"}}]"#;
        let d = parse_p2pquake(body.as_bytes(), JST).unwrap();
        let q = &d.quakes[0];
        assert!(q.hypocenter.is_none() && q.magnitude.is_none() && q.depth_km.is_none() && q.max_shindo.is_none());
    }

    #[test]
    fn empty_list_is_valid() {
        assert!(parse_p2pquake(b"[]", JST).unwrap().quakes.is_empty());
        assert!(parse_p2pquake(b"{}", JST).is_err());
    }
}
