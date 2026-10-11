//! Japanese national holidays from the Cabinet Office (内閣府) CSV.
//! <https://www8.cao.go.jp/chosei/shukujitsu/gaiyou.html>
//!
//! Lookup order: on-disk cache (UTF-8 copy) → bundled snapshot. The network
//! is only contacted when the cache is older than `holiday.refresh_days`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use chrono::{DateTime, NaiveDate, Utc};

use super::Context;
use crate::model::Holidays;
use crate::repository::Payload;
use crate::status::{ErrorKind, SourceError};

/// UTF-8 copy of the official CSV (2024 onwards), bundled for offline first boot.
pub const BUNDLED_CSV: &str = include_str!("../../fixtures/holiday/syukujitsu-snapshot.csv");
/// `Last-Modified` of the official file when the snapshot was taken.
pub const BUNDLED_AS_OF: &str = "2026-02-02T00:30:17Z";

fn cache_path(ctx: &Context) -> PathBuf {
    ctx.paths.cache_dir.join("holidays.csv")
}

/// Parse `YYYY/M/D,名称` lines. Accepts UTF-8 or Shift_JIS input.
pub fn parse_csv(bytes: &[u8]) -> Result<Holidays, SourceError> {
    let text: std::borrow::Cow<'_, str> = match std::str::from_utf8(bytes) {
        Ok(s) => s.into(),
        Err(_) => {
            let (s, _, had_errors) = encoding_rs::SHIFT_JIS.decode(bytes);
            if had_errors {
                return Err(SourceError::parse("祝日CSVの文字コードを解釈できません"));
            }
            s
        }
    };
    let mut h = Holidays::default();
    for line in text.lines() {
        let mut parts = line.trim().splitn(2, ',');
        let (Some(date), Some(name)) = (parts.next(), parts.next()) else {
            continue;
        };
        if let Ok(d) = NaiveDate::parse_from_str(date.trim(), "%Y/%m/%d") {
            h.days.insert(d, name.trim().to_owned());
        }
    }
    if h.days.len() < 10 {
        return Err(SourceError::parse("祝日CSVに有効な行がありません"));
    }
    Ok(h)
}

pub fn bundled() -> Holidays {
    parse_csv(BUNDLED_CSV.as_bytes()).expect("bundled holiday snapshot is valid")
}

fn read_cache(ctx: &Context) -> Option<(Holidays, SystemTime)> {
    let path = cache_path(ctx);
    let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok()?;
    let bytes = std::fs::read(&path).ok()?;
    parse_csv(&bytes).ok().map(|h| (h, modified))
}

/// Show cached or bundled holidays immediately at startup.
pub fn load_cached(ctx: &Context) {
    let (h, at) = match read_cache(ctx) {
        Some((h, m)) => (h, DateTime::<Utc>::from(m)),
        None => (bundled(), BUNDLED_AS_OF.parse().expect("valid timestamp")),
    };
    ctx.repo.store_cached(Payload::Holidays(Arc::new(h)), at);
}

pub async fn fetch(ctx: &Context) -> Result<Holidays, SourceError> {
    let max_age = Duration::from_secs(ctx.cfg.holiday.refresh_days.max(1) * 86_400);
    if let Some((h, modified)) = read_cache(ctx)
        && SystemTime::now().duration_since(modified).unwrap_or(Duration::MAX) < max_age
    {
        return Ok(h);
    }
    let bytes = ctx.http.get_bytes(&ctx.cfg.holiday.url, 1024 * 1024).await?;
    let h = parse_csv(&bytes)?;
    write_cache(ctx, &h).map_err(|e| SourceError::new(ErrorKind::Io, e.to_string()))?;
    Ok(h)
}

fn write_cache(ctx: &Context, h: &Holidays) -> std::io::Result<()> {
    let path = cache_path(ctx);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut out = String::from("国民の祝日・休日月日,国民の祝日・休日名称\n");
    for (d, name) in &h.days {
        out.push_str(&format!("{},{}\n", d.format("%Y/%-m/%-d"), name));
    }
    let tmp = path.with_extension("csv.tmp");
    std::fs::write(&tmp, out)?;
    std::fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_snapshot_has_known_holidays() {
        let h = bundled();
        assert_eq!(
            h.name(NaiveDate::from_ymd_opt(2026, 10, 12).unwrap()),
            Some("スポーツの日")
        );
        assert_eq!(h.name(NaiveDate::from_ymd_opt(2026, 10, 11).unwrap()), None);
        assert!(h.last_date().unwrap().year_ce().1 >= 2027);
    }

    #[test]
    fn decodes_shift_jis() {
        let (sjis, _, _) = encoding_rs::SHIFT_JIS.encode(BUNDLED_CSV);
        let h = parse_csv(&sjis).unwrap();
        assert_eq!(h.days.len(), bundled().days.len());
    }

    #[test]
    fn rejects_non_csv() {
        assert!(parse_csv(b"<html>maintenance</html>").is_err());
    }

    use chrono::Datelike;
}
