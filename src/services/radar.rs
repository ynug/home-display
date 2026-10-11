//! Rain radar: precipitation tiles composited over GSI base-map tiles
//! (地理院タイル), centred on the configured location.
//!
//! * 気象庁 高解像度降水ナウキャスト (default) — observations every 5 min for
//!   the past 3 h plus a 60-min forecast (250 m mesh up to 30 min ahead, 1 km
//!   beyond). These are the tiles behind the JMA website's 雨雲の動き page;
//!   they are not a documented API, so URLs may change without notice and the
//!   RainViewer fallback is used if they fail. Reuse follows the JMA website
//!   terms (出典表記・加工の明記).
//!   <https://www.jma.go.jp/jma/kishou/know/kurashi/highres_nowcast.html>
//! * RainViewer Weather Maps API (fallback) — free for personal/educational
//!   use, past ~2 h in 10-minute steps, max zoom 7, attribution required.
//!   <https://www.rainviewer.com/api/weather-maps-api.html>
//! * GSI tiles (白地図 by default) — real-time use needs attribution only; for
//!   the pale/std maps zoom ≥ 9 avoids the extra GEBCO / JCG notices of zoom 5–8.
//!   <https://maps.gsi.go.jp/development/ichiran.html>
//!
//! Pipeline (all off the UI thread): fetch index → fetch tiles (base map is
//! disk-cached) → composite at the native 1376×1012 viewport → resize to the
//! profile's resolution → PNG-encode. Only encoded frames are kept here;
//! RGBA decoding happens on demand for the radar screen.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, NaiveDateTime, Utc};
use image::{Rgb, RgbImage, RgbaImage};
use serde::Deserialize;

use super::Context;
use crate::clock;
use crate::config::{ProfileSettings, RADAR_VIEW_H, RADAR_VIEW_W, RadarSource};
use crate::model::{RadarData, RadarFrame};
use crate::repository::Payload;
use crate::status::{ErrorKind, SourceError, SourceId};

const BASE_TILE: u32 = 256;
const RAINVIEWER_TILE: u32 = 512;
const RAINVIEWER_MAX_ZOOM: u8 = 7;
const JMA_TILE: u32 = 256;
const JMA_MAX_ZOOM: u8 = 10;
const JMA_MIN_ZOOM: u8 = 4;

/// JMA nowcast tiles only carry data at even zoom levels (4, 6, 8, 10); odd
/// levels return an empty 334-byte image. Returns (fine, coarse) zooms for a
/// view: `fine` ≥ the view zoom (downsampled → full 250 m detail), `coarse` ≤
/// the view zoom (used for the 1 km forecast beyond 30 min).
pub fn jma_zooms(view_zoom: u8) -> (u8, u8) {
    let even_up = (view_zoom + view_zoom % 2).clamp(JMA_MIN_ZOOM, JMA_MAX_ZOOM);
    let even_down = (view_zoom - view_zoom % 2).clamp(JMA_MIN_ZOOM, JMA_MAX_ZOOM);
    (even_up, even_down)
}

pub const JMA_CREDIT: &str = "雨雲: 気象庁「高解像度降水ナウキャスト」を加工";
pub const RAINVIEWER_CREDIT: &str = "降水: RainViewer";

/// Attribution line: precipitation credit + base-map credit.
pub fn attribution(precip: &str, map_zoom: u8, basemap_url: &str) -> String {
    let map = match basemap_kind(basemap_url).as_str() {
        "blank" => "地理院タイル（白地図）を加工",
        "pale" => "地理院タイル（淡色地図）を加工",
        "std" => "地理院タイル（標準地図）を加工",
        _ => "地図タイルを加工",
    };
    let mut s = format!("{precip}  ·  地図: {map}");
    if map_zoom <= 8 && !basemap_url.contains("/blank/") {
        s.push_str("  ·  海域部は海上保安庁許可第292502号（水路業務法第25条に基づく類似刊行物）・GEBCO");
    }
    s
}

// ───────────────────────────── geometry ─────────────────────────────

/// Web-Mercator world pixel coordinates at `zoom` for a `tile`-pixel grid.
pub fn world_px(lat: f64, lon: f64, zoom: u8, tile: u32) -> (f64, f64) {
    let size = tile as f64 * 2f64.powi(zoom as i32);
    let lat = lat.clamp(-85.05112878, 85.05112878).to_radians();
    let x = (lon + 180.0) / 360.0 * size;
    let y = (1.0 - (lat.tan() + 1.0 / lat.cos()).ln() / std::f64::consts::PI) / 2.0 * size;
    (x, y)
}

/// The native (unscaled) viewport in base-map world pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    pub zoom: u8,
    /// World pixel of the top-left corner at `zoom` (256-px tiles).
    pub left: f64,
    pub top: f64,
    pub width: u32,
    pub height: u32,
}

impl Viewport {
    pub fn centered(lat: f64, lon: f64, zoom: u8) -> Viewport {
        let (cx, cy) = world_px(lat, lon, zoom, BASE_TILE);
        Viewport {
            zoom,
            left: (cx - RADAR_VIEW_W as f64 / 2.0).floor(),
            top: (cy - RADAR_VIEW_H as f64 / 2.0).floor(),
            width: RADAR_VIEW_W,
            height: RADAR_VIEW_H,
        }
    }

    /// Tiles (x, y) covering the viewport for a grid of `tile`-px tiles at
    /// `zoom`, where one viewport pixel equals `scale` grid pixels.
    pub fn tiles(&self, tile: u32, scale: f64) -> Vec<(u32, u32)> {
        let x0 = (self.left * scale / tile as f64).floor() as i64;
        let y0 = (self.top * scale / tile as f64).floor() as i64;
        let x1 = ((self.left + self.width as f64) * scale / tile as f64).floor() as i64;
        let y1 = ((self.top + self.height as f64) * scale / tile as f64).floor() as i64;
        let mut v = Vec::new();
        for y in y0..=y1 {
            for x in x0..=x1 {
                if x >= 0 && y >= 0 {
                    v.push((x as u32, y as u32));
                }
            }
        }
        v
    }

    /// Pixels of a `tile`-px grid at `zoom` per viewport pixel.
    pub fn grid_scale(&self, zoom: u8, tile: u32) -> f64 {
        let grid_world = tile as f64 * 2f64.powi(zoom as i32);
        let base_world = BASE_TILE as f64 * 2f64.powi(self.zoom as i32);
        grid_world / base_world
    }
}

/// A sparse set of decoded tiles addressed by world pixel.
struct Mosaic {
    tile: u32,
    tiles: HashMap<(u32, u32), RgbaImage>,
}

impl Mosaic {
    fn pixel(&self, wx: i64, wy: i64) -> Option<[u8; 4]> {
        if wx < 0 || wy < 0 {
            return None;
        }
        let t = self.tile as i64;
        let img = self.tiles.get(&((wx / t) as u32, (wy / t) as u32))?;
        let (px, py) = ((wx % t) as u32, (wy % t) as u32);
        if px < img.width() && py < img.height() {
            Some(img.get_pixel(px, py).0)
        } else {
            None
        }
    }

    /// Bilinear sample with missing pixels treated as transparent.
    fn sample(&self, x: f64, y: f64) -> [f32; 4] {
        let (x, y) = (x - 0.5, y - 0.5);
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = ((x - x0) as f32, (y - y0) as f32);
        let mut acc = [0f32; 4];
        for (dx, dy, w) in [
            (0, 0, (1.0 - fx) * (1.0 - fy)),
            (1, 0, fx * (1.0 - fy)),
            (0, 1, (1.0 - fx) * fy),
            (1, 1, fx * fy),
        ] {
            if let Some(p) = self.pixel(x0 as i64 + dx, y0 as i64 + dy) {
                let a = p[3] as f32 / 255.0;
                // premultiply so transparent neighbours don't darken edges
                acc[0] += p[0] as f32 * a * w;
                acc[1] += p[1] as f32 * a * w;
                acc[2] += p[2] as f32 * a * w;
                acc[3] += a * w;
            }
        }
        if acc[3] > 0.0 {
            [acc[0] / acc[3], acc[1] / acc[3], acc[2] / acc[3], acc[3]]
        } else {
            [0.0; 4]
        }
    }
}

// ───────────────────────────── styling ─────────────────────────────

const WATER: [f32; 3] = [11.0, 18.0, 27.0];
const LAND_DARK: [f32; 3] = [24.0, 32.0, 42.0];
const LAND_INK: [f32; 3] = [104.0, 118.0, 133.0];

/// Turn a light GSI tile pixel (blank / pale map) into the dark dashboard palette.
fn dark_style(p: [u8; 4]) -> [f32; 3] {
    if p[3] < 8 {
        return WATER;
    }
    let [r, g, b] = [p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0];
    let l = 0.299 * r + 0.587 * g + 0.114 * b;
    let blueish = b - r > 0.06 && b >= g - 0.02;
    if blueish {
        // water bodies: keep slightly lighter than the open-sea colour
        let k = (1.0 - l).clamp(0.0, 1.0) * 0.6;
        return lerp3(WATER, [22.0, 38.0, 56.0], k);
    }
    // land: bright → dark slate, dark ink (borders, labels) → light grey
    let ink = ((1.0 - l) * 1.6).clamp(0.0, 1.0).powf(0.9);
    lerp3(LAND_DARK, LAND_INK, ink)
}

fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

fn compose_base(view: &Viewport, base: &Mosaic) -> RgbImage {
    let mut img = RgbImage::new(view.width, view.height);
    let (l, t) = (view.left as i64, view.top as i64);
    for (x, y, px) in img.enumerate_pixels_mut() {
        let c = match base.pixel(l + x as i64, t + y as i64) {
            Some(p) => dark_style(p),
            None => WATER,
        };
        *px = Rgb([c[0] as u8, c[1] as u8, c[2] as u8]);
    }
    img
}

fn overlay_radar(img: &mut RgbImage, view: &Viewport, radar: &Mosaic, s: f64) {
    for (x, y, px) in img.enumerate_pixels_mut() {
        let wx = (view.left + x as f64 + 0.5) * s;
        let wy = (view.top + y as f64 + 0.5) * s;
        let c = radar.sample(wx, wy);
        let a = c[3] * 0.88;
        if a > 0.003 {
            for (dst, src) in px.0.iter_mut().zip(c) {
                *dst = (*dst as f32 * (1.0 - a) + src * a).round() as u8;
            }
        }
    }
}

fn finish(img: RgbImage, profile: &ProfileSettings) -> Result<(Vec<u8>, u32, u32), SourceError> {
    let (w, h) = profile.radar_size();
    let img = if (w, h) != img.dimensions() {
        image::imageops::resize(&img, w, h, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    Ok((encode_png(&img)?, w, h))
}

pub fn encode_png(img: &RgbImage) -> Result<Vec<u8>, SourceError> {
    use image::ImageEncoder;
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new_with_quality(
        &mut out,
        image::codecs::png::CompressionType::Fast,
        image::codecs::png::FilterType::Adaptive,
    )
    .write_image(img.as_raw(), img.width(), img.height(), image::ExtendedColorType::Rgb8)
    .map_err(|e| SourceError::new(ErrorKind::Io, format!("PNG encode: {e}")))?;
    Ok(out)
}

/// Downloaded tiles are 256–512 px; refuse anything larger before decoding
/// so a malformed or hostile PNG cannot make us allocate huge buffers.
const MAX_TILE_PX: u32 = 1024;

fn decode_tile(bytes: &[u8]) -> Option<RgbaImage> {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_TILE_PX);
    limits.max_image_height = Some(MAX_TILE_PX);
    limits.max_alloc = Some(16 * 1024 * 1024);
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), image::ImageFormat::Png);
    reader.limits(limits);
    reader.decode().ok().map(|i| i.to_rgba8())
}

// ───────────────────────────── progress & animation timing ─────────────────────────────

/// Download progress of the running radar refresh. Written by the fetch task
/// (atomics, no locking), read by the UI once per second.
#[derive(Debug, Default)]
pub struct Progress {
    active: AtomicBool,
    planned: AtomicBool,
    done: AtomicU32,
    total: AtomicU32,
    frames_done: AtomicU32,
    frames_total: AtomicU32,
}

/// A consistent-enough copy of [`Progress`] for display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgressSnapshot {
    /// false while the frame list (index) is still being fetched.
    pub planned: bool,
    pub done: u32,
    pub total: u32,
    pub frames_done: u32,
    pub frames_total: u32,
}

impl ProgressSnapshot {
    /// 0..=100 (100 only when everything is downloaded).
    pub fn percent(&self) -> u32 {
        (self.done.min(self.total) * 100).checked_div(self.total).unwrap_or(0)
    }
}

impl Progress {
    pub fn begin(&self) {
        self.done.store(0, Ordering::Relaxed);
        self.total.store(0, Ordering::Relaxed);
        self.frames_done.store(0, Ordering::Relaxed);
        self.frames_total.store(0, Ordering::Relaxed);
        self.planned.store(false, Ordering::Relaxed);
        self.active.store(true, Ordering::Release);
    }

    pub fn plan(&self, tiles: u32, frames: u32) {
        self.total.store(tiles, Ordering::Relaxed);
        self.frames_total.store(frames, Ordering::Relaxed);
        self.planned.store(true, Ordering::Release);
    }

    pub fn tile_done(&self) {
        self.done.fetch_add(1, Ordering::Relaxed);
    }

    pub fn frame_done(&self) {
        self.frames_done.fetch_add(1, Ordering::Relaxed);
    }

    pub fn end(&self) {
        self.active.store(false, Ordering::Release);
    }

    /// `None` when no refresh is running.
    pub fn snapshot(&self) -> Option<ProgressSnapshot> {
        self.active.load(Ordering::Acquire).then(|| ProgressSnapshot {
            planned: self.planned.load(Ordering::Acquire),
            done: self.done.load(Ordering::Relaxed),
            total: self.total.load(Ordering::Relaxed),
            frames_done: self.frames_done.load(Ordering::Relaxed),
            frames_total: self.frames_total.load(Ordering::Relaxed),
        })
    }
}

/// Marks the progress finished however `fetch` returns.
struct ProgressGuard<'a>(&'a Progress);

impl Drop for ProgressGuard<'_> {
    fn drop(&mut self) {
        self.0.end();
    }
}

/// Length of one animation loop exactly as `ui/radar.slint` plays it: one tick
/// per frame, two extra ticks on "now" (unless it is the last frame), three
/// on the last frame, plus one tick to wrap around.
pub fn animation_loop(frames: usize, now_index: usize, frame_ms: u32) -> Duration {
    if frames <= 1 || frame_ms == 0 {
        return Duration::ZERO;
    }
    let ticks = if now_index + 1 < frames { frames + 5 } else { frames + 3 };
    Duration::from_millis(ticks as u64 * frame_ms as u64)
}

/// Automatic radar dwell: the whole animation twice (plus a short margin so
/// the second loop's last frame is seen); 10 s for a static image.
pub fn auto_dwell(frames: usize, now_index: usize, frame_ms: u32) -> Duration {
    let one = animation_loop(frames, now_index, frame_ms);
    if one.is_zero() {
        crate::screen::DEFAULT_RADAR_DWELL
    } else {
        one * 2 + Duration::from_millis(500)
    }
}

// ───────────────────────────── frame planning ─────────────────────────────

/// One frame to download: a precipitation tile layer valid at `time`.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameSpec {
    /// Cache key; frames with the same key are never downloaded twice.
    pub key: String,
    pub time: DateTime<Utc>,
    pub forecast: bool,
    pub zoom: u8,
    pub tile: u32,
    /// Tile URL with `{x}` / `{y}` placeholders.
    pub url: String,
}

/// Which frames to show, oldest first.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub frames: Vec<FrameSpec>,
    /// Index of the latest observation.
    pub now_index: usize,
    pub source: &'static str,
    pub credit: &'static str,
    pub fallback: bool,
}

/// One entry of JMA `targetTimes_N1.json` (observations) / `_N2.json` (forecast).
#[derive(Debug, Clone, Deserialize)]
pub struct JmaTime {
    pub basetime: String,
    pub validtime: String,
    #[serde(default)]
    pub elements: Vec<String>,
}

/// JMA timestamps are UTC `YYYYMMDDhhmmss`.
fn jma_ts(s: &str) -> Option<DateTime<Utc>> {
    NaiveDateTime::parse_from_str(s, "%Y%m%d%H%M%S")
        .ok()
        .map(|n| n.and_utc())
}

/// Choose observation + forecast frames from the JMA time lists.
///
/// Observations: the latest one and every `radar_step_minutes` back, up to
/// `radar_frames` in total. Forecasts: from the newest forecast run, every
/// `radar_step_minutes` after the latest observation, up to
/// `radar_forecast_frames`. Forecasts more than 30 min ahead are a 1 km
/// product, so they are fetched at the coarser even zoom (see [`jma_zooms`]).
pub fn plan_jma(
    past: &[JmaTime],
    forecast: &[JmaTime],
    profile: &ProfileSettings,
    view_zoom: u8,
    base_url: &str,
) -> Result<Plan, SourceError> {
    let has_rain = |t: &JmaTime| t.elements.iter().any(|e| e == "hrpns");
    let mut obs: Vec<(&JmaTime, DateTime<Utc>)> = past
        .iter()
        .filter(|t| has_rain(t) && t.basetime == t.validtime)
        .filter_map(|t| jma_ts(&t.validtime).map(|v| (t, v)))
        .collect();
    obs.sort_by_key(|(_, v)| *v);
    let Some(&(latest, latest_t)) = obs.last() else {
        return Err(SourceError::parse("気象庁ナウキャスト: 観測時刻の一覧が空です"));
    };
    let step_min = (profile.radar_step_minutes.max(5) / 5 * 5) as i64;
    let (zoom, coarse) = jma_zooms(view_zoom);
    let spec = |t: &JmaTime, time: DateTime<Utc>, forecast: bool, z: u8| FrameSpec {
        key: format!("jma/{}/{}/{z}", t.basetime, t.validtime),
        time,
        forecast,
        zoom: z,
        tile: JMA_TILE,
        url: format!(
            "{base_url}/{}/none/{}/surf/hrpns/{z}/{{x}}/{{y}}.png",
            t.basetime, t.validtime
        ),
    };

    let mut frames = Vec::new();
    for k in (1..profile.radar_frames.max(1) as i64).rev() {
        let want = latest_t - chrono::Duration::minutes(step_min * k);
        if let Some((t, v)) = obs.iter().find(|(_, v)| *v == want) {
            frames.push(spec(t, *v, false, zoom));
        }
    }
    frames.push(spec(latest, latest_t, false, zoom));
    let now_index = frames.len() - 1;

    if profile.radar_forecast_frames > 0
        && let Some(run) = forecast
            .iter()
            .filter(|t| has_rain(t))
            .map(|t| t.basetime.as_str())
            .max()
    {
        let run_t = jma_ts(run);
        let mut fc: Vec<(&JmaTime, DateTime<Utc>)> = forecast
            .iter()
            .filter(|t| has_rain(t) && t.basetime == run)
            .filter_map(|t| jma_ts(&t.validtime).map(|v| (t, v)))
            .filter(|(_, v)| *v > latest_t && (*v - latest_t).num_minutes() % step_min == 0)
            .collect();
        fc.sort_by_key(|(_, v)| *v);
        for (t, v) in fc.into_iter().take(profile.radar_forecast_frames.min(12) as usize) {
            let lead = run_t.map(|r| (v - r).num_minutes()).unwrap_or(0);
            let z = if lead > 30 { coarse } else { zoom };
            frames.push(spec(t, v, true, z));
        }
    }
    Ok(Plan {
        frames,
        now_index,
        source: "気象庁",
        credit: JMA_CREDIT,
        fallback: false,
    })
}

#[derive(Deserialize)]
struct RainViewerIndex {
    host: String,
    radar: RainViewerRadar,
}

#[derive(Deserialize)]
struct RainViewerRadar {
    past: Vec<RainViewerFrame>,
}

#[derive(Deserialize, Clone)]
struct RainViewerFrame {
    time: i64,
    path: String,
}

/// RainViewer has 10-minute frames and no forecast; cover the same time span.
fn plan_rainviewer(index: &RainViewerIndex, profile: &ProfileSettings, view_zoom: u8, color: u8, smooth: bool) -> Plan {
    let span = (profile.radar_frames.max(1) - 1) * profile.radar_step_minutes.max(5);
    let n = (span / 10 + 1) as usize;
    let z = RAINVIEWER_MAX_ZOOM.min(view_zoom.saturating_sub(1));
    let host = &index.host;
    let frames: Vec<FrameSpec> = index
        .radar
        .past
        .iter()
        .rev()
        .take(n)
        .rev()
        .map(|f| FrameSpec {
            key: format!("rv{}/{z}", f.path),
            time: DateTime::from_timestamp(f.time, 0).unwrap_or_else(clock::now_utc),
            forecast: false,
            zoom: z,
            tile: RAINVIEWER_TILE,
            url: format!(
                "{host}{}/{RAINVIEWER_TILE}/{z}/{{x}}/{{y}}/{color}/{}_0.png",
                f.path,
                u8::from(smooth)
            ),
        })
        .collect();
    Plan {
        now_index: frames.len().saturating_sub(1),
        frames,
        source: "RainViewer",
        credit: RAINVIEWER_CREDIT,
        fallback: false,
    }
}

async fn jma_plan(ctx: &Context, profile: &ProfileSettings, view_zoom: u8) -> Result<Plan, SourceError> {
    let base = ctx.cfg.radar.jma_base_url.trim_end_matches('/');
    let body = ctx
        .http
        .get_bytes(&format!("{base}/targetTimes_N1.json"), 512 * 1024)
        .await?;
    let past: Vec<JmaTime> =
        serde_json::from_slice(&body).map_err(|e| SourceError::parse(format!("気象庁 targetTimes_N1: {e}")))?;
    let forecast: Vec<JmaTime> = if profile.radar_forecast_frames > 0 {
        // A missing forecast list only drops the forecast part.
        match ctx
            .http
            .get_bytes(&format!("{base}/targetTimes_N2.json"), 512 * 1024)
            .await
        {
            Ok(b) => serde_json::from_slice(&b).unwrap_or_else(|e| {
                tracing::warn!("気象庁 targetTimes_N2 を解釈できません: {e}");
                Vec::new()
            }),
            Err(e) => {
                tracing::warn!("気象庁の予測一覧を取得できません: {e}");
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    plan_jma(&past, &forecast, profile, view_zoom, base)
}

async fn rainviewer_plan(ctx: &Context, profile: &ProfileSettings, view_zoom: u8) -> Result<Plan, SourceError> {
    let body = ctx.http.get_bytes(&ctx.cfg.radar.index_url, 256 * 1024).await?;
    let index: RainViewerIndex =
        serde_json::from_slice(&body).map_err(|e| SourceError::parse(format!("RainViewer index: {e}")))?;
    Ok(plan_rainviewer(
        &index,
        profile,
        view_zoom,
        ctx.cfg.radar.color_scheme,
        ctx.cfg.radar.smooth,
    ))
}

// ───────────────────────────── live source ─────────────────────────────

/// State kept between refreshes so unchanged frames are not downloaded again.
#[derive(Default)]
pub struct LiveState {
    /// (viewport, profile size) the cache below was built for.
    key: Option<(u8, i64, i64, u32, u32)>,
    base_png: Option<Arc<[u8]>>,
    frames: HashMap<String, RadarFrame>,
}

/// Short name of the base map ("blank", "pale" …) taken from the URL template;
/// used to keep tile caches of different maps apart.
pub fn basemap_kind(url: &str) -> String {
    let name: String = url
        .split("/{z}")
        .next()
        .and_then(|p| p.rsplit('/').next())
        .unwrap_or("base")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if name.is_empty() { "base".into() } else { name }
}

fn tile_cache_path(ctx: &Context, z: u8, x: u32, y: u32) -> PathBuf {
    let kind = basemap_kind(&ctx.cfg.radar.basemap_url);
    ctx.paths.cache_dir.join(format!("tiles/{kind}/{z}/{x}/{y}.png"))
}

async fn base_tile(ctx: &Context, z: u8, x: u32, y: u32) -> Result<Option<Vec<u8>>, SourceError> {
    let path = tile_cache_path(ctx, z, x, y);
    let max_age = Duration::from_secs(ctx.cfg.radar.basemap_cache_days.max(1) * 86_400);
    let cached = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
    if let Some(m) = cached
        && SystemTime::now().duration_since(m).unwrap_or(Duration::MAX) < max_age
    {
        return Ok(std::fs::read(&path).ok());
    }
    let url = ctx
        .cfg
        .radar
        .basemap_url
        .replace("{z}", &z.to_string())
        .replace("{x}", &x.to_string())
        .replace("{y}", &y.to_string());
    match ctx.http.get_optional(&url, 512 * 1024).await {
        Ok(Some(bytes)) => {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&path, &bytes);
            Ok(Some(bytes))
        }
        Ok(None) => Ok(None),
        // Offline: an expired cached tile is better than none.
        Err(e) if cached.is_some() => {
            tracing::debug!("base tile refresh failed, using cache: {e}");
            Ok(std::fs::read(&path).ok())
        }
        Err(e) => Err(e),
    }
}

/// Tile downloads in flight at once — fast enough, still polite.
const PARALLEL_TILES: usize = 4;

/// Run `fetch` for every tile with bounded concurrency; results keep tile order.
async fn fetch_tiles<F, Fut>(
    tiles: &[(u32, u32)],
    progress: &Progress,
    fetch: F,
) -> Vec<((u32, u32), Result<Option<Vec<u8>>, SourceError>)>
where
    F: Fn(u32, u32) -> Fut,
    Fut: std::future::Future<Output = Result<Option<Vec<u8>>, SourceError>> + Send + 'static,
{
    let mut set = tokio::task::JoinSet::new();
    let mut out = Vec::with_capacity(tiles.len());
    let mut pending = tiles.iter().copied();
    loop {
        while set.len() < PARALLEL_TILES {
            let Some((x, y)) = pending.next() else { break };
            let fut = fetch(x, y);
            set.spawn(async move { ((x, y), fut.await) });
        }
        match set.join_next().await {
            Some(Ok(r)) => {
                progress.tile_done();
                out.push(r)
            }
            Some(Err(e)) => tracing::warn!("tile task failed: {e}"),
            None => break,
        }
    }
    out
}

async fn build_base(ctx: &Context, view: Viewport) -> Result<Arc<[u8]>, SourceError> {
    let mut mosaic = Mosaic {
        tile: BASE_TILE,
        tiles: HashMap::new(),
    };
    let tiles = view.tiles(BASE_TILE, 1.0);
    let results = fetch_tiles(&tiles, &ctx.radar_progress, |x, y| {
        let ctx = ctx.clone();
        async move { base_tile(&ctx, view.zoom, x, y).await }
    })
    .await;
    for ((x, y), r) in results {
        if let Some(img) = r?.as_deref().and_then(decode_tile) {
            mosaic.tiles.insert((x, y), img);
        }
    }
    let png = tokio::task::spawn_blocking(move || encode_png(&compose_base(&view, &mosaic)))
        .await
        .map_err(|e| SourceError::new(ErrorKind::Io, e.to_string()))??;
    Ok(png.into())
}

async fn build_frame(
    ctx: &Context,
    view: Viewport,
    profile: ProfileSettings,
    base_png: Arc<[u8]>,
    spec: &FrameSpec,
) -> Result<RadarFrame, SourceError> {
    let scale = view.grid_scale(spec.zoom, spec.tile);
    let mut mosaic = Mosaic {
        tile: spec.tile,
        tiles: HashMap::new(),
    };
    let tiles = view.tiles(spec.tile, scale);
    let mut failures = 0;
    let results = fetch_tiles(&tiles, &ctx.radar_progress, |x, y| {
        let url = spec.url.replace("{x}", &x.to_string()).replace("{y}", &y.to_string());
        let http = ctx.http.clone();
        async move { http.get_optional(&url, 2 * 1024 * 1024).await }
    })
    .await;
    for ((x, y), r) in results {
        match r {
            Ok(Some(bytes)) => {
                if let Some(img) = decode_tile(&bytes) {
                    mosaic.tiles.insert((x, y), img);
                }
            }
            Ok(None) => {}
            Err(e) => {
                failures += 1;
                if failures * 2 > tiles.len() {
                    return Err(e);
                }
            }
        }
    }
    let (time, forecast) = (spec.time, spec.forecast);
    tokio::task::spawn_blocking(move || {
        let mut img = image::load_from_memory_with_format(&base_png, image::ImageFormat::Png)
            .map_err(|e| SourceError::parse(format!("base map: {e}")))?
            .to_rgb8();
        overlay_radar(&mut img, &view, &mosaic, scale);
        drop(mosaic);
        let (png, _, _) = finish(img, &profile)?;
        Ok(RadarFrame {
            time,
            png: png.into(),
            forecast,
        })
    })
    .await
    .map_err(|e| SourceError::new(ErrorKind::Io, e.to_string()))?
}

pub async fn fetch(ctx: &Context, state: &tokio::sync::Mutex<LiveState>) -> Result<RadarData, SourceError> {
    ctx.radar_progress.begin();
    let _done = ProgressGuard(&ctx.radar_progress);
    let profile = *ctx.profile.borrow();
    let place = *ctx.place.borrow();
    let (lat, lon) = (place.radar_latitude, place.radar_longitude);
    // The zoom can change at runtime from the settings screen.
    let map_zoom = *ctx.radar_zoom.borrow();
    let view = Viewport::centered(lat, lon, map_zoom);
    let (w, h) = profile.radar_size();
    let key = (view.zoom, view.left as i64, view.top as i64, w, h);

    let plan = match ctx.cfg.radar.source {
        RadarSource::Jma => match jma_plan(ctx, &profile, view.zoom).await {
            Ok(p) => p,
            Err(e) if ctx.cfg.radar.fallback => {
                tracing::warn!("気象庁ナウキャストを取得できないため RainViewer で代替します: {e}");
                let mut p = rainviewer_plan(ctx, &profile, view.zoom).await?;
                p.fallback = true;
                p
            }
            Err(e) => return Err(e),
        },
        RadarSource::Rainviewer => rainviewer_plan(ctx, &profile, view.zoom).await?,
    };

    let mut st = state.lock().await;
    if st.key != Some(key) {
        st.frames.clear();
        st.base_png = None;
        st.key = Some(key);
    }
    // Size the work for the progress display: base map (if not cached yet)
    // plus the tiles of every frame that is not cached.
    let new_frames: Vec<&FrameSpec> = plan.frames.iter().filter(|f| !st.frames.contains_key(&f.key)).collect();
    let base_tiles = if st.base_png.is_none() {
        view.tiles(BASE_TILE, 1.0).len()
    } else {
        0
    };
    let frame_tiles: usize = new_frames
        .iter()
        .map(|f| view.tiles(f.tile, view.grid_scale(f.zoom, f.tile)).len())
        .sum();
    ctx.radar_progress
        .plan((base_tiles + frame_tiles) as u32, new_frames.len() as u32);

    let base_png = match &st.base_png {
        Some(b) => b.clone(),
        None => {
            let b = build_base(ctx, view).await?;
            st.base_png = Some(b.clone());
            b
        }
    };

    let now_key = plan.frames.get(plan.now_index).map(|f| f.key.clone());
    let mut frames = Vec::with_capacity(plan.frames.len());
    let mut kept = HashMap::with_capacity(plan.frames.len());
    let mut now_index = 0;
    for spec in &plan.frames {
        let is_now = now_key.as_ref() == Some(&spec.key);
        let frame = match st.frames.get(&spec.key) {
            Some(f) => f.clone(),
            None => match build_frame(ctx, view, profile, base_png.clone(), spec).await {
                Ok(f) => {
                    ctx.radar_progress.frame_done();
                    f
                }
                // Other frames are optional; the latest observation is required.
                Err(e) if !is_now => {
                    tracing::debug!("skipping radar frame {}: {e}", spec.key);
                    ctx.radar_progress.frame_done();
                    continue;
                }
                Err(e) => return Err(e),
            },
        };
        if is_now {
            now_index = frames.len();
        }
        kept.insert(spec.key.clone(), frame.clone());
        frames.push(frame);
    }
    st.frames = kept;

    Ok(RadarData {
        frames,
        now_index,
        width: w,
        height: h,
        attribution: attribution(plan.credit, map_zoom, &ctx.cfg.radar.basemap_url),
        source: plan.source.into(),
        fallback: plan.fallback,
        sample: false,
    })
}

// ───────────────────────────── fixture source ─────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FixtureRadar {
    /// Synthetic precipitation with the given intensity (0..=1).
    Frames(f32),
    /// Service reachable but no frames.
    Empty,
    /// Network failure.
    Error,
    /// Stale: frames present but fetched long ago and the last attempt failed.
    Stale(f32),
}

/// Synthetic, obviously non-geographic radar image: grid + moving blobs.
pub fn synth_frame(w: u32, h: u32, k: usize, n: usize, intensity: f32) -> RgbImage {
    let mut img = RgbImage::new(w, h);
    let cell = (w as f32 / 16.0).max(8.0);
    let t = if n > 1 { k as f32 / (n - 1) as f32 } else { 1.0 };
    let blobs = [
        (0.30 + 0.10 * t, 0.42 - 0.03 * t, 0.20, 1.00),
        (0.58 + 0.08 * t, 0.55 - 0.05 * t, 0.13, 0.80),
        (0.72 + 0.06 * t, 0.30 + 0.02 * t, 0.09, 0.95),
        (0.18 + 0.05 * t, 0.75 - 0.02 * t, 0.10, 0.55),
    ];
    let ramp = |v: f32| -> ([f32; 3], f32) {
        let stops: [(f32, [f32; 3]); 6] = [
            (0.05, [158.0, 200.0, 240.0]),
            (0.25, [62.0, 143.0, 219.0]),
            (0.45, [26.0, 95.0, 180.0]),
            (0.65, [245.0, 211.0, 58.0]),
            (0.80, [240.0, 140.0, 30.0]),
            (1.00, [217.0, 58.0, 58.0]),
        ];
        let mut prev = stops[0];
        for s in stops {
            if v <= s.0 {
                let f = ((v - prev.0) / (s.0 - prev.0).max(1e-3)).clamp(0.0, 1.0);
                return (lerp3(prev.1, s.1, f), (v * 4.0).clamp(0.0, 0.85));
            }
            prev = s;
        }
        (stops[5].1, 0.85)
    };
    for (x, y, px) in img.enumerate_pixels_mut() {
        let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
        let grid = (x as f32 % cell) < 1.0 || (y as f32 % cell) < 1.0;
        let mut c = if grid { [30.0, 40.0, 52.0] } else { [16.0, 22.0, 30.0] };
        let mut v = 0.0f32;
        for (bx, by, r, amp) in blobs {
            let d2 = ((fx - bx) * w as f32 / h as f32).powi(2) + (fy - by).powi(2);
            v += amp * (-d2 / (r * r)).exp();
        }
        v *= intensity;
        if v > 0.05 {
            let (col, a) = ramp(v.min(1.0));
            c = lerp3(c, col, a);
        }
        *px = Rgb([c[0] as u8, c[1] as u8, c[2] as u8]);
    }
    img
}

pub fn fixture_data(spec: FixtureRadar, profile: &ProfileSettings) -> Result<RadarData, SourceError> {
    let intensity = match spec {
        FixtureRadar::Frames(i) | FixtureRadar::Stale(i) => i,
        FixtureRadar::Empty => {
            return Ok(RadarData {
                frames: vec![],
                now_index: 0,
                width: 0,
                height: 0,
                attribution: String::new(),
                source: "サンプル".into(),
                fallback: false,
                sample: true,
            });
        }
        FixtureRadar::Error => {
            return Err(SourceError::new(ErrorKind::Network, "fixture: simulated network error"));
        }
    };
    let (w, h) = profile.radar_size();
    let past = profile.radar_frames.max(1) as usize;
    let total = profile.radar_total_frames() as usize;
    let step = profile.radar_step_minutes.max(5) as i64;
    let now = clock::now_utc();
    let latest = DateTime::from_timestamp(now.timestamp() / 300 * 300, 0).unwrap_or(now)
        - if matches!(spec, FixtureRadar::Stale(_)) {
            chrono::Duration::hours(3)
        } else {
            chrono::Duration::zero()
        };
    let mut frames = Vec::with_capacity(total);
    for k in 0..total {
        let img = synth_frame(w, h, k, total, intensity);
        frames.push(RadarFrame {
            time: latest + chrono::Duration::minutes(step * (k as i64 - (past as i64 - 1))),
            png: encode_png(&img)?.into(),
            forecast: k >= past,
        });
    }
    Ok(RadarData {
        frames,
        now_index: past - 1,
        width: w,
        height: h,
        attribution: "サンプル画像 (合成)".into(),
        source: "サンプル".into(),
        fallback: false,
        sample: true,
    })
}

/// Regenerates the fixture radar whenever the profile changes.
pub async fn run_fixture(ctx: Context, spec: FixtureRadar) {
    let mut rx = ctx.profile.clone();
    loop {
        let profile = *rx.borrow_and_update();
        ctx.repo.mark_loading(SourceId::Radar);
        let result = tokio::task::spawn_blocking(move || fixture_data(spec, &profile))
            .await
            .unwrap_or_else(|e| Err(SourceError::new(ErrorKind::Io, e.to_string())));
        let now = clock::now_utc();
        match result {
            Ok(data) => {
                let data = Payload::Radar(Arc::new(data));
                if let FixtureRadar::Stale(_) = spec {
                    ctx.repo.store_cached(data, now - chrono::Duration::hours(3));
                    ctx.repo.fail(
                        SourceId::Radar,
                        SourceError::new(ErrorKind::Network, "fixture: simulated outage"),
                        now,
                        None,
                    );
                } else {
                    ctx.repo.store(data, now, None);
                }
            }
            Err(e) => ctx.repo.fail(SourceId::Radar, e, now, None),
        }
        if rx.changed().await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        image::RgbaImage::new(w, h)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn oversized_tiles_are_rejected() {
        assert!(decode_tile(&png(256, 256)).is_some());
        assert!(decode_tile(&png(MAX_TILE_PX + 1, 1)).is_none());
        assert!(decode_tile(b"not a png").is_none());
    }

    #[test]
    fn world_px_matches_known_tile() {
        // Tokyo Station at z9 lies in tile (454, 201).
        let (x, y) = world_px(35.6812, 139.7671, 9, 256);
        assert_eq!(((x / 256.0) as u32, (y / 256.0) as u32), (454, 201));
    }

    #[test]
    fn viewport_tiles_cover_view() {
        let v = Viewport::centered(35.6812, 139.7671, 9);
        let base = v.tiles(BASE_TILE, 1.0);
        // 1376 px wide → 6 or 7 columns; 1012 px high → 4 or 5 rows.
        assert!((24..=35).contains(&base.len()), "{}", base.len());
        // JMA z10 tiles: about 4× the base tile count.
        let fine = v.tiles(JMA_TILE, v.grid_scale(10, JMA_TILE)).len();
        assert!((90..=130).contains(&fine), "{fine}");
        // One zoom coarser (forecast > 30 min) needs about a quarter of the tiles.
        assert!(v.tiles(JMA_TILE, v.grid_scale(8, JMA_TILE)).len() <= 12);
        // RainViewer z7 @ 512 px is half the base resolution.
        assert!((v.grid_scale(7, RAINVIEWER_TILE) - 0.5).abs() < 1e-9);
        let rv = v.tiles(RAINVIEWER_TILE, v.grid_scale(7, RAINVIEWER_TILE));
        assert!((2..=9).contains(&rv.len()), "{}", rv.len());
    }

    #[test]
    fn basemap_kind_and_attribution() {
        assert_eq!(
            basemap_kind("https://cyberjapandata.gsi.go.jp/xyz/blank/{z}/{x}/{y}.png"),
            "blank"
        );
        assert_eq!(
            basemap_kind("https://example.invalid/{z}/{x}/{y}.png"),
            "exampleinvalid"
        );
        assert!(
            attribution(
                JMA_CREDIT,
                9,
                "https://cyberjapandata.gsi.go.jp/xyz/pale/{z}/{x}/{y}.png"
            )
            .contains("淡色地図")
        );
        assert!(
            attribution(
                JMA_CREDIT,
                8,
                "https://cyberjapandata.gsi.go.jp/xyz/pale/{z}/{x}/{y}.png"
            )
            .contains("海上保安庁")
        );
        assert!(
            !attribution(
                JMA_CREDIT,
                8,
                "https://cyberjapandata.gsi.go.jp/xyz/blank/{z}/{x}/{y}.png"
            )
            .contains("海上保安庁")
        );
    }

    fn jt(base: &str, valid: &str) -> JmaTime {
        JmaTime {
            basetime: base.into(),
            validtime: valid.into(),
            elements: vec!["hrpns".into(), "hrpns_nd".into()],
        }
    }

    /// Observations every 5 min from 19:05 to 22:05 UTC and a forecast run
    /// based at 22:05 (22:10 … 23:05), like the live JMA lists.
    fn jma_lists() -> (Vec<JmaTime>, Vec<JmaTime>) {
        let t0 = jma_ts("20261010190500").unwrap();
        let fmt = |t: DateTime<Utc>| t.format("%Y%m%d%H%M%S").to_string();
        let past = (0..37)
            .map(|k| {
                let t = fmt(t0 + chrono::Duration::minutes(5 * k));
                jt(&t, &t)
            })
            .rev()
            .collect();
        let base = "20261010220500";
        let bt = jma_ts(base).unwrap();
        let fc = (1..=12)
            .map(|k| jt(base, &fmt(bt + chrono::Duration::minutes(5 * k))))
            .collect();
        (past, fc)
    }

    #[test]
    fn jma_plan_balanced_is_past_hour_plus_forecast_hour_every_10_min() {
        let (past, fc) = jma_lists();
        let p = ProfileSettings::defaults(crate::config::Profile::Balanced);
        let plan = plan_jma(&past, &fc, &p, 9, "https://x").unwrap();
        let times: Vec<String> = plan.frames.iter().map(|f| f.time.format("%H:%M").to_string()).collect();
        assert_eq!(
            times,
            [
                "21:05", "21:15", "21:25", "21:35", "21:45", "21:55", "22:05", "22:15", "22:25", "22:35", "22:45",
                "22:55", "23:05"
            ]
        );
        assert_eq!(plan.now_index, 6);
        assert!(plan.frames[..=6].iter().all(|f| !f.forecast));
        assert!(plan.frames[7..].iter().all(|f| f.forecast));
        // Observations and ≤ 30 min ahead at z10 (finer than the z9 view),
        // beyond that at z8 — JMA only serves even zoom levels.
        assert_eq!(plan.frames[0].zoom, 10);
        assert_eq!(plan.frames[9].zoom, 10);
        assert_eq!(plan.frames[10].zoom, 8);
        assert_eq!(
            plan.frames[6].url,
            "https://x/20261010220500/none/20261010220500/surf/hrpns/10/{x}/{y}.png"
        );
        assert_eq!(
            plan.frames[12].url,
            "https://x/20261010220500/none/20261010230500/surf/hrpns/8/{x}/{y}.png"
        );
    }

    #[test]
    fn animation_loop_matches_slint_timing() {
        // Balanced: 13 frames, now = 6 → 18 ticks × 500 ms = 9 s; twice + 0.5 s.
        assert_eq!(animation_loop(13, 6, 500), Duration::from_millis(9_000));
        assert_eq!(auto_dwell(13, 6, 500), Duration::from_millis(18_500));
        // Full: 25 frames × 300 ms → 9 s per loop.
        assert_eq!(animation_loop(25, 12, 300), Duration::from_millis(9_000));
        // Observations only (RainViewer fallback): now is the last frame.
        assert_eq!(animation_loop(7, 6, 500), Duration::from_millis(5_000));
        // Static image.
        assert_eq!(auto_dwell(1, 0, 0), crate::screen::DEFAULT_RADAR_DWELL);
    }

    #[test]
    fn progress_counts_and_finishes() {
        let p = Progress::default();
        assert_eq!(p.snapshot(), None);
        p.begin();
        assert!(!p.snapshot().unwrap().planned);
        p.plan(200, 13);
        for _ in 0..84 {
            p.tile_done();
        }
        p.frame_done();
        let s = p.snapshot().unwrap();
        assert_eq!((s.percent(), s.frames_done, s.frames_total), (42, 1, 13));
        {
            let _g = ProgressGuard(&p);
        }
        assert_eq!(p.snapshot(), None, "guard ends the refresh");
    }

    #[test]
    fn jma_zoom_levels_are_even() {
        assert_eq!(jma_zooms(9), (10, 8));
        assert_eq!(jma_zooms(8), (8, 8));
        assert_eq!(jma_zooms(10), (10, 10));
        assert_eq!(jma_zooms(11), (10, 10));
        assert_eq!(jma_zooms(5), (6, 4));
        let v = Viewport::centered(35.6812, 139.7671, 9);
        // z10 tiles are downsampled 2× onto the z9 view.
        assert!((v.grid_scale(10, JMA_TILE) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn jma_plan_full_and_low() {
        let (past, fc) = jma_lists();
        let full = ProfileSettings::defaults(crate::config::Profile::Full);
        let plan = plan_jma(&past, &fc, &full, 9, "u").unwrap();
        assert_eq!(plan.frames.len(), 25);
        assert_eq!(plan.now_index, 12);
        let low = ProfileSettings::defaults(crate::config::Profile::Low);
        let plan = plan_jma(&past, &fc, &low, 9, "u").unwrap();
        assert_eq!(plan.frames.len(), 1);
        assert!(!plan.frames[0].forecast);
        assert_eq!(plan.frames[0].time.format("%H:%M").to_string(), "22:05");
    }

    #[test]
    fn jma_plan_tolerates_missing_forecast_and_gaps() {
        let (mut past, _) = jma_lists();
        past.retain(|t| t.validtime != "20261010213500"); // a missing observation
        let p = ProfileSettings::defaults(crate::config::Profile::Balanced);
        let plan = plan_jma(&past, &[], &p, 9, "u").unwrap();
        assert_eq!(plan.frames.len(), 6, "gap skipped, no forecast");
        assert_eq!(plan.now_index, 5);
        assert!(plan_jma(&[], &[], &p, 9, "u").is_err());
    }

    #[test]
    fn rainviewer_plan_covers_same_span() {
        let index = RainViewerIndex {
            host: "https://h".into(),
            radar: RainViewerRadar {
                past: (0..13)
                    .map(|k| RainViewerFrame {
                        time: 1_800_000_000 + k * 600,
                        path: format!("/p{k}"),
                    })
                    .collect(),
            },
        };
        let p = ProfileSettings::defaults(crate::config::Profile::Balanced);
        let plan = plan_rainviewer(&index, &p, 9, 2, true);
        assert_eq!(plan.frames.len(), 7, "60 min at 10-min steps");
        assert_eq!(plan.now_index, 6);
        assert_eq!(plan.frames[6].url, "https://h/p12/512/7/{x}/{y}/2/1_0.png");
    }

    #[test]
    fn dark_style_maps_light_land_to_dark() {
        let land = dark_style([240, 240, 236, 255]);
        let ink = dark_style([60, 60, 60, 255]);
        assert!(land[0] < 60.0 && ink[0] > 100.0);
        assert_eq!(dark_style([0, 0, 0, 0]), WATER);
    }

    #[test]
    fn fixture_frames_follow_profile() {
        let b = ProfileSettings::defaults(crate::config::Profile::Balanced);
        let d = fixture_data(FixtureRadar::Frames(0.8), &b).unwrap();
        assert_eq!(d.frames.len(), 13);
        assert_eq!(d.now_index, 6);
        assert!(d.frames[7].forecast && !d.frames[6].forecast);
        let p = ProfileSettings::defaults(crate::config::Profile::Low);
        let d = fixture_data(FixtureRadar::Frames(0.8), &p).unwrap();
        assert_eq!(d.frames.len(), 1);
        assert!(d.sample);
        assert_eq!((d.width, d.height), p.radar_size());
        let img = image::load_from_memory(&d.frames[0].png).unwrap();
        assert_eq!(img.width(), d.width);
    }

    #[test]
    fn mosaic_sampling_handles_missing_tiles() {
        let mut m = Mosaic {
            tile: 2,
            tiles: HashMap::new(),
        };
        m.tiles
            .insert((0, 0), RgbaImage::from_pixel(2, 2, image::Rgba([200, 0, 0, 255])));
        let inside = m.sample(1.0, 1.0);
        assert!((inside[0] - 200.0).abs() < 0.5 && (inside[3] - 1.0).abs() < 1e-6);
        assert_eq!(m.sample(10.0, 10.0), [0.0; 4]);
    }
}
