//! Application configuration: one TOML file, edited by hand
//! (`sudo home-display config edit`), never written by the app.
//!
//! Resolution order for the config file:
//! `--config <path>` → `$XDG_CONFIG_HOME/home-display/config.toml`
//! (or `~/.config/home-display/config.toml`) → `/etc/home-display/config.toml`.
//! Without any file the built-in defaults run in fixture mode, so `cargo run`
//! shows a complete sample screen with no credentials.
//!
//! Precedence: command line > config file > built-in defaults. The only
//! environment variable the app reads is the secret calendar URL
//! (`HOME_DISPLAY_ICAL_URL`). Every key is listed, with its default, in the
//! commented template `packaging/config/config.toml` (checked by a test).

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub const ICAL_URL_ENV: &str = "HOME_DISPLAY_ICAL_URL";
/// systemd `LoadCredential=ical-url:...` exposes the secret under this name.
pub const ICAL_CREDENTIAL_NAME: &str = "ical-url";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Profile {
    Full,
    #[default]
    Balanced,
    Low,
}

impl Profile {
    pub const ALL: [Profile; 3] = [Profile::Full, Profile::Balanced, Profile::Low];

    pub fn name(self) -> &'static str {
        match self {
            Profile::Full => "Full",
            Profile::Balanced => "Balanced",
            Profile::Low => "Low",
        }
    }

    pub fn index(self) -> i32 {
        match self {
            Profile::Full => 0,
            Profile::Balanced => 1,
            Profile::Low => 2,
        }
    }

    pub fn next(self) -> Profile {
        match self {
            Profile::Full => Profile::Balanced,
            Profile::Balanced => Profile::Low,
            Profile::Low => Profile::Full,
        }
    }

    pub fn parse(s: &str) -> Option<Profile> {
        match s.to_ascii_lowercase().as_str() {
            "full" => Some(Profile::Full),
            "balanced" => Some(Profile::Balanced),
            "low" => Some(Profile::Low),
            _ => None,
        }
    }
}

/// Tunables that differ per display profile. Every field can be overridden in
/// `[profiles.<name>]`; unspecified fields keep the built-in default.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileSettings {
    /// Observed radar frames (including the latest) kept and animated.
    /// 1 = static latest image.
    pub radar_frames: u32,
    /// Minutes between animation frames (JMA: multiple of 5; RainViewer: 10).
    pub radar_step_minutes: u32,
    /// Forecast frames after "now" (JMA nowcast, up to 60 min ahead). 0 = none.
    pub radar_forecast_frames: u32,
    /// Delay between animation frames; 0 disables animation.
    pub radar_frame_ms: u32,
    /// How often the radar frame list is refreshed.
    pub radar_refresh_secs: u64,
    /// Composite resolution relative to the 1376×1012 map viewport.
    pub radar_scale: f32,
    /// Keep decoded radar frames while the radar screen is hidden.
    pub keep_radar_decoded: bool,
    /// Gradients and drop shadows.
    pub visual_effects: bool,
    /// Maximum timed events listed on the overview (in addition to the hero).
    pub max_events: u32,
    /// Hourly forecast cells shown on the weather card.
    pub hourly_cells: u32,
}

impl ProfileSettings {
    pub fn defaults(profile: Profile) -> Self {
        match profile {
            // Past 60 min + next 60 min every 5 min (25 frames); one loop ≈ 9 s.
            Profile::Full => ProfileSettings {
                radar_frames: 13,
                radar_step_minutes: 5,
                radar_forecast_frames: 12,
                radar_frame_ms: 300,
                radar_refresh_secs: 300,
                radar_scale: 1.0,
                keep_radar_decoded: true,
                visual_effects: true,
                max_events: 2,
                hourly_cells: 8,
            },
            // Past 60 min + next 60 min every 10 min (13 frames); one loop ≈ 9 s.
            Profile::Balanced => ProfileSettings {
                radar_frames: 7,
                radar_step_minutes: 10,
                radar_forecast_frames: 6,
                radar_frame_ms: 500,
                radar_refresh_secs: 600,
                radar_scale: 1.0,
                keep_radar_decoded: false,
                visual_effects: true,
                max_events: 2,
                hourly_cells: 8,
            },
            // Latest observation only, no animation.
            Profile::Low => ProfileSettings {
                radar_frames: 1,
                radar_step_minutes: 10,
                radar_forecast_frames: 0,
                radar_frame_ms: 0,
                radar_refresh_secs: 900,
                radar_scale: 1.0,
                keep_radar_decoded: false,
                visual_effects: false,
                max_events: 2,
                hourly_cells: 6,
            },
        }
    }

    /// Total animated frames (observed + forecast).
    pub fn radar_total_frames(&self) -> u32 {
        self.radar_frames.max(1) + self.radar_forecast_frames.min(12)
    }

    /// Composite size in pixels for the radar image.
    pub fn radar_size(&self) -> (u32, u32) {
        let s = self.radar_scale.clamp(0.25, 1.5);
        (
            ((RADAR_VIEW_W as f32) * s).round() as u32,
            ((RADAR_VIEW_H as f32) * s).round() as u32,
        )
    }

    /// Approximate RGBA bytes for one decoded frame.
    pub fn radar_frame_bytes(&self) -> u64 {
        let (w, h) = self.radar_size();
        w as u64 * h as u64 * 4
    }
}

/// Logical radar viewport (matches the map area of the radar screen at 1920×1080).
pub const RADAR_VIEW_W: u32 = 1376;
pub const RADAR_VIEW_H: u32 = 1012;

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProfileOverrides {
    pub full: Option<toml::Table>,
    pub balanced: Option<toml::Table>,
    pub low: Option<toml::Table>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DataMode {
    Live,
    #[default]
    Fixture,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Provider {
    /// The real provider for this source (気象庁, Open-Meteo …).
    Live,
    /// Bundled sample data, clearly labelled as such.
    Fixture,
    /// Do not fetch or show this source.
    Disabled,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppConfig {
    pub mode: DataMode,
    /// Fixture scenario when `mode = "fixture"` (see `home-display --list-fixtures`).
    pub fixture_scenario: String,
    pub location: LocationConfig,
    pub display: DisplayConfig,
    pub rotation: RotationConfig,
    pub calendar: CalendarConfig,
    pub weather: WeatherConfig,
    pub earthquake: EarthquakeConfig,
    pub radar: RadarConfig,
    pub holiday: HolidayConfig,
    pub profiles: ProfileOverrides,
    /// Indoor I2C sensors; `None` (no `[sensors]` table) = feature off.
    pub sensors: Option<SensorsConfig>,
}

impl Default for AppConfig {
    fn default() -> Self {
        AppConfig {
            mode: DataMode::Fixture,
            fixture_scenario: "default".into(),
            location: LocationConfig::default(),
            display: DisplayConfig::default(),
            rotation: RotationConfig::default(),
            calendar: CalendarConfig::default(),
            weather: WeatherConfig::default(),
            earthquake: EarthquakeConfig::default(),
            radar: RadarConfig::default(),
            holiday: HolidayConfig::default(),
            profiles: ProfileOverrides::default(),
            sensors: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct LocationConfig {
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
    /// IANA time zone used for the clock, calendar and forecasts.
    pub timezone: String,
}

impl Default for LocationConfig {
    fn default() -> Self {
        LocationConfig {
            name: "東京".into(),
            latitude: 35.6812,
            longitude: 139.7671,
            timezone: "Asia/Tokyo".into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct DisplayConfig {
    pub profile: Profile,
    /// Font family; empty = platform default (Hiragino Sans on macOS,
    /// Noto Sans CJK JP on Linux).
    pub font_family: String,
    /// Physical pixels per logical pixel (sets SLINT_SCALE_FACTOR when the
    /// environment doesn't). The UI is laid out for 1920×1080 logical px, so
    /// use 0.6667 for a 1280×720 output or 2.0 for 3840×2160.
    pub scale_factor: Option<f32>,
    /// Show seconds on the clock (one small redraw per second).
    pub show_seconds: bool,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        DisplayConfig {
            profile: Profile::default(),
            font_family: String::new(),
            scale_factor: None,
            show_seconds: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RotationConfig {
    pub enabled: bool,
    pub overview_seconds: u32,
    /// 0 = automatic: long enough for the radar animation (past hour →
    /// forecast hour) to play twice; 10 s for a static image.
    pub radar_seconds: u32,
    /// The settings screen returns to Overview after this much inactivity.
    pub settings_timeout_seconds: u32,
}

impl Default for RotationConfig {
    fn default() -> Self {
        RotationConfig {
            enabled: true,
            overview_seconds: 20,
            radar_seconds: 0,
            settings_timeout_seconds: 120,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct CalendarConfig {
    /// Discouraged: prefer `HOME_DISPLAY_ICAL_URL`, a systemd credential
    /// named `ical-url`, or `ical_url_file`. Never commit a real URL.
    pub ical_url: Option<String>,
    /// File containing only the secret iCal URL (mode 0600 recommended).
    pub ical_url_file: Option<PathBuf>,
    pub refresh_minutes: u64,
    pub stale_minutes: u64,
    /// Days ahead (including today) to expand recurring events for.
    pub horizon_days: u32,
}

impl Default for CalendarConfig {
    fn default() -> Self {
        CalendarConfig {
            ical_url: None,
            ical_url_file: None,
            refresh_minutes: 10,
            stale_minutes: 60,
            horizon_days: 3,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct WeatherConfig {
    pub provider: Provider,
    pub base_url: String,
    pub refresh_minutes: u64,
    pub stale_minutes: u64,
}

impl Default for WeatherConfig {
    fn default() -> Self {
        WeatherConfig {
            provider: Provider::Live,
            base_url: "https://api.open-meteo.com/v1/forecast".into(),
            refresh_minutes: 15,
            stale_minutes: 90,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum QuakeSource {
    Jma,
    P2pquake,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct EarthquakeConfig {
    pub provider: Provider,
    /// "jma" (気象庁 防災情報XML, official) or "p2pquake" (P2P地震情報 API).
    pub source: QuakeSource,
    /// Fall back to P2PQuake when the JMA feed cannot be fetched.
    pub fallback: bool,
    /// JMA PULL Atom feeds: high-frequency (≥ last 10 min, updated every
    /// minute) and long-term (several days, updated hourly).
    pub jma_feed_url: String,
    pub jma_long_feed_url: String,
    /// P2PQuake history endpoint.
    pub base_url: String,
    pub refresh_seconds: u64,
    pub stale_minutes: u64,
    /// Quakes older than this are shown muted, never highlighted.
    pub highlight_hours: u64,
}

impl Default for EarthquakeConfig {
    fn default() -> Self {
        EarthquakeConfig {
            provider: Provider::Live,
            source: QuakeSource::Jma,
            fallback: true,
            jma_feed_url: "https://www.data.jma.go.jp/developer/xml/feed/eqvol.xml".into(),
            jma_long_feed_url: "https://www.data.jma.go.jp/developer/xml/feed/eqvol_l.xml".into(),
            base_url: "https://api.p2pquake.net/v2/history".into(),
            refresh_seconds: 60,
            stale_minutes: 15,
            highlight_hours: 24,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RadarSource {
    Jma,
    Rainviewer,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RadarConfig {
    pub provider: Provider,
    /// Precipitation source: "jma" (気象庁 高解像度降水ナウキャスト, with
    /// 1-hour forecast) or "rainviewer" (past only, personal use).
    pub source: RadarSource,
    /// Fall back to RainViewer when the JMA data cannot be fetched.
    pub fallback: bool,
    /// JMA nowcast tile root (targetTimes_N1/N2.json and tiles live below it).
    pub jma_base_url: String,
    /// RainViewer Weather Maps index (fallback / `source = "rainviewer"`).
    pub index_url: String,
    /// Base-map tile template (`{z}/{x}/{y}`). GSI blank map (白地図) by default:
    /// coastlines and boundaries only, which keeps precipitation readable.
    pub basemap_url: String,
    /// Base-map zoom (5–11 on the settings screen).
    pub map_zoom: u8,
    /// RainViewer colour scheme id (2 = Universal Blue).
    pub color_scheme: u8,
    pub smooth: bool,
    pub stale_minutes: u64,
    /// Days before cached base-map tiles are re-downloaded.
    pub basemap_cache_days: u64,
    /// Map centre; defaults to `[location]` when unset.
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    /// Label shown on the radar screen when a separate centre is used.
    pub center_name: Option<String>,
    /// Refresh interval in seconds; overrides the profile's `radar_refresh_secs`.
    pub refresh_seconds: Option<u64>,
}

impl Default for RadarConfig {
    fn default() -> Self {
        RadarConfig {
            provider: Provider::Live,
            source: RadarSource::Jma,
            fallback: true,
            jma_base_url: "https://www.jma.go.jp/bosai/jmatile/data/nowc".into(),
            index_url: "https://api.rainviewer.com/public/weather-maps.json".into(),
            basemap_url: "https://cyberjapandata.gsi.go.jp/xyz/blank/{z}/{x}/{y}.png".into(),
            map_zoom: 9,
            color_scheme: 2,
            smooth: true,
            stale_minutes: 40,
            basemap_cache_days: 30,
            latitude: None,
            longitude: None,
            center_name: None,
            refresh_seconds: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct HolidayConfig {
    pub provider: Provider,
    /// Cabinet Office CSV (Shift_JIS).
    pub url: String,
    pub refresh_days: u64,
}

impl Default for HolidayConfig {
    fn default() -> Self {
        HolidayConfig {
            provider: Provider::Live,
            url: "https://www8.cao.go.jp/chosei/shukujitsu/syukujitsu.csv".into(),
            refresh_days: 7,
        }
    }
}

/// What a sensor measures. Units are fixed: °C, %RH, ppm, hPa.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Quantity {
    Temperature,
    Humidity,
    Co2,
    Pressure,
}

impl Quantity {
    pub const ALL: [Quantity; 4] = [
        Quantity::Temperature,
        Quantity::Humidity,
        Quantity::Co2,
        Quantity::Pressure,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Quantity::Temperature => "temperature",
            Quantity::Humidity => "humidity",
            Quantity::Co2 => "co2",
            Quantity::Pressure => "pressure",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Quantity::Temperature => "温度",
            Quantity::Humidity => "湿度",
            Quantity::Co2 => "CO2",
            Quantity::Pressure => "気圧",
        }
    }

    pub fn unit(self) -> &'static str {
        match self {
            Quantity::Temperature => "℃",
            Quantity::Humidity => "%",
            Quantity::Co2 => "ppm",
            Quantity::Pressure => "hPa",
        }
    }
}

/// Supported I2C sensor chips (one driver each in `src/sensors/`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SensorModel {
    Sht35,
    Scd41,
    Bmp581,
}

impl SensorModel {
    pub fn name(self) -> &'static str {
        match self {
            SensorModel::Sht35 => "SHT35",
            SensorModel::Scd41 => "SCD41",
            SensorModel::Bmp581 => "BMP581",
        }
    }

    pub fn quantities(self) -> &'static [Quantity] {
        match self {
            SensorModel::Sht35 => &[Quantity::Temperature, Quantity::Humidity],
            SensorModel::Scd41 => &[Quantity::Co2, Quantity::Temperature, Quantity::Humidity],
            SensorModel::Bmp581 => &[Quantity::Pressure, Quantity::Temperature],
        }
    }
}

/// `[sensors]` — read directly over I2C (docs/sensors.md).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SensorsConfig {
    /// `/dev/i2c-N` on Linux, or `"ftdi"` for an FT232H USB adapter
    /// (development builds with the `usb-i2c` feature).
    pub bus: String,
    pub interval_seconds: u64,
    /// Values older than this are shown dimmed.
    pub stale_seconds: u64,
    pub co2_caution_ppm: u32,
    pub co2_warning_ppm: u32,
    pub device: Vec<SensorDevice>,
    pub display: SensorDisplay,
}

impl Default for SensorsConfig {
    fn default() -> Self {
        SensorsConfig {
            bus: "/dev/i2c-1".into(),
            interval_seconds: 1,
            stale_seconds: 60,
            co2_caution_ppm: 1000,
            co2_warning_ppm: 1500,
            device: Vec::new(),
            display: SensorDisplay::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SensorDevice {
    /// Free name referenced from `[sensors.display]` / `pressure_from`.
    pub id: String,
    pub model: SensorModel,
    /// 7-bit I2C address (`0x44` in TOML).
    pub address: u8,
    /// SCD41: compensate CO2 with the pressure of this device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pressure_from: Option<String>,
    /// SCD41: automatic self-calibration; unset = keep the sensor's setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_calibration: Option<bool>,
}

/// Which device supplies each displayed value; unset = not shown.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SensorDisplay {
    pub temperature: Option<String>,
    pub humidity: Option<String>,
    pub co2: Option<String>,
    pub pressure: Option<String>,
}

impl SensorDisplay {
    pub fn source(&self, q: Quantity) -> Option<&str> {
        match q {
            Quantity::Temperature => self.temperature.as_deref(),
            Quantity::Humidity => self.humidity.as_deref(),
            Quantity::Co2 => self.co2.as_deref(),
            Quantity::Pressure => self.pressure.as_deref(),
        }
    }
}

/// Valid 7-bit addresses (0x00–0x07 and 0x78–0x7F are reserved).
pub const I2C_ADDRESS_RANGE: std::ops::RangeInclusive<u8> = 0x08..=0x77;

impl SensorsConfig {
    /// The sensor board this display is built with (docs/sensors.md):
    /// SHT35 at 0x44 (ADDR = GND), SCD41 at 0x62, BMP581 at 0x47 (SDO = 3V3)
    /// on `/dev/i2c-1`. Temperature and humidity come from the SHT35 (the
    /// SCD41 self-heats). Also used for the sample data.
    pub fn standard() -> SensorsConfig {
        let dev = |id: &str, model, address| SensorDevice {
            id: id.into(),
            model,
            address,
            pressure_from: None,
            auto_calibration: None,
        };
        SensorsConfig {
            device: vec![
                dev("room", SensorModel::Sht35, 0x44),
                SensorDevice {
                    pressure_from: Some("baro".into()),
                    ..dev("co2", SensorModel::Scd41, 0x62)
                },
                dev("baro", SensorModel::Bmp581, 0x47),
            ],
            display: SensorDisplay {
                temperature: Some("room".into()),
                humidity: Some("room".into()),
                co2: Some("co2".into()),
                pressure: Some("baro".into()),
            },
            ..SensorsConfig::default()
        }
    }

    pub fn find(&self, id: &str) -> Option<&SensorDevice> {
        self.device.iter().find(|d| d.id == id)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        let bad = |m: String| Err(ConfigError::Invalid(format!("sensors: {m}")));
        if self.bus.trim().is_empty() {
            return bad("bus が空です".into());
        }
        if !(1..=60).contains(&self.interval_seconds) {
            return bad("interval_seconds は 1〜60 で指定してください".into());
        }
        if self.co2_caution_ppm == 0 || self.co2_caution_ppm > self.co2_warning_ppm {
            return bad("co2_caution_ppm は 1 以上、co2_warning_ppm 以下にしてください".into());
        }
        if self.device.is_empty() {
            return bad("[[sensors.device]] が 1 つもありません".into());
        }
        for (i, d) in self.device.iter().enumerate() {
            let valid_id = !d.id.is_empty() && d.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
            if !valid_id {
                return bad(format!("id {:?} は英数字と - _ で指定してください", d.id));
            }
            if !I2C_ADDRESS_RANGE.contains(&d.address) {
                return bad(format!(
                    "{}: address 0x{:02X} は 0x08〜0x77 の範囲外です",
                    d.id, d.address
                ));
            }
            for other in &self.device[..i] {
                if other.id == d.id {
                    return bad(format!("id {:?} が重複しています", d.id));
                }
                if other.address == d.address {
                    return bad(format!(
                        "{} と {} のアドレス 0x{:02X} が重複しています",
                        other.id, d.id, d.address
                    ));
                }
            }
            if d.model != SensorModel::Scd41 && (d.pressure_from.is_some() || d.auto_calibration.is_some()) {
                return bad(format!(
                    "{}: pressure_from / auto_calibration は scd41 にだけ指定できます",
                    d.id
                ));
            }
            if let Some(src) = &d.pressure_from {
                match self.find(src) {
                    Some(p) if p.model.quantities().contains(&Quantity::Pressure) => {}
                    Some(_) => return bad(format!("{}: pressure_from {src:?} は気圧を測れません", d.id)),
                    None => return bad(format!("{}: pressure_from {src:?} という id はありません", d.id)),
                }
            }
        }
        for q in Quantity::ALL {
            if let Some(src) = self.display.source(q) {
                match self.find(src) {
                    Some(d) if d.model.quantities().contains(&q) => {}
                    Some(d) => {
                        return bad(format!(
                            "display.{} = {src:?}: {} は{}を測れません",
                            q.key(),
                            d.model.name(),
                            q.label()
                        ));
                    }
                    None => return bad(format!("display.{} = {src:?} という id はありません", q.key())),
                }
            }
        }
        Ok(())
    }
}

/// Approximate width in km of the radar map at `zoom` around `lat`.
pub fn radar_view_width_km(lat: f64, zoom: u8) -> f64 {
    let metres_per_px = 156_543.033_92 * lat.to_radians().cos() / 2f64.powi(zoom as i32);
    metres_per_px * RADAR_VIEW_W as f64 / 1000.0
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("設定ファイルを読めません ({path}): {source}")]
    Read { path: PathBuf, source: std::io::Error },
    #[error("設定ファイルの形式が不正です ({path}): {message}")]
    Parse { path: PathBuf, message: String },
    #[error("設定値が不正です: {0}")]
    Invalid(String),
}

impl AppConfig {
    pub fn from_toml_str(s: &str, path: &Path) -> Result<Self, ConfigError> {
        // Display (not just message()) keeps the line, column and source excerpt.
        let cfg: AppConfig = toml::from_str(s).map_err(|e| ConfigError::Parse {
            path: path.to_owned(),
            message: e.to_string().trim_end().to_owned(),
        })?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let s = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_owned(),
            source,
        })?;
        Self::from_toml_str(&s, path)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        self.timezone()?;
        if !(-90.0..=90.0).contains(&self.location.latitude) || !(-180.0..=180.0).contains(&self.location.longitude) {
            return Err(ConfigError::Invalid("location の緯度経度が範囲外です".into()));
        }
        if let Some(f) = self.display.scale_factor
            && !(0.25..=4.0).contains(&f)
        {
            return Err(ConfigError::Invalid(
                "display.scale_factor は 0.25〜4.0 で指定してください".into(),
            ));
        }
        if !(3..=14).contains(&self.radar.map_zoom) {
            return Err(ConfigError::Invalid(
                "radar.map_zoom は 3〜14 の範囲で指定してください".into(),
            ));
        }
        for p in Profile::ALL {
            self.profile_settings(p)?;
        }
        if let Some(sensors) = &self.sensors {
            sensors.validate()?;
        }
        Ok(())
    }

    pub fn timezone(&self) -> Result<chrono_tz::Tz, ConfigError> {
        self.location
            .timezone
            .parse()
            .map_err(|_| ConfigError::Invalid(format!("不明なタイムゾーン: {}", self.location.timezone)))
    }

    /// Built-in defaults for `profile` merged with `[profiles.<name>]`.
    pub fn profile_settings(&self, profile: Profile) -> Result<ProfileSettings, ConfigError> {
        let base = ProfileSettings::defaults(profile);
        let patch = match profile {
            Profile::Full => &self.profiles.full,
            Profile::Balanced => &self.profiles.balanced,
            Profile::Low => &self.profiles.low,
        };
        let Some(patch) = patch else { return Ok(base) };
        let mut table = toml::Table::try_from(base).expect("ProfileSettings serializes to a table");
        for (k, v) in patch {
            table.insert(k.clone(), v.clone());
        }
        let merged: ProfileSettings = table.try_into().map_err(|e: toml::de::Error| {
            ConfigError::Invalid(format!("profiles.{}: {}", profile.name().to_lowercase(), e.message()))
        })?;
        Ok(merged)
    }

    /// Radar map centre: `[radar]` latitude/longitude, else `[location]`.
    pub fn radar_center(&self) -> (f64, f64) {
        match (self.radar.latitude, self.radar.longitude) {
            (Some(lat), Some(lon)) => (lat, lon),
            _ => (self.location.latitude, self.location.longitude),
        }
    }

    /// Label for the radar screen.
    pub fn radar_label(&self) -> String {
        match (self.radar.latitude, &self.radar.center_name) {
            (Some(_), Some(n)) => n.clone(),
            (Some(_), None) => String::new(),
            (None, _) => self.location.name.clone(),
        }
    }

    /// Effective radar refresh interval for a profile.
    pub fn radar_refresh(&self, p: &ProfileSettings) -> Duration {
        Duration::from_secs(
            self.radar
                .refresh_seconds
                .unwrap_or(p.radar_refresh_secs)
                .max(MIN_RADAR_SECS),
        )
    }

    /// A refresh interval longer than the freshness limit would mark data as
    /// "old" between two normal updates; keep the limit at ≥ 3 intervals.
    pub fn normalize(&mut self) {
        let at_least = |stale: &mut u64, refresh_secs: u64| {
            *stale = (*stale).max((refresh_secs * 3).div_ceil(60));
        };
        at_least(&mut self.calendar.stale_minutes, self.calendar.refresh_minutes * 60);
        at_least(&mut self.weather.stale_minutes, self.weather.refresh_minutes * 60);
        at_least(&mut self.earthquake.stale_minutes, self.earthquake.refresh_seconds);
        if let Some(s) = self.radar.refresh_seconds {
            at_least(&mut self.radar.stale_minutes, s);
        }
        // SCD41 produces a value every 5 s whatever the interval.
        if let Some(s) = &mut self.sensors {
            s.stale_seconds = s.stale_seconds.max(s.interval_seconds * 3).max(15);
        }
    }

    /// Effective provider for a source, taking the global mode into account.
    pub fn provider_for(&self, p: Provider) -> Provider {
        match (self.mode, p) {
            (_, Provider::Disabled) => Provider::Disabled,
            (DataMode::Fixture, _) => Provider::Fixture,
            (DataMode::Live, p) => p,
        }
    }

    /// Resolve the secret iCal URL without ever logging it.
    /// Order: env → systemd credential → `ical_url_file` → `ical_url`.
    pub fn resolve_ical_url(&self) -> Result<Option<String>, ConfigError> {
        if let Ok(v) = std::env::var(ICAL_URL_ENV) {
            let v = v.trim().to_owned();
            if !v.is_empty() {
                return Ok(Some(v));
            }
        }
        if let Ok(dir) = std::env::var("CREDENTIALS_DIRECTORY") {
            let p = Path::new(&dir).join(ICAL_CREDENTIAL_NAME);
            if p.exists() {
                return read_secret_file(&p).map(Some);
            }
        }
        if let Some(p) = &self.calendar.ical_url_file {
            return read_secret_file(p).map(Some);
        }
        Ok(self.calendar.ical_url.clone().filter(|s| !s.trim().is_empty()))
    }
}

fn read_secret_file(p: &Path) -> Result<String, ConfigError> {
    let s = std::fs::read_to_string(p).map_err(|source| ConfigError::Read {
        path: p.to_owned(),
        source,
    })?;
    let s = s.trim().to_owned();
    if s.is_empty() {
        return Err(ConfigError::Invalid(format!("{} が空です", p.display())));
    }
    Ok(s)
}

/// Lower bound for the radar refresh (JMA nowcast updates every 5 min).
pub const MIN_RADAR_SECS: u64 = 2 * 60;

/// `"90"` / `"90s"` / `"10m"` / `"1h"` / `"7d"` / `"1h30m"` → seconds.
pub fn parse_duration_secs(s: &str) -> Option<u64> {
    let s = s.trim().to_ascii_lowercase();
    if s.is_empty() {
        return None;
    }
    if let Ok(n) = s.parse::<u64>() {
        return Some(n);
    }
    let mut total: u64 = 0;
    let mut num = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() {
            num.push(c);
            continue;
        }
        let n: u64 = num.parse().ok()?;
        num.clear();
        let unit = match c {
            's' => 1,
            'm' => 60,
            'h' => 3_600,
            'd' => 86_400,
            _ => return None,
        };
        total = total.checked_add(n.checked_mul(unit)?)?;
    }
    num.is_empty().then_some(total)
}

/// Parse "35.6812, 139.7671" (as copied from Google Maps), "35.6812 139.7671"
/// or the same with full-width digits / commas. `None` when out of range.
pub fn parse_coordinates(s: &str) -> Option<(f64, f64)> {
    let ascii: String = s
        .trim()
        .chars()
        .map(|c| match c {
            '０'..='９' => char::from(b'0' + (c as u32 - '０' as u32) as u8),
            '，' | '、' => ',',
            '．' => '.',
            'ー' | '－' | '−' => '-',
            '　' => ' ',
            c => c,
        })
        .collect();
    let parts: Vec<&str> = ascii
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|p| !p.is_empty())
        .collect();
    let [a, b] = parts.as_slice() else { return None };
    let lat: f64 = a.parse().ok()?;
    let lon: f64 = b.parse().ok()?;
    ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)).then_some((lat, lon))
}

/// Rough bounding box of the JMA radar / GSI map coverage.
pub fn in_japan(lat: f64, lon: f64) -> bool {
    (20.0..=46.5).contains(&lat) && (122.0..=154.5).contains(&lon)
}

/// `90` → "1m30s", `600` → "10m", `86400` → "1d".
pub fn fmt_secs(s: u64) -> String {
    let (d, h, m, sec) = (s / 86_400, s % 86_400 / 3_600, s % 3_600 / 60, s % 60);
    let mut out = String::new();
    for (v, u) in [(d, "d"), (h, "h"), (m, "m"), (sec, "s")] {
        if v > 0 {
            out.push_str(&format!("{v}{u}"));
        }
    }
    if out.is_empty() { "0s".into() } else { out }
}

pub fn clamp_seconds(v: u32) -> u32 {
    v.clamp(5, 600)
}

/// Like [`clamp_seconds`] but keeps 0 (= automatic).
pub fn clamp_radar_seconds(v: u32) -> u32 {
    if v == 0 { 0 } else { clamp_seconds(v) }
}

impl RotationConfig {
    pub fn overview(&self) -> Duration {
        Duration::from_secs(clamp_seconds(self.overview_seconds) as u64)
    }
    /// `None` = automatic (see `radar_seconds`).
    pub fn radar(&self) -> Option<Duration> {
        (self.radar_seconds != 0).then(|| Duration::from_secs(clamp_seconds(self.radar_seconds) as u64))
    }
}

/// Filesystem locations used at runtime.
#[derive(Debug, Clone)]
pub struct Paths {
    pub config_file: Option<PathBuf>,
    pub cache_dir: PathBuf,
    pub state_dir: PathBuf,
}

impl Paths {
    /// Saved settings-screen values of v0.0.5 and older: no longer read
    /// (the Pi installer moves them into config.toml).
    pub fn legacy_overrides_file(&self) -> PathBuf {
        self.state_dir.join("ui-overrides.toml")
    }

    /// Respects systemd's `CACHE_DIRECTORY` / `STATE_DIRECTORY`, then XDG.
    pub fn discover(explicit_config: Option<PathBuf>) -> Paths {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let xdg = |var: &str, fallback: &str| -> PathBuf {
            std::env::var_os(var)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .or_else(|| home.as_ref().map(|h| h.join(fallback)))
                .unwrap_or_else(std::env::temp_dir)
        };
        let config_file = explicit_config
            .or_else(|| {
                let user = xdg("XDG_CONFIG_HOME", ".config").join("home-display/config.toml");
                user.exists().then_some(user)
            })
            .or_else(|| {
                let sys = PathBuf::from("/etc/home-display/config.toml");
                sys.exists().then_some(sys)
            });
        let first_dir = |var: &str| {
            std::env::var(var)
                .ok()
                .and_then(|v| v.split(':').next().map(PathBuf::from))
                .filter(|p| p.is_absolute())
        };
        let cache_dir =
            first_dir("CACHE_DIRECTORY").unwrap_or_else(|| xdg("XDG_CACHE_HOME", ".cache").join("home-display"));
        let state_dir =
            first_dir("STATE_DIRECTORY").unwrap_or_else(|| xdg("XDG_STATE_HOME", ".local/state").join("home-display"));
        Paths {
            config_file,
            cache_dir,
            state_dir,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_validate_and_use_fixture_mode() {
        let c = AppConfig::default();
        c.validate().unwrap();
        assert_eq!(c.mode, DataMode::Fixture);
        assert_eq!(c.rotation.overview_seconds, 20);
        assert_eq!(c.rotation.radar_seconds, 0, "radar dwell is automatic by default");
        assert_eq!(c.rotation.radar(), None);
    }

    #[test]
    fn example_config_parses() {
        let s = include_str!("../packaging/config/config.toml");
        let c = AppConfig::from_toml_str(s, Path::new("config.toml")).unwrap();
        assert_eq!(c.mode, DataMode::Live);
        assert!(c.calendar.ical_url.is_none(), "example config must not contain a URL");
    }

    #[test]
    fn profile_patch_merges_over_defaults() {
        let c = AppConfig::from_toml_str("[profiles.low]\nradar_frames = 2\n", Path::new("x.toml")).unwrap();
        let low = c.profile_settings(Profile::Low).unwrap();
        assert_eq!(low.radar_frames, 2);
        assert_eq!(
            low.radar_frame_ms,
            ProfileSettings::defaults(Profile::Low).radar_frame_ms
        );
    }

    #[test]
    fn bad_profile_patch_is_rejected() {
        let err = AppConfig::from_toml_str("[profiles.full]\nradar_frames = \"many\"\n", Path::new("x.toml"));
        assert!(err.is_err());
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(AppConfig::from_toml_str("modee = \"live\"\n", Path::new("x.toml")).is_err());
    }

    #[test]
    fn invalid_timezone_is_rejected() {
        assert!(AppConfig::from_toml_str("[location]\ntimezone = \"Mars/Base\"\n", Path::new("x.toml")).is_err());
    }

    #[test]
    fn rotation_seconds_are_clamped() {
        let c = AppConfig::from_toml_str(
            "[rotation]\noverview_seconds = 1\nradar_seconds = 9999\n",
            Path::new("x.toml"),
        )
        .unwrap();
        assert_eq!(c.rotation.overview(), Duration::from_secs(5));
        assert_eq!(c.rotation.radar(), Some(Duration::from_secs(600)));
        assert_eq!(clamp_radar_seconds(0), 0);
        assert_eq!(clamp_radar_seconds(3), 5);
    }

    // ───── the commented config.toml template ─────

    const TEMPLATE: &str = include_str!("../packaging/config/config.toml");

    /// "# key = value" / "# [table]" lines are settings; other comments are prose.
    fn setting_line(line: &str) -> Option<&str> {
        let rest = line.strip_prefix("# ")?;
        let is_key = rest.split_once(" = ").is_some_and(|(k, _)| {
            !k.is_empty()
                && k.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        });
        (is_key || rest.starts_with('[')).then_some(rest)
    }

    /// The template with every setting uncommented.
    fn template_uncommented() -> String {
        TEMPLATE
            .lines()
            .map(|l| setting_line(l).unwrap_or(l))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// "a.b.c" for every leaf; arrays of tables use "a[]".
    fn key_paths(prefix: &str, t: &toml::Table, out: &mut std::collections::BTreeSet<String>) {
        for (k, v) in t {
            let path = if prefix.is_empty() {
                k.clone()
            } else {
                format!("{prefix}.{k}")
            };
            match v {
                toml::Value::Table(sub) => key_paths(&path, sub, out),
                toml::Value::Array(items) if items.iter().all(|i| i.is_table()) && !items.is_empty() => {
                    for i in items {
                        key_paths(&format!("{path}[]"), i.as_table().unwrap(), out);
                    }
                }
                _ => {
                    out.insert(path);
                }
            }
        }
    }

    /// Every optional field filled in, so that serialising lists every key.
    fn fully_populated() -> AppConfig {
        let mut c = AppConfig::default();
        c.display.scale_factor = Some(1.0);
        c.calendar.ical_url = Some("https://example.invalid/basic.ics".into());
        c.calendar.ical_url_file = Some("/etc/home-display/ical-url".into());
        c.radar.latitude = Some(34.6937);
        c.radar.longitude = Some(135.5023);
        c.radar.center_name = Some("大阪".into());
        c.radar.refresh_seconds = Some(600);
        let table = |p| toml::Table::try_from(ProfileSettings::defaults(p)).unwrap();
        c.profiles.full = Some(table(Profile::Full));
        c.profiles.balanced = Some(table(Profile::Balanced));
        c.profiles.low = Some(table(Profile::Low));
        let mut sensors = SensorsConfig::standard();
        sensors.device[1].auto_calibration = Some(true);
        c.sensors = Some(sensors);
        c
    }

    #[test]
    fn template_lists_every_setting() {
        let text = template_uncommented();
        // Valid as a whole (unknown keys would be rejected by deny_unknown_fields).
        AppConfig::from_toml_str(&text, Path::new("config.toml")).unwrap();
        let mut in_template = std::collections::BTreeSet::new();
        key_paths("", &text.parse::<toml::Table>().unwrap(), &mut in_template);
        let mut all = std::collections::BTreeSet::new();
        key_paths("", &toml::Table::try_from(fully_populated()).unwrap(), &mut all);
        let missing: Vec<_> = all.difference(&in_template).collect();
        assert!(
            missing.is_empty(),
            "add these to packaging/config/config.toml: {missing:?}"
        );
        // The shipped file itself (only mode and [location] active) is valid too.
        AppConfig::from_toml_str(TEMPLATE, Path::new("config.toml")).unwrap();
    }

    #[test]
    fn template_shows_the_defaults() {
        let mut defaults = toml::Table::try_from(AppConfig::default()).unwrap();
        let profiles = defaults.get_mut("profiles").unwrap().as_table_mut().unwrap();
        for p in Profile::ALL {
            let t = toml::Table::try_from(ProfileSettings::defaults(p)).unwrap();
            profiles.insert(p.name().to_lowercase(), toml::Value::Table(t));
        }
        let mut table: Vec<String> = Vec::new();
        let mut checked = 0;
        for line in TEMPLATE.lines() {
            // Table headers count whether active ("[display]") or commented.
            let Some(rest) = setting_line(line).or_else(|| line.starts_with('[').then_some(line)) else {
                continue;
            };
            if let Some(name) = rest.strip_prefix('[') {
                table = name
                    .trim_matches(|c| c == '[' || c == ']')
                    .split('.')
                    .map(String::from)
                    .collect();
                continue;
            }
            // Examples for optional values and the sensor board are not defaults.
            if table.first().is_some_and(|t| t == "sensors") {
                continue;
            }
            let (key, value) = rest.split_once(" = ").unwrap();
            let mut node = &defaults;
            for t in &table {
                node = node[t.as_str()].as_table().unwrap();
            }
            let Some(default) = node.get(key) else { continue };
            let shown: toml::Table = format!("v = {value}").parse().unwrap();
            assert_eq!(
                &shown["v"],
                default,
                "[{}] {key}: template shows {value}",
                table.join(".")
            );
            checked += 1;
        }
        assert!(checked > 60, "only {checked} defaults compared");
    }

    #[test]
    fn coordinates_parse() {
        assert_eq!(parse_coordinates("35.6812, 139.7671"), Some((35.6812, 139.7671)));
        assert_eq!(parse_coordinates("35.6812 139.7671"), Some((35.6812, 139.7671)));
        assert_eq!(
            parse_coordinates("３５．６８１２，１３９．７６７１"),
            Some((35.6812, 139.7671))
        );
        assert_eq!(parse_coordinates("-33.8, 151.2"), Some((-33.8, 151.2)));
        assert_eq!(parse_coordinates("95, 10"), None);
        assert_eq!(parse_coordinates("35.6"), None);
        assert_eq!(parse_coordinates("Tokyo"), None);
        assert!(in_japan(35.68, 139.77) && in_japan(26.2, 127.7));
        assert!(!in_japan(51.5, -0.1));
    }

    #[test]
    fn normalize_widens_stale_limits() {
        let mut c = AppConfig::default();
        c.calendar.refresh_minutes = 120;
        c.normalize();
        assert_eq!(c.calendar.stale_minutes, 360, "3 × 2 h");
        assert_eq!(c.weather.stale_minutes, 90, "already ≥ 3 intervals");
    }

    #[test]
    fn fixture_mode_forces_fixture_providers() {
        let c = AppConfig::default();
        assert_eq!(c.provider_for(Provider::Live), Provider::Fixture);
        assert_eq!(c.provider_for(Provider::Disabled), Provider::Disabled);
    }

    #[test]
    fn profile_radar_budgets() {
        let total = |p: Profile| {
            let s = ProfileSettings::defaults(p);
            s.radar_frame_bytes() * s.radar_total_frames() as u64
        };
        // Every profile renders at full resolution; frame count sets the budget.
        assert_eq!(ProfileSettings::defaults(Profile::Full).radar_total_frames(), 25);
        assert_eq!(ProfileSettings::defaults(Profile::Balanced).radar_total_frames(), 13);
        assert_eq!(ProfileSettings::defaults(Profile::Low).radar_total_frames(), 1);
        assert!(total(Profile::Low) * 10 < total(Profile::Balanced));
        assert!(total(Profile::Balanced) < total(Profile::Full));
    }

    #[test]
    fn durations_parse() {
        assert_eq!(parse_duration_secs("90"), Some(90));
        assert_eq!(parse_duration_secs("90s"), Some(90));
        assert_eq!(parse_duration_secs("10m"), Some(600));
        assert_eq!(parse_duration_secs("1h30m"), Some(5_400));
        assert_eq!(parse_duration_secs("7d"), Some(604_800));
        assert_eq!(parse_duration_secs("10 min"), None);
        assert_eq!(parse_duration_secs("m"), None);
        assert_eq!(parse_duration_secs("5x"), None);
        assert_eq!(fmt_secs(90), "1m30s");
        assert_eq!(fmt_secs(86_400), "1d");
    }

    #[test]
    fn radar_zoom_range_and_width() {
        assert!(AppConfig::from_toml_str("[radar]\nmap_zoom = 7\n", Path::new("x.toml")).is_ok());
        assert!(AppConfig::from_toml_str("[radar]\nmap_zoom = 20\n", Path::new("x.toml")).is_err());
        // Tokyo at z9 ≈ 340 km wide; each zoom step halves it.
        let w9 = radar_view_width_km(35.68, 9);
        assert!((330.0..350.0).contains(&w9), "{w9}");
        assert!((radar_view_width_km(35.68, 10) * 2.0 - w9).abs() < 0.01);
    }

    const SENSORS: &str = r#"
[sensors]
bus = "/dev/i2c-1"
interval_seconds = 1

[[sensors.device]]
id = "room"
model = "sht35"
address = 0x44

[[sensors.device]]
id = "co2"
model = "scd41"
address = 0x62
pressure_from = "baro"
auto_calibration = true

[[sensors.device]]
id = "baro"
model = "bmp581"
address = 0x47

[sensors.display]
temperature = "room"
humidity = "room"
co2 = "co2"
pressure = "baro"
"#;

    #[test]
    fn sensors_parse() {
        let mut c = AppConfig::from_toml_str(SENSORS, Path::new("x.toml")).unwrap();
        c.normalize();
        let s = c.sensors.as_ref().unwrap();
        assert_eq!(s.device.len(), 3);
        assert_eq!(s.device[0].address, 0x44);
        assert_eq!(s.device[1].model, SensorModel::Scd41);
        assert_eq!(s.device[1].pressure_from.as_deref(), Some("baro"));
        assert_eq!(s.display.source(Quantity::Co2), Some("co2"));
        assert_eq!(s.stale_seconds, 60);
        assert!(AppConfig::default().sensors.is_none(), "off unless configured");
    }

    #[test]
    fn sensors_stale_limit_follows_interval() {
        let mut c = AppConfig::from_toml_str(
            &SENSORS.replace("interval_seconds = 1", "interval_seconds = 30\nstale_seconds = 10"),
            Path::new("x.toml"),
        )
        .unwrap();
        c.normalize();
        assert_eq!(c.sensors.unwrap().stale_seconds, 90);
    }

    #[test]
    fn sensors_errors_are_rejected() {
        let bad = |from: &str, to: &str| {
            let toml = SENSORS.replacen(from, to, 1);
            assert_ne!(toml, SENSORS, "pattern {from:?} not found");
            AppConfig::from_toml_str(&toml, Path::new("x.toml"))
                .unwrap_err()
                .to_string()
        };
        assert!(bad("model = \"sht35\"", "model = \"dht22\"").contains("dht22"));
        assert!(bad("address = 0x44", "address = 0x200").contains("形式"));
        assert!(bad("address = 0x44", "address = 0x03").contains("範囲外"));
        assert!(bad("address = 0x44", "address = 0x62").contains("重複"));
        assert!(bad("id = \"co2\"", "id = \"room\"").contains("重複"));
        assert!(bad("id = \"room\"", "id = \"my room\"").contains("英数字"));
        assert!(bad("co2 = \"co2\"", "co2 = \"room\"").contains("CO2を測れません"));
        assert!(bad("pressure = \"baro\"", "pressure = \"nope\"").contains("ありません"));
        assert!(bad("pressure_from = \"baro\"", "pressure_from = \"room\"").contains("気圧を測れません"));
        assert!(bad("interval_seconds = 1", "interval_seconds = 0").contains("interval_seconds"));
        assert!(bad("address = 0x47", "address = 0x47\nauto_calibration = false").contains("scd41"));
        let empty = AppConfig::from_toml_str("[sensors]\n", Path::new("x.toml")).unwrap_err();
        assert!(empty.to_string().contains("1 つもありません"));
    }
}
