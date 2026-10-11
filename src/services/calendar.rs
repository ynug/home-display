//! iCalendar (Google Calendar "secret address in iCal format") source.
//!
//! The URL is a secret: it is resolved by `AppConfig::resolve_ical_url`,
//! never logged, and errors are built with `reqwest::Error::without_url`.

use std::collections::HashSet;

use chrono::{DateTime, Days, NaiveDate, NaiveDateTime, TimeZone};
use chrono_tz::Tz;
use icalendar::{CalendarDateTime, Component, DatePerhapsTime, EventLike, EventStatus};

use super::Context;
use crate::clock;
use crate::model::{CalendarData, CalendarEvent};
use crate::status::SourceError;

const MAX_ICS_BYTES: usize = 16 * 1024 * 1024;
const MAX_OCCURRENCES: usize = 2_000;

pub async fn fetch(ctx: &Context, url: &str) -> Result<CalendarData, SourceError> {
    let text = ctx.http.get_text(url, MAX_ICS_BYTES).await?;
    let now = clock::now_in(ctx.tz);
    let (start, end) = window(now, ctx.cfg.calendar.horizon_days);
    // Parsing/expansion is CPU work; keep it off the async workers.
    let tz = ctx.tz;
    tokio::task::spawn_blocking(move || parse_ics(&text, tz, start, end))
        .await
        .map_err(|e| SourceError::parse(format!("parser task failed: {e}")))?
}

/// `[today 00:00, today + days 00:00)` in the display time zone.
/// Always covers at least today and tomorrow (the card shows both).
pub fn window(now: DateTime<Tz>, days: u32) -> (DateTime<Tz>, DateTime<Tz>) {
    let tz = now.timezone();
    let today = now.date_naive();
    let end_date = today + Days::new(days.max(2) as u64);
    (local_midnight(tz, today), local_midnight(tz, end_date))
}

/// Delay after midnight before the day-change refresh (lets the clock settle).
const DAY_CHANGE_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// Time until shortly after the next local midnight, so the calendar is
/// re-fetched for the new day right when the date changes.
pub fn until_day_change(now: DateTime<Tz>) -> std::time::Duration {
    let next = local_midnight(now.timezone(), now.date_naive() + Days::new(1));
    (next - now).to_std().unwrap_or_default() + DAY_CHANGE_GRACE
}

pub fn local_midnight(tz: Tz, d: NaiveDate) -> DateTime<Tz> {
    let naive = d.and_hms_opt(0, 0, 0).expect("midnight");
    tz.from_local_datetime(&naive)
        .earliest()
        // A DST gap at midnight: fall back to the UTC interpretation.
        .unwrap_or_else(|| tz.from_utc_datetime(&naive))
}

fn localize(tz: Tz, naive: NaiveDateTime) -> DateTime<Tz> {
    tz.from_local_datetime(&naive)
        .earliest()
        .unwrap_or_else(|| tz.from_utc_datetime(&naive))
}

/// Convert an iCal date/time to the display zone. Floating times and
/// unknown TZIDs (e.g. Windows zone names) are interpreted in `tz`.
fn to_local(v: &DatePerhapsTime, tz: Tz) -> (DateTime<Tz>, bool) {
    match v {
        DatePerhapsTime::Date(d) => (local_midnight(tz, *d), true),
        DatePerhapsTime::DateTime(cdt) => {
            let dt = match cdt {
                CalendarDateTime::Utc(u) => u.with_timezone(&tz),
                CalendarDateTime::Floating(n) => localize(tz, *n),
                CalendarDateTime::WithTimezone { date_time, .. } => match cdt.try_into_utc() {
                    Some(u) => u.with_timezone(&tz),
                    None => localize(tz, *date_time),
                },
            };
            (dt, false)
        }
    }
}

fn clean_text(s: &str) -> String {
    let s = s
        .replace("\\n", " ")
        .replace("\\N", " ")
        .replace("\\,", ",")
        .replace("\\;", ";")
        .replace("\\\\", "\\");
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn parse_ics(
    text: &str,
    tz: Tz,
    window_start: DateTime<Tz>,
    window_end: DateTime<Tz>,
) -> Result<CalendarData, SourceError> {
    if !text.trim_start().starts_with("BEGIN:VCALENDAR") {
        return Err(SourceError::parse("iCalendar 形式ではありません"));
    }
    let cal: icalendar::Calendar = text
        .parse()
        .map_err(|e: String| SourceError::parse(format!("ICS: {}", e.lines().next().unwrap_or(""))))?;

    // Occurrences replaced by a RECURRENCE-ID override (including cancelled ones).
    let mut overridden: HashSet<(String, i64)> = HashSet::new();
    for ev in cal.events() {
        if let (Some(uid), Some(rid)) = (ev.get_uid(), ev.get_recurrence_id()) {
            overridden.insert((uid.to_owned(), to_local(&rid, tz).0.timestamp()));
        }
    }

    let mut out: Vec<CalendarEvent> = Vec::new();
    for ce in cal.calendar_events() {
        let ev = ce.event();
        if matches!(ev.get_status(), Some(EventStatus::Cancelled)) {
            continue;
        }
        let Some(start_raw) = ev.get_start() else { continue };
        let (start, all_day) = to_local(&start_raw, tz);
        let end = match ev.get_end() {
            Some(e) => to_local(&e, tz).0,
            None => match ev.property_value("DURATION").and_then(parse_duration) {
                Some(d) => start + d,
                None if all_day => start + chrono::Duration::days(1),
                None => start,
            },
        };
        let duration = (end - start).max(chrono::Duration::zero());
        let title = ev
            .get_summary()
            .map(clean_text)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "(タイトルなし)".into());
        let location = ev.get_location().map(clean_text).filter(|s| !s.is_empty());
        let uid = ev.get_uid().unwrap_or_default().to_owned();

        let recurring = ev.property_value("RRULE").is_some() || ev.multi_properties().contains_key("RDATE");
        if !recurring || ev.get_recurrence_id().is_some() {
            push_if_overlaps(
                &mut out,
                &title,
                &location,
                start,
                end,
                all_day,
                window_start,
                window_end,
            );
            continue;
        }

        let set = match ce.get_recurrence() {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!("skipping event with unparsable recurrence: {e}");
                continue;
            }
        };
        let floating = matches!(start_raw, DatePerhapsTime::DateTime(CalendarDateTime::Floating(_)));
        let after = (window_start - duration - chrono::Duration::seconds(1)).with_timezone(&icalendar::rrule::Tz::UTC);
        let before = window_end.with_timezone(&icalendar::rrule::Tz::UTC);
        let result = set.after(after).before(before).all(MAX_OCCURRENCES as u16);
        for occ in result.dates {
            let occ_start = if all_day {
                local_midnight(tz, occ.date_naive())
            } else if floating {
                localize(tz, occ.naive_local())
            } else {
                occ.with_timezone(&tz)
            };
            if overridden.contains(&(uid.clone(), occ_start.timestamp())) {
                continue;
            }
            let occ_end = if all_day {
                let days = duration.num_days().max(1) as u64;
                local_midnight(tz, occ_start.date_naive() + Days::new(days))
            } else {
                occ_start + duration
            };
            push_if_overlaps(
                &mut out,
                &title,
                &location,
                occ_start,
                occ_end,
                all_day,
                window_start,
                window_end,
            );
            if out.len() >= MAX_OCCURRENCES {
                break;
            }
        }
    }

    out.sort_by(|a, b| {
        b.all_day
            .cmp(&a.all_day)
            .then(a.start.cmp(&b.start))
            .then(a.end.cmp(&b.end))
            .then(a.title.cmp(&b.title))
    });
    out.dedup();
    Ok(CalendarData { events: out })
}

#[allow(clippy::too_many_arguments)]
fn push_if_overlaps(
    out: &mut Vec<CalendarEvent>,
    title: &str,
    location: &Option<String>,
    start: DateTime<Tz>,
    end: DateTime<Tz>,
    all_day: bool,
    window_start: DateTime<Tz>,
    window_end: DateTime<Tz>,
) {
    let end_cmp = if end > start {
        end
    } else {
        start + chrono::Duration::seconds(1)
    };
    if start < window_end && end_cmp > window_start {
        out.push(CalendarEvent {
            title: title.to_owned(),
            location: location.clone(),
            start,
            end,
            all_day,
        });
    }
}

/// Minimal RFC 5545 DURATION parser (`P1D`, `PT1H30M`, `P1W`, `-PT15M` …).
fn parse_duration(s: &str) -> Option<chrono::Duration> {
    let s = s.trim();
    let (neg, s) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let s = s.strip_prefix('P')?;
    let mut total = chrono::Duration::zero();
    let mut num = String::new();
    let mut in_time = false;
    for c in s.chars() {
        match c {
            'T' => in_time = true,
            '0'..='9' => num.push(c),
            _ => {
                let n: i64 = num.parse().ok()?;
                num.clear();
                total += match (c, in_time) {
                    ('W', false) => chrono::Duration::weeks(n),
                    ('D', false) => chrono::Duration::days(n),
                    ('H', true) => chrono::Duration::hours(n),
                    ('M', true) => chrono::Duration::minutes(n),
                    ('S', true) => chrono::Duration::seconds(n),
                    _ => return None,
                };
            }
        }
    }
    Some(if neg { -total } else { total })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;

    const TZ: Tz = chrono_tz::Asia::Tokyo;

    fn win(y: i32, m: u32, d: u32) -> (DateTime<Tz>, DateTime<Tz>) {
        let now = TZ.with_ymd_and_hms(y, m, d, 9, 0, 0).unwrap();
        window(now, 3)
    }

    fn ics(body: &str) -> String {
        format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//test//EN\r\nX-WR-TIMEZONE:Asia/Tokyo\r\n{body}END:VCALENDAR\r\n"
        )
    }

    #[test]
    fn timed_and_all_day_events() {
        let text = ics(concat!(
            "BEGIN:VEVENT\r\nUID:a\r\nSUMMARY:会議\\, 定例\r\nLOCATION:本社 3F\r\n",
            "DTSTART;TZID=Asia/Tokyo:20261011T100000\r\nDTEND;TZID=Asia/Tokyo:20261011T110000\r\nEND:VEVENT\r\n",
            "BEGIN:VEVENT\r\nUID:b\r\nSUMMARY:旅行\r\nDTSTART;VALUE=DATE:20261011\r\nDTEND;VALUE=DATE:20261013\r\nEND:VEVENT\r\n",
            "BEGIN:VEVENT\r\nUID:c\r\nSUMMARY:UTC予定\r\nDTSTART:20261011T060000Z\r\nDTEND:20261011T070000Z\r\nEND:VEVENT\r\n",
        ));
        let (s, e) = win(2026, 10, 11);
        let data = parse_ics(&text, TZ, s, e).unwrap();
        assert_eq!(data.events.len(), 3);
        assert!(data.events[0].all_day);
        assert_eq!(data.events[0].title, "旅行");
        assert_eq!(data.events[1].title, "会議, 定例");
        assert_eq!(data.events[1].start.hour(), 10);
        assert_eq!(data.events[1].location.as_deref(), Some("本社 3F"));
        // 06:00Z == 15:00 JST
        assert_eq!(data.events[2].start.hour(), 15);
    }

    #[test]
    fn weekly_recurrence_with_exdate_and_override() {
        let text = ics(concat!(
            "BEGIN:VEVENT\r\nUID:r\r\nSUMMARY:朝会\r\n",
            "DTSTART;TZID=Asia/Tokyo:20260901T093000\r\nDTEND;TZID=Asia/Tokyo:20260901T094500\r\n",
            "RRULE:FREQ=DAILY\r\nEXDATE;TZID=Asia/Tokyo:20261012T093000\r\nEND:VEVENT\r\n",
            "BEGIN:VEVENT\r\nUID:r\r\nSUMMARY:朝会 (時間変更)\r\nRECURRENCE-ID;TZID=Asia/Tokyo:20261013T093000\r\n",
            "DTSTART;TZID=Asia/Tokyo:20261013T110000\r\nDTEND;TZID=Asia/Tokyo:20261013T111500\r\nEND:VEVENT\r\n",
        ));
        let (s, e) = win(2026, 10, 11);
        let data = parse_ics(&text, TZ, s, e).unwrap();
        let titles: Vec<_> = data
            .events
            .iter()
            .map(|e| (e.start.format("%d %H:%M").to_string(), e.title.clone()))
            .collect();
        assert_eq!(
            titles,
            vec![
                ("11 09:30".to_string(), "朝会".to_string()),
                ("13 11:00".to_string(), "朝会 (時間変更)".to_string()),
            ]
        );
    }

    #[test]
    fn cancelled_and_out_of_window_are_dropped() {
        let text = ics(concat!(
            "BEGIN:VEVENT\r\nUID:x\r\nSUMMARY:中止\r\nSTATUS:CANCELLED\r\n",
            "DTSTART;TZID=Asia/Tokyo:20261011T100000\r\nDTEND;TZID=Asia/Tokyo:20261011T110000\r\nEND:VEVENT\r\n",
            "BEGIN:VEVENT\r\nUID:y\r\nSUMMARY:昨日\r\n",
            "DTSTART;TZID=Asia/Tokyo:20261010T100000\r\nDTEND;TZID=Asia/Tokyo:20261010T110000\r\nEND:VEVENT\r\n",
        ));
        let (s, e) = win(2026, 10, 11);
        assert!(parse_ics(&text, TZ, s, e).unwrap().events.is_empty());
    }

    #[test]
    fn overnight_event_from_yesterday_is_kept() {
        let text = ics(concat!(
            "BEGIN:VEVENT\r\nUID:n\r\nSUMMARY:夜勤\r\n",
            "DTSTART;TZID=Asia/Tokyo:20261010T220000\r\nDTEND;TZID=Asia/Tokyo:20261011T060000\r\nEND:VEVENT\r\n",
        ));
        let (s, e) = win(2026, 10, 11);
        assert_eq!(parse_ics(&text, TZ, s, e).unwrap().events.len(), 1);
    }

    #[test]
    fn yearly_all_day_recurrence() {
        let text = ics(concat!(
            "BEGIN:VEVENT\r\nUID:bd\r\nSUMMARY:誕生日\r\nDTSTART;VALUE=DATE:20101012\r\n",
            "DTEND;VALUE=DATE:20101013\r\nRRULE:FREQ=YEARLY\r\nEND:VEVENT\r\n",
        ));
        let (s, e) = win(2026, 10, 11);
        let data = parse_ics(&text, TZ, s, e).unwrap();
        assert_eq!(data.events.len(), 1);
        assert_eq!(
            data.events[0].start.date_naive(),
            NaiveDate::from_ymd_opt(2026, 10, 12).unwrap()
        );
        assert!(data.events[0].all_day);
    }

    #[test]
    fn duration_property() {
        assert_eq!(parse_duration("PT1H30M"), Some(chrono::Duration::minutes(90)));
        assert_eq!(parse_duration("P1W"), Some(chrono::Duration::weeks(1)));
        assert_eq!(parse_duration("-PT15M"), Some(chrono::Duration::minutes(-15)));
        assert_eq!(parse_duration("garbage"), None);
    }

    #[test]
    fn rejects_html_error_pages() {
        let (s, e) = win(2026, 10, 11);
        assert!(parse_ics("<html>login</html>", TZ, s, e).is_err());
    }

    #[test]
    fn refetch_right_after_midnight() {
        let at = |h, m, s| TZ.with_ymd_and_hms(2026, 10, 11, h, m, s).unwrap();
        assert_eq!(until_day_change(at(23, 59, 0)).as_secs(), 60 + 5);
        assert_eq!(until_day_change(at(0, 0, 0)).as_secs(), 24 * 3600 + 5);
        assert_eq!(until_day_change(at(12, 0, 0)).as_secs(), 12 * 3600 + 5);
    }

    #[test]
    fn window_always_includes_tomorrow() {
        let now = TZ.with_ymd_and_hms(2026, 10, 11, 9, 0, 0).unwrap();
        let (s, e) = window(now, 1);
        assert_eq!(s, TZ.with_ymd_and_hms(2026, 10, 11, 0, 0, 0).unwrap());
        assert_eq!(e, TZ.with_ymd_and_hms(2026, 10, 13, 0, 0, 0).unwrap());
    }
}
