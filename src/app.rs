//! Application wiring: config → background runtime + services → UI controller.
//!
//! Threads:
//! * UI thread — Slint event loop, `Controller`, one 1 s `slint::Timer`
//!   (clock + rotation + freshness). Never does network or image decoding.
//! * Tokio runtime (2 workers) — one task per data source; CPU work
//!   (ICS expansion, radar compositing/decoding) on `spawn_blocking`.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use slint::{ComponentHandle, Image, ModelRc, Rgb8Pixel, SharedPixelBuffer, VecModel};
use tokio::sync::{Notify, watch};

use crate::config::{self, AppConfig, DataMode, Paths, Profile, ProfileSettings, UiOverrides};
use crate::model::RadarData;
use crate::repository::{Repository, Snapshot};
use crate::screen::{RotationSettings, Screen, ScreenManager};
use crate::services::{self, Http};
use crate::status::SourceId;
use crate::ui::{self, MainWindow};
use crate::{clock, diag, ui_bridge as bridge, view};

/// Command-line options (see `main.rs`).
#[derive(Debug, Default, Clone)]
pub struct Options {
    pub config: Option<PathBuf>,
    pub fixture: Option<String>,
    pub profile: Option<Profile>,
    pub screen: Option<Screen>,
    pub now: Option<DateTime<Utc>>,
    pub scale: Option<f32>,
    pub fullscreen: bool,
    pub no_rotate: bool,
    pub screenshot: Option<PathBuf>,
    pub screenshot_dir: Option<PathBuf>,
    /// Screenshot mode: advance the UI clock by this much before rendering
    /// (e.g. to capture a later radar animation frame).
    pub screenshot_advance: Option<Duration>,
    /// Screenshot mode: maximum wait for data before rendering (default 90 s).
    pub screenshot_wait: Option<Duration>,
}

pub struct Loaded {
    pub cfg: AppConfig,
    pub paths: Paths,
    pub overrides: UiOverrides,
    pub tz: Tz,
}

pub fn load(opts: &Options) -> anyhow::Result<Loaded> {
    let paths = Paths::discover(opts.config.clone());
    let mut cfg = match &paths.config_file {
        Some(p) => {
            let c = AppConfig::load(p)?;
            tracing::info!("config: {}", p.display());
            c
        }
        None => {
            tracing::info!("no config file found; using built-in defaults (fixture mode)");
            AppConfig::default()
        }
    };
    // Precedence: CLI > settings-screen overrides > config file.
    let overrides = config::load_overrides(&paths.overrides_file());
    cfg.apply_overrides(&overrides);
    cfg.normalize();
    cfg.validate()?;
    if let Some(sc) = &opts.fixture {
        if crate::fixtures::Scenario::parse(sc).is_none() {
            anyhow::bail!("unknown fixture scenario {sc:?} (try --list-fixtures)");
        }
        cfg.mode = DataMode::Fixture;
        cfg.fixture_scenario = sc.clone();
    }
    if let Some(p) = opts.profile {
        cfg.display.profile = p;
    }
    if opts.no_rotate {
        cfg.rotation.enabled = false;
    }
    let tz = cfg.timezone()?;
    Ok(Loaded {
        cfg,
        paths,
        overrides,
        tz,
    })
}

fn default_font() -> &'static str {
    if cfg!(target_os = "macos") {
        "Hiragino Sans"
    } else {
        "Noto Sans CJK JP"
    }
}

fn rotation(cfg: &AppConfig) -> RotationSettings {
    RotationSettings {
        enabled: cfg.rotation.enabled,
        overview: cfg.rotation.overview(),
        radar: cfg.rotation.radar(),
        settings_timeout: Duration::from_secs(cfg.rotation.settings_timeout_seconds.max(10) as u64),
    }
}

#[derive(Default)]
struct LastViews {
    header: Option<view::HeaderView>,
    clock: Option<view::ClockView>,
    calendar: Option<view::CalendarView>,
    weather: Option<view::WeatherView>,
    quake: Option<view::QuakeView>,
    footer: Option<view::FooterView>,
    radar: Option<(view::RadarView, bool)>,
    settings: Option<view::SettingsView>,
}

/// Decoded radar frames currently handed to the UI.
#[derive(Default)]
struct RadarImages {
    /// The data the current/pending decode belongs to.
    source: Option<Arc<RadarData>>,
    generation: u64,
    decoding: bool,
    count: usize,
    bytes: u64,
}

pub struct Controller {
    ui: slint::Weak<MainWindow>,
    cfg: AppConfig,
    paths: Paths,
    overrides: UiOverrides,
    tz: Tz,
    snap: Snapshot,
    screens: ScreenManager,
    profile_settings: ProfileSettings,
    profile_tx: watch::Sender<ProfileSettings>,
    wake: Arc<Notify>,
    rt: tokio::runtime::Handle,
    last: LastViews,
    radar: RadarImages,
    save_message: String,
    /// Screenshot mode: decode synchronously, no event loop available.
    sync_decode: bool,
    /// Settings-screen location fields were filled for the current visit.
    location_fields_filled: bool,
    last_minute: Option<i64>,
    /// Radar download progress (shared with the fetch task).
    radar_progress: Arc<services::radar::Progress>,
    last_progress: Option<services::radar::ProgressSnapshot>,
    zoom_tx: watch::Sender<u8>,
    place_tx: watch::Sender<services::Place>,
    repo: Arc<Repository>,
}

thread_local! {
    static CONTROLLER: RefCell<Weak<RefCell<Controller>>> = const { RefCell::new(Weak::new()) };
}

/// Run `f` on the UI-thread controller, if it is still alive.
fn with_controller(f: impl FnOnce(&mut Controller)) {
    let c = CONTROLLER.with(|c| c.borrow().upgrade());
    if let Some(c) = c {
        f(&mut c.borrow_mut());
    }
}

impl Controller {
    fn window(&self) -> Option<MainWindow> {
        self.ui.upgrade()
    }

    fn view_ctx<'a>(&'a self, now: DateTime<Tz>) -> view::ViewContext<'a> {
        view::ViewContext {
            snap: &self.snap,
            now,
            cfg: &self.cfg,
            profile: self.cfg.display.profile,
            settings: &self.profile_settings,
            fake_clock: clock::is_faked(),
            radar_progress: self.last_progress,
        }
    }

    /// Automatic radar dwell from the frames the radar screen will animate.
    fn update_radar_dwell(&mut self) {
        let dwell = match &self.snap.radar {
            Some(r) if !r.frames.is_empty() => {
                services::radar::auto_dwell(r.frames.len(), r.now_index, self.profile_settings.radar_frame_ms)
            }
            _ => crate::screen::DEFAULT_RADAR_DWELL,
        };
        self.screens.set_radar_auto_dwell(dwell);
    }

    pub fn on_snapshot(&mut self, snap: Snapshot) {
        if snap.revision < self.snap.revision {
            return; // an older publication overtaken by a newer one
        }
        self.snap = snap;
        let radar_on = self
            .snap
            .status(SourceId::Radar)
            .is_some_and(|s| s.origin != crate::status::Origin::Disabled);
        let before = self.screens.current();
        self.screens.set_radar_available(radar_on, Instant::now());
        // Rotate to the radar only once images exist (first fetch finished).
        let radar_ready = self.snap.radar.as_ref().is_some_and(|r| !r.frames.is_empty());
        self.screens.set_radar_ready(radar_ready);
        self.update_radar_dwell();
        if self.screens.current() != before {
            self.apply_screen();
        }
        self.sync_radar_images();
        self.refresh();
    }

    /// Called once per second, aligned to the wall-clock second.
    /// Only the clock is rebuilt every second; the other panels (relative
    /// times, freshness, "ongoing" events) are recomputed when the minute changes.
    pub fn tick(&mut self) {
        if self.screens.tick(Instant::now()).is_some() {
            self.apply_screen();
            return;
        }
        // Radar download progress is polled here, i.e. once per second.
        let progress = self.radar_progress.snapshot();
        let progress_changed = progress != self.last_progress;
        if progress_changed && let Some(p) = &progress {
            tracing::debug!(
                "radar progress {}% ({}/{} frames)",
                p.percent(),
                p.frames_done,
                p.frames_total
            );
        }
        self.last_progress = progress;
        let now = clock::now_in(self.tz);
        let minute = now.timestamp() / 60;
        if self.last_minute != Some(minute) || progress_changed {
            self.last_minute = Some(minute);
            self.refresh();
        } else {
            self.refresh_clock();
        }
    }

    fn refresh_clock(&mut self) {
        let Some(ui) = self.window() else { return };
        let clock_v = view::clock(&self.view_ctx(clock::now_in(self.tz)));
        if self.last.clock.as_ref() != Some(&clock_v) {
            ui.set_clock(bridge::clock(&clock_v));
            self.last.clock = Some(clock_v);
        }
    }

    /// Read the radar download progress (normally done by `tick`).
    pub fn poll_progress(&mut self) {
        self.last_progress = self.radar_progress.snapshot();
    }

    fn apply_screen(&mut self) {
        tracing::debug!(
            "screen → {:?} (radar dwell {:?})",
            self.screens.current(),
            self.screens.radar_dwell()
        );
        if self.screens.current() == Screen::Settings {
            if !self.location_fields_filled {
                self.fill_location_fields();
            }
        } else {
            self.location_fields_filled = false;
        }
        if let Some(ui) = self.window() {
            ui.set_screen(bridge::screen(self.screens.current()));
        }
        self.sync_radar_images();
        self.refresh();
    }

    /// Update only the properties whose view model changed.
    pub fn refresh(&mut self) {
        let Some(ui) = self.window() else { return };
        let now = clock::now_in(self.tz);
        let screen = self.screens.current();
        let (header, clock_v, weather, overview, radar, settings) = {
            let cx = self.view_ctx(now);
            let overview =
                (screen == Screen::Overview).then(|| (view::calendar(&cx), view::quake(&cx), view::footer(&cx)));
            let radar = (screen == Screen::Radar).then(|| view::radar(&cx));
            let settings = (screen == Screen::Settings).then(|| self.settings_view(&cx));
            (
                view::header(&cx),
                view::clock(&cx),
                view::weather(&cx),
                overview,
                radar,
                settings,
            )
        };

        if self.last.header.as_ref() != Some(&header) {
            ui.set_header(bridge::header(&header));
            self.last.header = Some(header);
        }
        if self.last.clock.as_ref() != Some(&clock_v) {
            ui.set_clock(bridge::clock(&clock_v));
            self.last.clock = Some(clock_v);
        }
        if self.last.weather.as_ref() != Some(&weather) {
            ui.set_weather(bridge::weather(&weather));
            self.last.weather = Some(weather);
        }
        if let Some((cal, quake, footer)) = overview {
            if self.last.calendar.as_ref() != Some(&cal) {
                ui.set_calendar(bridge::calendar(&cal));
                self.last.calendar = Some(cal);
            }
            if self.last.quake.as_ref() != Some(&quake) {
                ui.set_quake(bridge::quake(&quake));
                self.last.quake = Some(quake);
            }
            if self.last.footer.as_ref() != Some(&footer) {
                ui.set_footer(bridge::footer(&footer));
                self.last.footer = Some(footer);
            }
        }
        if let Some(r) = radar {
            let key = (r, self.radar.decoding);
            if self.last.radar.as_ref() != Some(&key) {
                ui.set_radar(bridge::radar(&key.0, key.1));
                self.last.radar = Some(key);
            }
        }
        if let Some(s) = settings
            && self.last.settings.as_ref() != Some(&s)
        {
            ui.set_settings(bridge::settings(&s));
            self.last.settings = Some(s);
        }
    }

    fn settings_view(&self, cx: &view::ViewContext) -> view::SettingsView {
        let rs = self.screens.settings();
        let mode_text = match (&self.snap.fixture_scenario, self.cfg.mode) {
            (Some(sc), _) => format!("サンプルデータ: {sc}"),
            (None, DataMode::Live) => "ライブデータ".into(),
            (None, DataMode::Fixture) => "サンプルデータ".into(),
        };
        let encoded = self
            .snap
            .radar
            .as_ref()
            .map(|r| (r.frames.len(), r.encoded_bytes()))
            .unwrap_or((0, 0));
        view::SettingsView {
            profile: self.cfg.display.profile,
            profile_detail: view::profile_detail(&self.profile_settings, &self.cfg),
            ical_text: match self.snap.status(SourceId::Calendar).map(|s| &s.origin) {
                Some(crate::status::Origin::Live(_)) => "設定済み（URL は非表示）".into(),
                Some(crate::status::Origin::Unconfigured(_)) => format!("未設定（{}）", config::ICAL_URL_ENV),
                Some(crate::status::Origin::Fixture) => "サンプルデータ".into(),
                _ => "無効".into(),
            },
            rotation_enabled: rs.enabled,
            rotation_paused: self.screens.is_paused(),
            overview_seconds: rs.overview.as_secs() as u32,
            radar_seconds: rs.radar.map(|d| d.as_secs() as u32).unwrap_or(0),
            radar_auto_seconds: self.screens.radar_dwell().as_secs_f32().round() as u32,
            radar_zoom: self.cfg.radar.map_zoom as u32,
            radar_zoom_text: {
                let (lat, _) = self.cfg.radar_center();
                let km = config::radar_view_width_km(lat, self.cfg.radar.map_zoom);
                // round to 10 km to keep the label short
                format!("{} · 約{}km", self.cfg.radar.map_zoom, (km / 10.0).round() as u32 * 10)
            },
            mode_text,
            config_path: self
                .paths
                .config_file
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "（なし — 組み込みの既定値）".into()),
            overrides_path: self.paths.overrides_file().display().to_string(),
            cache_dir: self.paths.cache_dir.display().to_string(),
            version: format!("v{}", env!("CARGO_PKG_VERSION")),
            radar_memory: format!(
                "保持 {}枚 (PNG {:.1} MB) · 表示用に展開 {}枚 ({:.1} MB)",
                encoded.0,
                encoded.1 as f64 / 1_048_576.0,
                self.radar.count,
                self.radar.bytes as f64 / 1_048_576.0
            ),
            save_message: self.save_message.clone(),
            sources: view::diag_rows(cx),
            logs: diag::recent().into_iter().take(10).collect(),
        }
    }

    // ───────────── radar images: decode on demand, release when hidden ─────────────

    fn radar_needed(&self) -> bool {
        self.screens.current() == Screen::Radar || (self.profile_settings.keep_radar_decoded && self.radar.count > 0)
    }

    fn clear_radar_images(&mut self) {
        if self.radar.count == 0 && !self.radar.decoding && self.radar.source.is_none() {
            return;
        }
        if let Some(ui) = self.window() {
            ui.set_radar_frames(ModelRc::default());
        }
        let generation = self.radar.generation + 1;
        self.radar = RadarImages {
            generation,
            ..Default::default()
        };
        tracing::debug!("radar images released");
    }

    fn sync_radar_images(&mut self) {
        if !self.radar_needed() {
            self.clear_radar_images();
            return;
        }
        let Some(data) = self.snap.radar.clone() else {
            self.clear_radar_images();
            return;
        };
        if self.radar.source.as_ref().is_some_and(|s| Arc::ptr_eq(s, &data)) {
            return; // already decoded or in flight
        }
        // The service already limits frames to the profile's budget.
        let want = data.frames.len();
        let frames: Vec<_> = data.frames[data.frames.len() - want..].to_vec();
        self.radar.generation += 1;
        self.radar.source = Some(data);
        self.radar.decoding = true;
        let generation = self.radar.generation;
        let decode = move || -> Vec<SharedPixelBuffer<Rgb8Pixel>> {
            frames
                .iter()
                .filter_map(
                    |f| match image::load_from_memory_with_format(&f.png, image::ImageFormat::Png) {
                        Ok(img) => {
                            let rgb = img.to_rgb8();
                            Some(SharedPixelBuffer::<Rgb8Pixel>::clone_from_slice(
                                rgb.as_raw(),
                                rgb.width(),
                                rgb.height(),
                            ))
                        }
                        Err(e) => {
                            tracing::warn!("radar frame decode failed: {e}");
                            None
                        }
                    },
                )
                .collect()
        };
        if self.sync_decode {
            let bufs = decode();
            self.radar_decoded(generation, bufs);
        } else {
            self.rt.spawn_blocking(move || {
                let bufs = decode();
                let _ = slint::invoke_from_event_loop(move || with_controller(|c| c.radar_decoded(generation, bufs)));
            });
        }
    }

    fn radar_decoded(&mut self, generation: u64, bufs: Vec<SharedPixelBuffer<Rgb8Pixel>>) {
        if generation != self.radar.generation || !self.radar_needed() {
            return; // screen changed or newer data arrived meanwhile
        }
        self.radar.bytes = bufs.iter().map(|b| b.width() as u64 * b.height() as u64 * 3).sum();
        self.radar.count = bufs.len();
        self.radar.decoding = false;
        let images: Vec<Image> = bufs.into_iter().map(Image::from_rgb8).collect();
        if let Some(ui) = self.window() {
            ui.set_radar_frames(ModelRc::new(VecModel::from(images)));
        }
        // The animation (re)starts now: count the radar dwell from here.
        if self.screens.current() == Screen::Radar {
            self.screens.restart_dwell(Instant::now());
        }
        self.refresh();
    }

    // ───────────── user actions ─────────────

    fn set_profile(&mut self, p: Profile) {
        if p == self.cfg.display.profile {
            return;
        }
        match self.cfg.profile_settings(p) {
            Ok(ps) => {
                self.cfg.display.profile = p;
                self.profile_settings = ps;
                self.overrides.profile = Some(p);
                let _ = self.profile_tx.send(ps);
                self.update_radar_dwell();
                if let Some(ui) = self.window() {
                    ui.global::<ui::Theme>().set_effects(ps.visual_effects);
                    ui.set_radar_frame_ms(ps.radar_frame_ms as i32);
                }
                // Re-decode with the new frame budget.
                self.clear_radar_images();
                self.sync_radar_images();
                self.last = LastViews::default();
                self.save_message = format!("プロファイルを {} に変更しました（未保存）", p.name());
                tracing::info!("profile → {}", p.name());
            }
            Err(e) => self.save_message = e.to_string(),
        }
    }

    fn update_rotation(&mut self, f: impl FnOnce(&mut config::RotationConfig)) {
        f(&mut self.cfg.rotation);
        self.cfg.rotation.overview_seconds = config::clamp_seconds(self.cfg.rotation.overview_seconds);
        self.cfg.rotation.radar_seconds = config::clamp_radar_seconds(self.cfg.rotation.radar_seconds);
        self.overrides.rotation_enabled = Some(self.cfg.rotation.enabled);
        self.overrides.overview_seconds = Some(self.cfg.rotation.overview_seconds);
        self.overrides.radar_seconds = Some(self.cfg.rotation.radar_seconds);
        self.screens.set_settings(rotation(&self.cfg));
        self.save_message = "変更しました（未保存）".into();
    }

    /// Put the current location into the settings-screen text fields.
    fn fill_location_fields(&mut self) {
        let Some(ui) = self.window() else { return };
        let l = &self.cfg.location;
        ui.set_edit_location_name(l.name.as_str().into());
        ui.set_edit_location_coords(format!("{}, {}", l.latitude, l.longitude).into());
        match (self.cfg.radar.latitude, self.cfg.radar.longitude) {
            (Some(lat), Some(lon)) => ui.set_edit_radar_coords(format!("{lat}, {lon}").into()),
            _ => ui.set_edit_radar_coords("".into()),
        }
        ui.set_edit_radar_name(self.cfg.radar.center_name.clone().unwrap_or_default().into());
        self.location_fields_filled = true;
    }

    /// Validate the location form and apply it immediately.
    fn apply_location(&mut self) {
        let Some(ui) = self.window() else { return };
        let name = ui.get_edit_location_name().trim().to_owned();
        let coords = ui.get_edit_location_coords().to_string();
        let radar_coords = ui.get_edit_radar_coords().trim().to_owned();
        let radar_name = ui.get_edit_radar_name().trim().to_owned();

        if name.is_empty() {
            self.save_message = "地点名を入力してください".into();
            return;
        }
        let Some((lat, lon)) = config::parse_coordinates(&coords) else {
            self.save_message = "座標は「緯度, 経度」の形式で入力してください（例: 35.6812, 139.7671）".into();
            return;
        };
        let radar = if radar_coords.is_empty() {
            None
        } else {
            match config::parse_coordinates(&radar_coords) {
                Some(c) => Some(c),
                None => {
                    self.save_message = "雨雲の中心は「緯度, 経度」の形式で入力するか、空欄にしてください".into();
                    return;
                }
            }
        };

        self.cfg.location.name = name.clone();
        self.cfg.location.latitude = lat;
        self.cfg.location.longitude = lon;
        self.cfg.radar.latitude = radar.map(|r| r.0);
        self.cfg.radar.longitude = radar.map(|r| r.1);
        self.cfg.radar.center_name = (!radar_name.is_empty()).then(|| radar_name.clone());
        self.overrides.location_name = Some(name.clone());
        self.overrides.location = Some(format!("{lat}, {lon}"));
        self.overrides.radar_center = Some(radar.map(|(a, b)| format!("{a}, {b}")).unwrap_or_default());
        self.overrides.radar_center_name = Some(radar_name);

        let place = services::Place::from_config(&self.cfg);
        let moved = *self.place_tx.borrow() != place;
        if moved {
            // Drop data for the old place (live sources only; fixtures are not re-fetched).
            for id in [SourceId::Weather, SourceId::Radar] {
                if matches!(
                    self.snap.status(id).map(|s| &s.origin),
                    Some(crate::status::Origin::Live(_))
                ) {
                    self.repo.reset(id);
                }
            }
            let _ = self.place_tx.send(place);
        }
        let (rlat, rlon) = self.cfg.radar_center();
        let mut msg = format!("地点を「{name}」（{lat:.4}, {lon:.4}）にしました");
        if moved {
            msg.push_str("。天気と雨雲を取得し直します");
        }
        if !config::in_japan(lat, lon) || !config::in_japan(rlat, rlon) {
            msg.push_str("。※日本の範囲外のため、雨雲・地図は表示されない可能性があります");
        }
        msg.push_str("（未保存）");
        self.save_message = msg;
        self.last = LastViews::default();
        tracing::info!("location → {name} ({lat}, {lon}); radar centre ({rlat}, {rlon})");
    }

    /// Change the radar map zoom; the radar is re-fetched for the new area.
    fn adjust_zoom(&mut self, delta: i32) {
        let range = config::RADAR_ZOOM_RANGE;
        let z = (self.cfg.radar.map_zoom as i32 + delta).clamp(*range.start() as i32, *range.end() as i32) as u8;
        if z == self.cfg.radar.map_zoom {
            return;
        }
        self.cfg.radar.map_zoom = z;
        self.overrides.radar_zoom = Some(z);
        let _ = self.zoom_tx.send(z);
        let (lat, _) = self.cfg.radar_center();
        self.save_message = format!(
            "地図の縮尺を {z}（横幅 約{:.0}km）に変更しました。雨雲を取得し直します（未保存）",
            config::radar_view_width_km(lat, z)
        );
        tracing::info!("radar zoom → {z}");
    }

    /// ± on the radar dwell; from "auto" it starts at the current automatic value.
    fn adjust_radar(&mut self, delta: &str) {
        let auto = self.screens.radar_dwell().as_secs_f32().round() as u32;
        self.update_rotation(|r| {
            if r.radar_seconds == 0 {
                r.radar_seconds = auto;
            }
            Self::adjust(&mut r.radar_seconds, delta);
        });
    }

    fn adjust(v: &mut u32, delta: &str) {
        let d: i64 = delta.parse().unwrap_or(0);
        *v = (*v as i64 + d).clamp(5, 600) as u32;
    }

    pub fn action(&mut self, action: &str) {
        let now = Instant::now();
        self.screens.touch(now);
        let in_settings = self.screens.current() == Screen::Settings;
        let changed = match action {
            "key:1" => self.screens.show(Screen::Overview, now),
            "key:2" => self.screens.show(Screen::Radar, now),
            "key:3" | "key:s" => {
                if in_settings {
                    self.screens.show(Screen::Overview, now)
                } else {
                    self.screens.show(Screen::Settings, now)
                }
            }
            "open-settings" => self.screens.show(Screen::Settings, now),
            "key:escape" | "close" => self.screens.show(Screen::Overview, now),
            "key:next" | "key:prev" | "tap" => self.screens.next(now),
            "key:space" => {
                let paused = self.screens.toggle_pause(now);
                tracing::info!("rotation {}", if paused { "paused" } else { "resumed" });
                None
            }
            "key:u" | "refresh" => {
                self.wake.notify_waiters();
                self.save_message = "データの再取得を要求しました".into();
                None
            }
            "key:p" if in_settings => {
                self.set_profile(self.cfg.display.profile.next());
                None
            }
            "key:a" | "rotation:toggle" => {
                self.update_rotation(|r| r.enabled = !r.enabled);
                None
            }
            "key:[" => {
                self.update_rotation(|r| Self::adjust(&mut r.overview_seconds, "-5"));
                None
            }
            "key:]" => {
                self.update_rotation(|r| Self::adjust(&mut r.overview_seconds, "5"));
                None
            }
            "key:-" => {
                self.adjust_radar("-5");
                None
            }
            "key:=" => {
                self.adjust_radar("5");
                None
            }
            "location:apply" => {
                self.apply_location();
                None
            }
            "key:," | "zoom:-1" => {
                self.adjust_zoom(-1);
                None
            }
            "key:." | "zoom:+1" => {
                self.adjust_zoom(1);
                None
            }
            "key:0" | "radar:auto" => {
                self.update_rotation(|r| r.radar_seconds = 0);
                None
            }
            "key:w" | "save" => {
                let path = self.paths.overrides_file();
                self.save_message = match config::save_overrides(&path, &self.overrides) {
                    Ok(()) => format!("保存しました: {}", path.display()),
                    Err(e) => {
                        tracing::warn!("saving overrides failed: {e}");
                        format!("保存できませんでした: {e}")
                    }
                };
                None
            }
            other => {
                if let Some(p) = other.strip_prefix("profile:").and_then(Profile::parse) {
                    self.set_profile(p);
                } else if let Some(d) = other.strip_prefix("overview:") {
                    let d = d.to_owned();
                    self.update_rotation(|r| Self::adjust(&mut r.overview_seconds, &d));
                } else if let Some(d) = other.strip_prefix("radar:") {
                    let d = d.to_owned();
                    self.adjust_radar(&d);
                }
                None
            }
        };
        if changed.is_some() {
            self.apply_screen();
        } else {
            self.refresh();
        }
    }
}

/// Everything needed to run (shared by the windowed app and screenshot mode).
struct Runtime {
    rt: tokio::runtime::Runtime,
    repo: Arc<Repository>,
    wake: Arc<Notify>,
    radar_progress: Arc<services::radar::Progress>,
    profile_tx: watch::Sender<ProfileSettings>,
    zoom_tx: watch::Sender<u8>,
    place_tx: watch::Sender<services::Place>,
}

fn start_backend(loaded: &Loaded, profile: ProfileSettings) -> anyhow::Result<Runtime> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(4)
        .thread_name("hd-worker")
        .enable_time()
        .enable_io()
        .build()
        .context("starting the async runtime")?;
    let repo = Repository::new(Snapshot::default());
    let wake = Arc::new(Notify::new());
    let (profile_tx, profile_rx) = watch::channel(profile);
    let radar_progress = Arc::new(services::radar::Progress::default());
    let (zoom_tx, zoom_rx) = watch::channel(loaded.cfg.radar.map_zoom);
    let (place_tx, place_rx) = watch::channel(services::Place::from_config(&loaded.cfg));
    let ctx = services::Context {
        repo: repo.clone(),
        http: {
            let _guard = rt.enter();
            Http::new()?
        },
        cfg: Arc::new(loaded.cfg.clone()),
        tz: loaded.tz,
        paths: loaded.paths.clone(),
        profile: profile_rx,
        wake: wake.clone(),
        radar_progress: radar_progress.clone(),
        radar_zoom: zoom_rx,
        place: place_rx,
    };
    services::spawn_all(rt.handle(), ctx);
    Ok(Runtime {
        rt,
        repo,
        wake,
        radar_progress,
        profile_tx,
        zoom_tx,
        place_tx,
    })
}

fn build_ui(loaded: &Loaded, profile: &ProfileSettings) -> anyhow::Result<MainWindow> {
    let ui = MainWindow::new().context("creating the window (is a display/GPU available?)")?;
    let font = if loaded.cfg.display.font_family.is_empty() {
        default_font()
    } else {
        &loaded.cfg.display.font_family
    };
    ui.set_font_family(font.into());
    ui.global::<ui::Theme>().set_effects(profile.visual_effects);
    ui.set_radar_frame_ms(profile.radar_frame_ms as i32);
    Ok(ui)
}

fn make_controller(
    ui: &MainWindow,
    loaded: Loaded,
    backend: &Runtime,
    profile: ProfileSettings,
    sync_decode: bool,
    initial: Option<Screen>,
) -> Rc<RefCell<Controller>> {
    let mut screens = ScreenManager::new(rotation(&loaded.cfg), Instant::now());
    if let Some(s) = initial {
        screens.show(s, Instant::now());
    }
    let c = Rc::new(RefCell::new(Controller {
        ui: ui.as_weak(),
        cfg: loaded.cfg,
        paths: loaded.paths,
        overrides: loaded.overrides,
        tz: loaded.tz,
        snap: Snapshot::default(),
        screens,
        profile_settings: profile,
        profile_tx: backend.profile_tx.clone(),
        wake: backend.wake.clone(),
        rt: backend.rt.handle().clone(),
        last: LastViews::default(),
        radar: RadarImages::default(),
        save_message: String::new(),
        sync_decode,
        location_fields_filled: false,
        last_minute: None,
        radar_progress: backend.radar_progress.clone(),
        last_progress: None,
        zoom_tx: backend.zoom_tx.clone(),
        place_tx: backend.place_tx.clone(),
        repo: backend.repo.clone(),
    }));
    CONTROLLER.with(|slot| *slot.borrow_mut() = Rc::downgrade(&c));
    c
}

/// Must run before any Slint backend or extra thread is created.
fn configure_scale(opts: &Options, cfg: &AppConfig) {
    if std::env::var_os("SLINT_SCALE_FACTOR").is_some() {
        return;
    }
    let scale = opts.scale.or(cfg.display.scale_factor).or_else(|| {
        // Desktop development: show the 1920×1080 layout 1:1 in physical
        // pixels so it fits any screen (half size on Retina).
        cfg!(feature = "desktop").then_some(1.0)
    });
    if let Some(s) = scale {
        // SAFETY: called from main() before any other thread is spawned.
        unsafe { std::env::set_var("SLINT_SCALE_FACTOR", format!("{s}")) };
    }
}

pub fn run(opts: Options) -> anyhow::Result<()> {
    if let Some(now) = opts.now {
        clock::set_fake_now(now);
    }
    let loaded = load(&opts)?;
    if opts.screenshot.is_some() || opts.screenshot_dir.is_some() {
        return screenshot::run(opts, loaded);
    }
    configure_scale(&opts, &loaded.cfg);
    let profile = loaded.cfg.profile_settings(loaded.cfg.display.profile)?;
    tracing::info!(
        "Home Display v{} — mode {:?}, profile {}, tz {}",
        env!("CARGO_PKG_VERSION"),
        loaded.cfg.mode,
        loaded.cfg.display.profile.name(),
        loaded.tz
    );

    let backend = start_backend(&loaded, profile)?;
    let ui = build_ui(&loaded, &profile)?;
    let controller = make_controller(&ui, loaded, &backend, profile, false, opts.screen);

    backend.repo.subscribe(|snap| {
        let _ = slint::invoke_from_event_loop(move || with_controller(|c| c.on_snapshot(snap)));
    });

    let c = controller.clone();
    ui.on_action(move |a| c.borrow_mut().action(a.as_str()));

    // The single UI timer: clock, rotation, freshness labels.
    schedule_tick();

    controller.borrow_mut().apply_screen();
    if opts.fullscreen {
        ui.window().set_fullscreen(true);
    } else if cfg!(feature = "desktop") && !cfg!(feature = "kms") {
        // macOS / Windows / Linux desktop preview: fixed 1920×1080 window.
        ui.set_fixed_size(true);
    }
    ui.run().context("running the UI event loop")?;

    drop(controller);
    backend.rt.shutdown_timeout(Duration::from_secs(2));
    Ok(())
}

/// One-shot timer re-armed for just after the next wall-clock second, so the
/// seconds display never drifts or skips (a repeated 1 s timer slowly would).
fn schedule_tick() {
    let into_second = clock::now_utc().timestamp_subsec_millis().min(999) as u64;
    let delay = Duration::from_millis(1000 - into_second + 3);
    slint::Timer::single_shot(delay, || {
        with_controller(|c| c.tick());
        schedule_tick();
    });
}

/// Headless rendering with Slint's software renderer, used for design review
/// and CI artifacts: `home-display --fixture default --screenshot-dir shots/`.
mod screenshot {
    use super::*;
    use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
    use slint::platform::{Platform, WindowAdapter};

    struct ShotPlatform {
        window: Rc<MinimalSoftwareWindow>,
        start: Instant,
        /// Virtual time added on top of real time (see `--screenshot-advance`).
        skew: Rc<std::cell::Cell<Duration>>,
    }

    impl Platform for ShotPlatform {
        fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
            Ok(self.window.clone())
        }
        fn duration_since_start(&self) -> Duration {
            self.start.elapsed() + self.skew.get()
        }
    }

    const W: u32 = 1920;
    const H: u32 = 1080;

    fn render(window: &MinimalSoftwareWindow, path: &std::path::Path) -> anyhow::Result<()> {
        slint::platform::update_timers_and_animations();
        let mut buf = vec![Rgb8Pixel::default(); (W * H) as usize];
        window.request_redraw();
        window.draw_if_needed(|r| {
            r.render(&mut buf, W as usize);
        });
        let raw: Vec<u8> = buf.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
        let img = image::RgbImage::from_raw(W, H, raw).context("screenshot buffer")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        img.save(path).with_context(|| format!("writing {}", path.display()))?;
        println!("wrote {}", path.display());
        Ok(())
    }

    pub fn run(opts: Options, loaded: Loaded) -> anyhow::Result<()> {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        window.set_size(slint::PhysicalSize::new(W, H));
        let skew = Rc::new(std::cell::Cell::new(Duration::ZERO));
        slint::platform::set_platform(Box::new(ShotPlatform {
            window: window.clone(),
            start: Instant::now(),
            skew: skew.clone(),
        }))
        .map_err(|e| anyhow::anyhow!("platform: {e:?}"))?;

        let profile = loaded.cfg.profile_settings(loaded.cfg.display.profile)?;
        let backend = start_backend(&loaded, profile)?;

        // Wait (bounded) for every source to settle: fixtures are immediate,
        // live sources may take a few seconds.
        let deadline = Instant::now() + opts.screenshot_wait.unwrap_or(Duration::from_secs(90));
        loop {
            let snap = backend.repo.snapshot();
            let pending = snap.status.values().any(|s| {
                s.last_attempt.is_none()
                    && s.last_success.is_none()
                    && matches!(
                        s.origin,
                        crate::status::Origin::Live(_) | crate::status::Origin::Fixture
                    )
            });
            let radar_pending = snap
                .status(SourceId::Radar)
                .is_some_and(|s| s.loading || (s.last_success.is_none() && s.last_error.is_none()))
                && snap
                    .status(SourceId::Radar)
                    .is_some_and(|s| s.origin != crate::status::Origin::Disabled);
            if !pending && !radar_pending {
                break;
            }
            if Instant::now() > deadline {
                tracing::warn!("screenshot: some sources still loading; rendering anyway");
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        let ui = build_ui(&loaded, &profile)?;
        let shots: Vec<(Screen, PathBuf)> = match (&opts.screenshot_dir, &opts.screenshot) {
            (Some(dir), _) => {
                let tag = loaded.cfg.fixture_scenario.clone();
                let tag = if loaded.cfg.mode == DataMode::Fixture {
                    tag
                } else {
                    "live".into()
                };
                let p = loaded.cfg.display.profile.name().to_lowercase();
                vec![
                    (Screen::Overview, dir.join(format!("{tag}-{p}-overview.png"))),
                    (Screen::Radar, dir.join(format!("{tag}-{p}-radar.png"))),
                    (Screen::Settings, dir.join(format!("{tag}-{p}-settings.png"))),
                ]
            }
            (None, Some(path)) => vec![(opts.screen.unwrap_or(Screen::Overview), path.clone())],
            (None, None) => unreachable!(),
        };
        let controller = make_controller(&ui, loaded, &backend, profile, true, None);
        ui.show()?;
        for (screen, path) in shots {
            {
                let mut c = controller.borrow_mut();
                c.poll_progress();
                c.on_snapshot(backend.repo.snapshot());
                if screen != c.screens.current() {
                    c.screens.show(screen, Instant::now());
                }
                c.apply_screen();
            }
            if let Some(adv) = opts.screenshot_advance {
                // Draw once so bindings (e.g. a Timer's `running`) are evaluated,
                // then step the virtual clock so Slint timers fire one by one.
                let mut scratch = vec![Rgb8Pixel::default(); (W * H) as usize];
                slint::platform::update_timers_and_animations();
                window.request_redraw();
                window.draw_if_needed(|r| {
                    r.render(&mut scratch, W as usize);
                });
                let step = Duration::from_millis(50);
                let mut done = Duration::ZERO;
                while done < adv {
                    skew.set(skew.get() + step);
                    done += step;
                    slint::platform::update_timers_and_animations();
                }
            }
            render(&window, &path)?;
        }
        ui.hide()?;
        drop(controller);
        backend.rt.shutdown_timeout(Duration::from_secs(1));
        Ok(())
    }
}
