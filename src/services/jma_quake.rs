//! Earthquake information straight from 気象庁防災情報XML (official, free,
//! no registration). <https://xml.kishou.go.jp/xmlpull.html>
//!
//! * `eqvol.xml` (high-frequency Atom feed, updated every minute, ≥ last
//!   10 min) is polled; `eqvol_l.xml` (several days, hourly) fills history at
//!   start-up and once an hour so nothing is missed after an outage.
//! * Each telegram URL is downloaded at most once (JMA blocks clients that
//!   download more than 10 GB/day; this source uses ~30 MB/day).
//! * Reports of the same event share an `EventID` and are merged; a 取消
//!   (cancellation) removes the event.
//!
//! Telegrams used: VXSE51 震度速報, VXSE52 震源に関する情報,
//! VXSE53 震源・震度に関する情報 (incl. 遠地地震に関する情報).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use chrono::{DateTime, FixedOffset};
use chrono_tz::Tz;
use roxmltree::{Document, Node};

use super::Context;
use crate::model::{Earthquake, EarthquakeData, Shindo, Tsunami};
use crate::status::SourceError;

pub const SOURCE_LABEL: &str = "出典: 気象庁 防災情報XML";

/// Feed titles we read.
const TITLES: [&str; 4] = [
    "震度速報",
    "震源に関する情報",
    "震源・震度に関する情報",
    "遠地地震に関する情報",
];
/// Newest telegrams fetched per refresh (bounds the start-up burst).
const MAX_NEW_PER_RUN: usize = 40;
const LONG_FEED_EVERY: Duration = Duration::from_secs(3600);

/// One parsed telegram.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub event_id: String,
    pub report_time: DateTime<FixedOffset>,
    pub cancelled: bool,
    pub origin_time: Option<DateTime<FixedOffset>>,
    pub hypocenter: Option<String>,
    pub magnitude: Option<f32>,
    pub depth_km: Option<i32>,
    pub max_shindo: Option<Shindo>,
    pub tsunami: Option<Tsunami>,
    pub foreign: bool,
}

/// An Atom feed entry: (title, telegram URL).
/// Telegram links in the feed are only followed to JMA's own host.
const JMA_TELEGRAM_PREFIX: &str = "https://www.data.jma.go.jp/";

fn is_jma_telegram_url(url: &str) -> bool {
    url.starts_with(JMA_TELEGRAM_PREFIX) && !url.contains("..")
}

pub fn parse_feed(xml: &str) -> Result<Vec<(String, String)>, SourceError> {
    let doc = Document::parse(xml).map_err(|e| SourceError::parse(format!("気象庁フィード: {e}")))?;
    let mut out = Vec::new();
    for entry in doc.descendants().filter(|n| n.tag_name().name() == "entry") {
        let title = child_text(entry, "title").unwrap_or_default();
        let href = entry
            .children()
            .find(|n| n.tag_name().name() == "link")
            .and_then(|l| l.attribute("href"))
            .unwrap_or_default();
        if !href.is_empty() {
            out.push((title.to_owned(), href.to_owned()));
        }
    }
    Ok(out)
}

fn child<'a, 'i>(n: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| c.tag_name().name() == name)
}

fn child_text<'a>(n: Node<'a, '_>, name: &str) -> Option<&'a str> {
    child(n, name).and_then(|c| c.text()).map(str::trim)
}

/// Follow a path of local element names (namespaces ignored).
fn path<'a, 'i>(n: Node<'a, 'i>, names: &[&str]) -> Option<Node<'a, 'i>> {
    names.iter().try_fold(n, |cur, name| child(cur, name))
}

fn path_text<'a>(n: Node<'a, '_>, names: &[&str]) -> Option<&'a str> {
    path(n, names)
        .and_then(|c| c.text())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn shindo(s: &str) -> Option<Shindo> {
    Some(match s.trim() {
        "1" => Shindo::S1,
        "2" => Shindo::S2,
        "3" => Shindo::S3,
        "4" => Shindo::S4,
        "5-" => Shindo::S5Lower,
        "5+" => Shindo::S5Upper,
        "6-" => Shindo::S6Lower,
        "6+" => Shindo::S6Upper,
        "7" => Shindo::S7,
        _ => return None,
    })
}

/// Depth from `+32.4+130.5-10000/` (metres, negative down) and its description.
fn depth_km(coord: Option<&str>, description: Option<&str>) -> Option<i32> {
    if description.is_some_and(|d| d.contains("ごく浅い")) {
        return Some(0);
    }
    if description.is_some_and(|d| d.contains("不明")) {
        return None;
    }
    let c = coord?.trim_end_matches('/');
    let mut parts = Vec::new();
    let mut cur = String::new();
    for ch in c.chars() {
        if (ch == '+' || ch == '-') && !cur.is_empty() {
            parts.push(std::mem::take(&mut cur));
        }
        cur.push(ch);
    }
    if !cur.is_empty() {
        parts.push(cur);
    }
    let metres: f64 = parts.get(2)?.parse().ok()?;
    Some((metres.abs() / 1000.0).round() as i32)
}

/// Tsunami status from the fixed comment text (codes vary by telegram type).
fn tsunami_from_comment(text: &str) -> Option<Tsunami> {
    if text.contains("大津波警報") || text.contains("津波警報") {
        Some(Tsunami::Warning)
    } else if text.contains("津波注意報") {
        Some(Tsunami::Watch)
    } else if text.contains("海面変動") {
        Some(Tsunami::NonEffective)
    } else if text.contains("津波の心配はありません") || text.contains("津波の影響はありません") {
        Some(Tsunami::None)
    } else {
        None
    }
}

/// Parse one VXSE51/52/53 telegram. Returns `Ok(None)` for other kinds.
pub fn parse_report(xml: &str) -> Result<Option<Report>, SourceError> {
    let doc = Document::parse(xml).map_err(|e| SourceError::parse(format!("気象庁XML: {e}")))?;
    let root = doc.root_element();
    let (Some(head), Some(body)) = (child(root, "Head"), child(root, "Body")) else {
        return Ok(None);
    };
    let title = child_text(head, "Title").unwrap_or_default();
    let control_title = path_text(root, &["Control", "Title"]).unwrap_or_default();
    if !TITLES.contains(&control_title) {
        return Ok(None);
    }
    let rt = |s: Option<&str>| s.and_then(|s| DateTime::parse_from_rfc3339(s).ok());
    let Some(report_time) = rt(child_text(head, "ReportDateTime")) else {
        return Err(SourceError::parse("気象庁XML: ReportDateTime がありません"));
    };
    let event_id = child_text(head, "EventID").unwrap_or_default().to_owned();
    let cancelled = child_text(head, "InfoType") == Some("取消");

    let quake = child(body, "Earthquake");
    // Display time = ArrivalTime (発現時刻), the "HH時MM分ころ" JMA announces and
    // shows on its website; OriginTime (震源時) is usually ~1 min earlier.
    // 震度速報 has neither: its TargetDateTime is that same minute.
    let origin_time = quake
        .and_then(|q| rt(child_text(q, "ArrivalTime")))
        .or_else(|| quake.and_then(|q| rt(child_text(q, "OriginTime"))))
        .or_else(|| rt(child_text(head, "TargetDateTime")));
    let area = quake.and_then(|q| path(q, &["Hypocenter", "Area"]));
    let hypocenter = area
        .and_then(|a| child_text(a, "Name"))
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let coord = area.and_then(|a| child(a, "Coordinate"));
    let depth = depth_km(
        coord.and_then(|c| c.text()),
        coord.and_then(|c| c.attribute("description")),
    );
    let magnitude = quake
        .and_then(|q| child_text(q, "Magnitude"))
        .and_then(|m| m.parse::<f32>().ok())
        .filter(|m| m.is_finite());
    let max_shindo = path_text(body, &["Intensity", "Observation", "MaxInt"]).and_then(shindo);
    let tsunami = path(body, &["Comments", "ForecastComment"])
        .and_then(|c| child_text(c, "Text"))
        .and_then(tsunami_from_comment);

    Ok(Some(Report {
        event_id,
        report_time,
        cancelled,
        origin_time,
        hypocenter,
        magnitude,
        depth_km: depth,
        max_shindo,
        tsunami,
        foreign: control_title.contains("遠地") || title.contains("遠地"),
    }))
}

/// Merge telegrams per EventID (newest report wins, earlier ones fill gaps),
/// drop cancelled events, newest first.
pub fn merge(reports: &[Report], tz: Tz) -> Vec<Earthquake> {
    let mut by_event: HashMap<&str, Vec<&Report>> = HashMap::new();
    for r in reports {
        by_event.entry(r.event_id.as_str()).or_default().push(r);
    }
    let mut out = Vec::new();
    for (id, mut rs) in by_event {
        rs.sort_by_key(|r| std::cmp::Reverse(r.report_time));
        if rs[0].cancelled {
            continue;
        }
        let pick = |f: &dyn Fn(&Report) -> bool| rs.iter().find(|r| f(r)).copied();
        let Some(origin) = pick(&|r| r.origin_time.is_some()).and_then(|r| r.origin_time) else {
            continue;
        };
        out.push(Earthquake {
            id: id.to_owned(),
            occurred_at: origin.with_timezone(&tz),
            issued_at: rs[0].report_time.with_timezone(&tz),
            hypocenter: pick(&|r| r.hypocenter.is_some()).and_then(|r| r.hypocenter.clone()),
            magnitude: pick(&|r| r.magnitude.is_some()).and_then(|r| r.magnitude),
            depth_km: pick(&|r| r.depth_km.is_some()).and_then(|r| r.depth_km),
            max_shindo: rs.iter().filter_map(|r| r.max_shindo).max(),
            tsunami: pick(&|r| r.tsunami.is_some())
                .and_then(|r| r.tsunami)
                .unwrap_or(Tsunami::Unknown),
            foreign: rs.iter().any(|r| r.foreign),
        });
    }
    out.sort_by_key(|q| std::cmp::Reverse(q.occurred_at));
    out.truncate(10);
    out
}

/// Telegrams already downloaded, keyed by URL (`None` = not an earthquake report).
#[derive(Default)]
pub struct JmaState {
    reports: HashMap<String, Option<Report>>,
    long_feed_at: Option<Instant>,
}

pub async fn fetch(ctx: &Context, state: &tokio::sync::Mutex<JmaState>) -> Result<EarthquakeData, SourceError> {
    let mut st = state.lock().await;
    let use_long = st.long_feed_at.is_none_or(|t| t.elapsed() >= LONG_FEED_EVERY);
    let feed_url = if use_long {
        &ctx.cfg.earthquake.jma_long_feed_url
    } else {
        &ctx.cfg.earthquake.jma_feed_url
    };
    let feed = ctx.http.get_text(feed_url, 8 * 1024 * 1024).await?;
    let entries: Vec<(String, String)> = parse_feed(&feed)?
        .into_iter()
        .filter(|(title, url)| TITLES.contains(&title.as_str()) && is_jma_telegram_url(url))
        .collect();

    // Feeds list newest first; download only unseen telegrams.
    let new: Vec<String> = entries
        .iter()
        .map(|(_, url)| url.clone())
        .filter(|url| !st.reports.contains_key(url))
        .take(MAX_NEW_PER_RUN)
        .collect();
    for url in new {
        match ctx.http.get_text(&url, 2 * 1024 * 1024).await {
            Ok(xml) => {
                let parsed = parse_report(&xml).unwrap_or_else(|e| {
                    tracing::warn!("地震XMLを解釈できません: {e}");
                    None
                });
                st.reports.insert(url, parsed);
            }
            // Retry on the next run; keep what we have.
            Err(e) => tracing::warn!("地震XMLを取得できません: {e}"),
        }
    }
    if use_long {
        st.long_feed_at = Some(Instant::now());
        // The long feed covers several days: forget telegrams it no longer lists.
        let listed: std::collections::HashSet<&str> = entries.iter().map(|(_, u)| u.as_str()).collect();
        st.reports.retain(|url, _| listed.contains(url.as_str()));
    }
    let reports: Vec<Report> = st.reports.values().flatten().cloned().collect();
    Ok(EarthquakeData {
        quakes: merge(&reports, ctx.tz),
        source: SOURCE_LABEL.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telegrams_only_from_jma() {
        assert!(is_jma_telegram_url(
            "https://www.data.jma.go.jp/developer/xml/data/x_VXSE53.xml"
        ));
        assert!(!is_jma_telegram_url(
            "http://www.data.jma.go.jp/developer/xml/data/x.xml"
        ));
        assert!(!is_jma_telegram_url("https://www.data.jma.go.jp.evil.example/x.xml"));
        assert!(!is_jma_telegram_url("https://example/ash.xml"));
        assert!(!is_jma_telegram_url("https://www.data.jma.go.jp/../x.xml"));
    }

    const VXSE53: &str = include_str!("../../fixtures/earthquake/jma/VXSE53_sample.xml");
    const VXSE51: &str = include_str!("../../fixtures/earthquake/jma/VXSE51_sample.xml");
    const TZ: Tz = chrono_tz::Asia::Tokyo;

    #[test]
    fn parses_hypocentre_and_intensity_report() {
        let r = parse_report(VXSE53).unwrap().unwrap();
        assert_eq!(r.event_id, "20261011061904");
        assert_eq!(r.hypocenter.as_deref(), Some("熊本県天草・芦北地方"));
        assert_eq!(r.magnitude, Some(2.3));
        assert_eq!(r.depth_km, Some(0), "ごく浅い");
        assert_eq!(r.max_shindo, Some(Shindo::S1));
        assert_eq!(r.tsunami, Some(Tsunami::None));
        assert!(!r.cancelled && !r.foreign);
        // ArrivalTime, matching JMA's "06時19分ころ" (OriginTime is 06:18).
        assert_eq!(r.origin_time.unwrap().to_rfc3339(), "2026-10-11T06:19:00+09:00");
    }

    #[test]
    fn parses_quick_intensity_report_without_hypocentre() {
        let r = parse_report(VXSE51).unwrap().unwrap();
        assert_eq!(r.max_shindo, Some(Shindo::S3));
        assert!(r.hypocenter.is_none() && r.magnitude.is_none());
        assert_eq!(r.tsunami, None, "「今後の情報に注意」 says nothing about tsunami");
        assert_eq!(r.origin_time.unwrap().to_rfc3339(), "2026-10-08T07:14:00+09:00");
    }

    #[test]
    fn depth_and_unknown_magnitude() {
        assert_eq!(
            depth_km(
                Some("+35.7+140.0-50000/"),
                Some("北緯３５．７度　東経１４０．０度　深さ　５０ｋｍ")
            ),
            Some(50)
        );
        assert_eq!(depth_km(Some("+35.7+140.0/"), Some("深さ不明")), None);
        let xml = VXSE53.replace(">2.3</jmx_eb:Magnitude>", ">NaN</jmx_eb:Magnitude>");
        assert_eq!(
            parse_report(&xml).unwrap().unwrap().magnitude,
            None,
            "Ｍ不明 is not invented"
        );
    }

    #[test]
    fn merges_reports_and_honours_cancellation() {
        let quick = parse_report(VXSE51).unwrap().unwrap();
        let mut detail = parse_report(VXSE53).unwrap().unwrap();
        // Pretend the detailed report belongs to the same event, issued later.
        detail.event_id = quick.event_id.clone();
        detail.report_time = quick.report_time + chrono::Duration::minutes(5);
        detail.max_shindo = Some(Shindo::S2);
        let q = merge(&[quick.clone(), detail.clone()], TZ);
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].hypocenter.as_deref(), Some("熊本県天草・芦北地方"));
        assert_eq!(q[0].max_shindo, Some(Shindo::S3), "max over reports");
        let mut cancel = detail.clone();
        cancel.cancelled = true;
        cancel.report_time += chrono::Duration::minutes(5);
        assert!(merge(&[quick, detail, cancel], TZ).is_empty());
    }

    #[test]
    fn cancellation_flag_and_tsunami_comments() {
        let xml = VXSE53.replace("<InfoType>発表</InfoType>", "<InfoType>取消</InfoType>");
        assert!(parse_report(&xml).unwrap().unwrap().cancelled);
        assert_eq!(
            tsunami_from_comment("津波注意報を発表しています。"),
            Some(Tsunami::Watch)
        );
        assert_eq!(
            tsunami_from_comment("若干の海面変動があるかもしれません"),
            Some(Tsunami::NonEffective)
        );
        assert_eq!(tsunami_from_comment("今後の情報に注意してください。"), None);
    }

    #[test]
    fn parses_atom_feed() {
        let feed = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom" lang="ja"><title>高頻度（地震火山）</title>
<entry><title>震源・震度に関する情報</title><id>a</id>
<link type="application/xml" href="https://www.data.jma.go.jp/developer/xml/data/x_VXSE53.xml"/></entry>
<entry><title>降灰予報（定時）</title><link type="application/xml" href="https://example/ash.xml"/></entry>
</feed>"#;
        let e = parse_feed(feed).unwrap();
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].0, "震源・震度に関する情報");
        assert!(e[0].1.ends_with("x_VXSE53.xml"));
    }

    #[test]
    fn ignores_other_telegrams() {
        let other = r#"<Report xmlns="http://xml.kishou.go.jp/jmaxml1/"><Control><Title>降灰予報（定時）</Title></Control>
<Head xmlns="http://xml.kishou.go.jp/jmaxml1/informationBasis1/"><Title>降灰予報</Title><ReportDateTime>2026-10-11T06:00:00+09:00</ReportDateTime></Head><Body/></Report>"#;
        assert_eq!(parse_report(other).unwrap(), None);
    }
}
