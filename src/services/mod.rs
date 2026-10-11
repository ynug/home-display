//! Data services. Each source runs as one Tokio task on the background
//! runtime: fetch → normalize → `Repository::store`, then sleep with
//! interval/backoff. Nothing here touches the UI thread.

pub mod calendar;
pub mod earthquake;
pub mod holiday;
pub mod jma_quake;
pub mod radar;
pub mod weather;

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Notify, watch};

use crate::clock;
use crate::config::{AppConfig, Paths, ProfileSettings, Provider};
use crate::fixtures;
use crate::repository::{Payload, Repository};
use crate::status::{ErrorKind, Origin, SourceError, SourceId, SourceStatus, next_delay};

pub const USER_AGENT: &str = concat!(
    "home-display/",
    env!("CARGO_PKG_VERSION"),
    " (always-on home dashboard; low-frequency polling)"
);

/// Thin wrapper around a shared reqwest client with timeouts, a response
/// size cap and error classification that never leaks the request URL.
#[derive(Clone)]
pub struct Http {
    client: reqwest::Client,
}

impl Http {
    pub fn new() -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .pool_idle_timeout(Duration::from_secs(60))
            // Redirects must not forward the (secret) iCal URL as Referer,
            // and every source is HTTPS: never follow a downgrade to http.
            .referer(false)
            .https_only(true)
            .build()?;
        Ok(Http { client })
    }

    pub async fn get_bytes(&self, url: &str, max_bytes: usize) -> Result<Vec<u8>, SourceError> {
        let mut resp = self.client.get(url).send().await.map_err(classify)?;
        let status = resp.status();
        if status.as_u16() == 429 {
            return Err(SourceError::new(ErrorKind::RateLimited, "HTTP 429"));
        }
        if !status.is_success() {
            return Err(SourceError::new(
                ErrorKind::Http(status.as_u16()),
                format!("HTTP {}", status.as_u16()),
            ));
        }
        if resp.content_length().is_some_and(|l| l as usize > max_bytes) {
            return Err(SourceError::parse(format!("応答が大きすぎます (> {max_bytes} bytes)")));
        }
        let mut buf = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(classify)? {
            if buf.len() + chunk.len() > max_bytes {
                return Err(SourceError::parse(format!("応答が大きすぎます (> {max_bytes} bytes)")));
            }
            buf.extend_from_slice(&chunk);
        }
        Ok(buf)
    }

    pub async fn get_text(&self, url: &str, max_bytes: usize) -> Result<String, SourceError> {
        let bytes = self.get_bytes(url, max_bytes).await?;
        String::from_utf8(bytes).map_err(|_| SourceError::parse("UTF-8 ではない応答です"))
    }

    /// Like `get_bytes`, but a 404 is `Ok(None)` (missing map tiles over the sea).
    pub async fn get_optional(&self, url: &str, max_bytes: usize) -> Result<Option<Vec<u8>>, SourceError> {
        match self.get_bytes(url, max_bytes).await {
            Ok(b) => Ok(Some(b)),
            Err(SourceError {
                kind: ErrorKind::Http(404),
                ..
            }) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// Map a reqwest error to a category. `without_url()` keeps secret iCal URLs
/// out of messages and logs.
fn classify(e: reqwest::Error) -> SourceError {
    let kind = if e.is_timeout() {
        ErrorKind::Timeout
    } else if e.is_connect() || e.is_request() {
        ErrorKind::Network
    } else if e.is_decode() || e.is_body() {
        ErrorKind::Parse
    } else if let Some(s) = e.status() {
        ErrorKind::Http(s.as_u16())
    } else {
        ErrorKind::Network
    };
    let e = e.without_url();
    let mut msg = e.to_string();
    let mut src = std::error::Error::source(&e);
    while let Some(s) = src {
        msg.push_str(": ");
        msg.push_str(&s.to_string());
        src = s.source();
    }
    SourceError::new(kind, redact_urls(&msg))
}

/// Belt and braces: strip anything that looks like a URL from a message.
pub fn redact_urls(s: &str) -> String {
    s.split_whitespace()
        .map(|w| if w.contains("://") { "<url>" } else { w })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Where weather is fetched and the radar is centred. Editable at runtime on
/// the settings screen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Place {
    pub latitude: f64,
    pub longitude: f64,
    pub radar_latitude: f64,
    pub radar_longitude: f64,
}

impl Place {
    pub fn from_config(cfg: &AppConfig) -> Place {
        let (radar_latitude, radar_longitude) = cfg.radar_center();
        Place {
            latitude: cfg.location.latitude,
            longitude: cfg.location.longitude,
            radar_latitude,
            radar_longitude,
        }
    }
}

/// Shared context for all source tasks.
#[derive(Clone)]
pub struct Context {
    pub repo: Arc<Repository>,
    pub http: Http,
    pub cfg: Arc<AppConfig>,
    pub tz: chrono_tz::Tz,
    pub paths: Paths,
    pub profile: watch::Receiver<ProfileSettings>,
    /// `notify_waiters()` asks every source to refresh now (rate-limited).
    pub wake: Arc<Notify>,
    /// Radar download progress, polled by the UI once per second.
    pub radar_progress: Arc<radar::Progress>,
    /// Radar map zoom (settings screen can change it at runtime).
    pub radar_zoom: watch::Receiver<u8>,
    /// Weather location / radar centre (settings screen can change it).
    pub place: watch::Receiver<Place>,
}

/// Minimum time between two attempts of the same source, even on manual refresh.
const MIN_GAP: Duration = Duration::from_secs(15);
/// Hard ceiling for one fetch cycle (radar can involve dozens of tiles).
const CYCLE_TIMEOUT: Duration = Duration::from_secs(180);

/// Generic refresh loop for one source.
pub async fn run_periodic<F, Fut>(
    ctx: Context,
    id: SourceId,
    interval: impl Fn(&Context) -> Duration + Send + 'static,
    mut fetch: F,
) where
    F: FnMut(Context) -> Fut + Send + 'static,
    Fut: Future<Output = Result<Payload, SourceError>> + Send,
{
    let mut failures = 0u32;
    let mut profile_rx = ctx.profile.clone();
    let mut zoom_rx = ctx.radar_zoom.clone();
    let mut place_rx = ctx.place.clone();
    let follows_place = matches!(id, SourceId::Weather | SourceId::Radar);
    loop {
        let started = tokio::time::Instant::now();
        ctx.repo.mark_loading(id);
        let result = match tokio::time::timeout(CYCLE_TIMEOUT, fetch(ctx.clone())).await {
            Ok(r) => r,
            Err(_) => Err(SourceError::new(ErrorKind::Timeout, "処理が時間内に終わりませんでした")),
        };
        let ok = result.is_ok();
        failures = if ok { 0 } else { failures + 1 };
        let delay = next_delay(ok, failures, interval(&ctx));
        let now = clock::now_utc();
        let next = chrono::Duration::from_std(delay).ok().map(|d| now + d);
        match result {
            Ok(payload) => {
                tracing::debug!(source = id.label(), "updated");
                ctx.repo.store(payload, now, next);
            }
            Err(e) => {
                tracing::warn!(source = id.label(), failures, "{e}");
                ctx.repo.fail(id, e, now, next);
            }
        }

        // A manual refresh or a setting change brings the next attempt
        // forward, but never closer than MIN_GAP to the previous one.
        let mut deadline = started + delay.max(MIN_GAP);
        let soonest = started + MIN_GAP;
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => break,
                _ = ctx.wake.notified() => deadline = deadline.min(soonest),
                changed = profile_rx.changed(), if id == SourceId::Radar => {
                    if changed.is_err() { return; }
                    deadline = deadline.min(soonest);
                }
                changed = zoom_rx.changed(), if id == SourceId::Radar => {
                    if changed.is_err() { return; }
                    deadline = deadline.min(soonest);
                }
                changed = place_rx.changed(), if follows_place => {
                    if changed.is_err() { return; }
                    deadline = deadline.min(soonest);
                }
            }
        }
    }
}

fn stale(minutes: u64) -> chrono::Duration {
    chrono::Duration::minutes(minutes as i64)
}

/// Register statuses and start one task per enabled source.
pub fn spawn_all(rt: &tokio::runtime::Handle, ctx: Context) {
    let cfg = ctx.cfg.clone();
    let scenario = fixtures::Scenario::parse(&cfg.fixture_scenario).unwrap_or_default();
    let now = clock::now_utc();
    let fixture = fixtures::FixtureSet::load(scenario, clock::now_in(ctx.tz));

    let mut any_fixture = false;
    let mut setup = |id: SourceId, provider: Provider, live_name: &str, stale_after: chrono::Duration| -> Provider {
        let provider = cfg.provider_for(provider);
        let origin = match provider {
            Provider::Live => Origin::Live(live_name.into()),
            Provider::Fixture => {
                any_fixture = true;
                Origin::Fixture
            }
            Provider::Disabled => Origin::Disabled,
        };
        ctx.repo.set_status(id, SourceStatus::new(origin, stale_after));
        provider
    };

    // Calendar: live only when a secret URL is available.
    let cal_provider = setup(
        SourceId::Calendar,
        Provider::Live,
        "iCal",
        stale(cfg.calendar.stale_minutes),
    );
    let weather_provider = setup(
        SourceId::Weather,
        cfg.weather.provider,
        "Open-Meteo",
        stale(cfg.weather.stale_minutes),
    );
    let quake_label = match cfg.earthquake.source {
        crate::config::QuakeSource::Jma => "気象庁 防災情報XML",
        crate::config::QuakeSource::P2pquake => "P2P地震情報 (気象庁)",
    };
    let radar_label = match cfg.radar.source {
        crate::config::RadarSource::Jma => "気象庁ナウキャスト + 地理院タイル",
        crate::config::RadarSource::Rainviewer => "RainViewer + 地理院タイル",
    };
    let quake_provider = setup(
        SourceId::Earthquake,
        cfg.earthquake.provider,
        quake_label,
        stale(cfg.earthquake.stale_minutes),
    );
    let radar_provider = setup(
        SourceId::Radar,
        cfg.radar.provider,
        radar_label,
        stale(cfg.radar.stale_minutes),
    );
    let holiday_provider = setup(
        SourceId::Holiday,
        cfg.holiday.provider,
        "内閣府 祝日CSV",
        chrono::Duration::days(cfg.holiday.refresh_days as i64 * 4),
    );

    if any_fixture {
        ctx.repo.set_fixture_scenario(Some(scenario.name().to_string()));
    }

    // ---- Calendar ----
    match cal_provider {
        Provider::Live => match cfg.resolve_ical_url() {
            Ok(Some(url)) => {
                rt.spawn(run_periodic(
                    ctx.clone(),
                    SourceId::Calendar,
                    // Regular refresh, plus one right after midnight for the new day.
                    |c| {
                        Duration::from_secs(c.cfg.calendar.refresh_minutes.max(1) * 60)
                            .min(calendar::until_day_change(clock::now_in(c.tz)))
                    },
                    move |c| {
                        let url = url.clone();
                        async move { calendar::fetch(&c, &url).await.map(|d| Payload::Calendar(Arc::new(d))) }
                    },
                ));
            }
            Ok(None) => ctx.repo.update_status(SourceId::Calendar, |s| {
                s.origin = Origin::Unconfigured(format!("{} が未設定です", crate::config::ICAL_URL_ENV));
            }),
            Err(e) => {
                tracing::warn!("calendar: {e}");
                ctx.repo.update_status(SourceId::Calendar, |s| {
                    s.record_failure(now, SourceError::new(ErrorKind::Config, e.to_string()));
                });
            }
        },
        Provider::Fixture => fixture.apply_calendar(&ctx.repo, now),
        Provider::Disabled => {}
    }

    // ---- Weather ----
    match weather_provider {
        Provider::Live => {
            rt.spawn(run_periodic(
                ctx.clone(),
                SourceId::Weather,
                |c| Duration::from_secs(c.cfg.weather.refresh_minutes.max(5) * 60),
                |c| async move { weather::fetch(&c).await.map(|d| Payload::Weather(Arc::new(d))) },
            ));
        }
        Provider::Fixture => fixture.apply_weather(&ctx.repo, now),
        Provider::Disabled => {}
    }

    // ---- Earthquake ----
    match quake_provider {
        Provider::Live => {
            let jma = Arc::new(tokio::sync::Mutex::new(jma_quake::JmaState::default()));
            rt.spawn(run_periodic(
                ctx.clone(),
                SourceId::Earthquake,
                // The JMA feed updates every minute (P2PQuake allows 60 req/min).
                |c| Duration::from_secs(c.cfg.earthquake.refresh_seconds.max(30)),
                move |c| {
                    let jma = jma.clone();
                    async move {
                        earthquake::fetch(&c, &jma)
                            .await
                            .map(|d| Payload::Earthquake(Arc::new(d)))
                    }
                },
            ));
        }
        Provider::Fixture => fixture.apply_earthquake(&ctx.repo, now),
        Provider::Disabled => {}
    }

    // ---- Radar ----
    match radar_provider {
        Provider::Live => {
            let state = Arc::new(tokio::sync::Mutex::new(radar::LiveState::default()));
            rt.spawn(run_periodic(
                ctx.clone(),
                SourceId::Radar,
                |c| c.cfg.radar_refresh(&c.profile.borrow()),
                move |c| {
                    let state = state.clone();
                    async move { radar::fetch(&c, &state).await.map(|d| Payload::Radar(Arc::new(d))) }
                },
            ));
        }
        Provider::Fixture => {
            rt.spawn(radar::run_fixture(ctx.clone(), fixture.radar_spec()));
        }
        Provider::Disabled => {}
    }

    // ---- Holidays ----
    match holiday_provider {
        Provider::Live => {
            holiday::load_cached(&ctx);
            rt.spawn(run_periodic(
                ctx.clone(),
                SourceId::Holiday,
                |c| Duration::from_secs(c.cfg.holiday.refresh_days.max(1) * 86_400),
                |c| async move { holiday::fetch(&c).await.map(|d| Payload::Holidays(Arc::new(d))) },
            ));
        }
        Provider::Fixture => fixture.apply_holidays(&ctx.repo, now),
        Provider::Disabled => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn ctx() -> (Context, watch::Sender<u8>) {
        let (_ptx, profile) = watch::channel(crate::config::ProfileSettings::defaults(
            crate::config::Profile::Balanced,
        ));
        std::mem::forget(_ptx);
        let (ztx, radar_zoom) = watch::channel(9u8);
        let (ptx, place) = watch::channel(Place::from_config(&AppConfig::default()));
        std::mem::forget(ptx);
        let ctx = Context {
            repo: Repository::new(crate::repository::Snapshot::default()),
            http: Http::new().unwrap(),
            cfg: Arc::new(AppConfig::default()),
            tz: chrono_tz::Asia::Tokyo,
            paths: Paths {
                config_file: None,
                cache_dir: std::env::temp_dir(),
                state_dir: std::env::temp_dir(),
            },
            profile,
            wake: Arc::new(Notify::new()),
            radar_progress: Arc::new(radar::Progress::default()),
            radar_zoom,
            place,
        };
        (ctx, ztx)
    }

    /// A setting change (here: radar zoom) or a manual refresh right after a
    /// fetch must not be dropped: it runs once MIN_GAP has passed instead of
    /// waiting for the regular 10-minute interval.
    #[tokio::test(start_paused = true)]
    async fn change_shortly_after_fetch_runs_at_min_gap() {
        let (ctx, zoom_tx) = ctx();
        ctx.repo.set_status(
            SourceId::Radar,
            SourceStatus::new(Origin::Live("t".into()), chrono::Duration::minutes(40)),
        );
        let calls = Arc::new(AtomicU32::new(0));
        let c2 = calls.clone();
        tokio::spawn(run_periodic(
            ctx.clone(),
            SourceId::Radar,
            |_| Duration::from_secs(600),
            move |_| {
                c2.fetch_add(1, Ordering::SeqCst);
                async { Err::<Payload, _>(SourceError::new(ErrorKind::Network, "test")) }
            },
        ));
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // Failure → 30 s backoff; a zoom change at t=2 s brings it to t=15 s.
        zoom_tx.send(8).unwrap();
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1, "not before MIN_GAP");
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "ran at MIN_GAP, not after the 30 s backoff"
        );

        // Manual refresh shortly after: also deferred to MIN_GAP, not dropped.
        tokio::time::sleep(Duration::from_secs(1)).await;
        ctx.wake.notify_waiters();
        tokio::time::sleep(Duration::from_secs(16)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }
}
