//! Conversions from `view::*` (plain Rust) to the Slint-generated structs.
//! Runs on the UI thread only (Slint models are `Rc`-based).

use slint::{ModelRc, SharedString, VecModel};

use crate::model::WeatherKind;
use crate::status::Health;
use crate::ui;
use crate::view;

fn s(v: &str) -> SharedString {
    SharedString::from(v)
}

fn model<T: Clone + 'static>(v: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(v))
}

pub fn health(h: Health) -> ui::Health {
    match h {
        Health::Ok => ui::Health::Ok,
        Health::Retrying => ui::Health::Retrying,
        Health::Stale => ui::Health::Stale,
        Health::Error => ui::Health::Error,
        Health::Loading => ui::Health::Loading,
        Health::Unconfigured => ui::Health::Unconfigured,
        Health::Disabled => ui::Health::Disabled,
        Health::Sample => ui::Health::Sample,
    }
}

pub fn panel(p: view::Panel) -> ui::PanelState {
    match p {
        view::Panel::Ready => ui::PanelState::Ready,
        view::Panel::Empty => ui::PanelState::Empty,
        view::Panel::Loading => ui::PanelState::Loading,
        view::Panel::Error => ui::PanelState::Error,
        view::Panel::Unconfigured => ui::PanelState::Unconfigured,
        view::Panel::Disabled => ui::PanelState::Disabled,
    }
}

pub fn weather_kind(k: WeatherKind) -> ui::WeatherKind {
    match k {
        WeatherKind::Clear => ui::WeatherKind::Clear,
        WeatherKind::PartlyCloudy => ui::WeatherKind::PartlyCloudy,
        WeatherKind::Cloudy => ui::WeatherKind::Cloudy,
        WeatherKind::Fog => ui::WeatherKind::Fog,
        WeatherKind::Drizzle => ui::WeatherKind::Drizzle,
        WeatherKind::Rain => ui::WeatherKind::Rain,
        WeatherKind::HeavyRain => ui::WeatherKind::HeavyRain,
        WeatherKind::Snow => ui::WeatherKind::Snow,
        WeatherKind::Thunder => ui::WeatherKind::Thunder,
        WeatherKind::Unknown => ui::WeatherKind::Unknown,
    }
}

pub fn header(v: &view::HeaderView) -> ui::HeaderData {
    ui::HeaderData {
        date: s(&v.date),
        weekday: s(&v.weekday),
        year: s(&v.year),
        holiday: s(&v.holiday),
        red_day: v.red_day,
        status_text: s(&v.status_text),
        status_health: health(v.status_health.unwrap_or(Health::Loading)),
        updated: s(&v.updated),
        sample: v.sample,
    }
}

pub fn clock(v: &view::ClockView) -> ui::ClockData {
    ui::ClockData {
        time: s(&v.time),
        seconds: s(&v.seconds),
        subline: s(&v.subline),
    }
}

fn event(v: &view::EventRowView) -> ui::EventRow {
    ui::EventRow {
        start: s(&v.start),
        end: s(&v.end),
        title: s(&v.title),
        location: s(&v.location),
        all_day: v.all_day,
        ongoing: v.ongoing,
        relative: s(&v.relative),
    }
}

pub fn calendar(v: &view::CalendarView) -> ui::CalendarView {
    ui::CalendarView {
        state: panel(v.state),
        message: s(&v.message),
        detail: s(&v.detail),
        count_text: s(&v.count_text),
        has_next: v.next.is_some(),
        next_label: s(&v.next_label),
        next: v.next.as_ref().map(event).unwrap_or_default(),
        all_day: model(v.all_day.iter().map(event).collect()),
        rows: model(v.rows.iter().map(event).collect()),
        more_text: s(&v.more_text),
        finished_text: s(&v.finished_text),
        tomorrow_label: s(&v.tomorrow_label),
        tomorrow: model(v.tomorrow.iter().map(event).collect()),
        stale: v.stale,
        stale_text: s(&v.stale_text),
    }
}

pub fn weather(v: &view::WeatherView) -> ui::WeatherView {
    ui::WeatherView {
        state: panel(v.state),
        message: s(&v.message),
        detail: s(&v.detail),
        location: s(&v.location),
        kind: weather_kind(v.kind),
        is_day: v.is_day,
        temp: s(&v.temp),
        condition: s(&v.condition),
        feels: s(&v.feels),
        humidity: s(&v.humidity),
        pop: s(&v.pop),
        high: s(&v.high),
        low: s(&v.low),
        sun_text: s(&v.sun_text),
        as_of: s(&v.as_of),
        hours: model(
            v.hours
                .iter()
                .map(|h| ui::HourCell {
                    label: s(&h.label),
                    kind: weather_kind(h.kind),
                    is_day: h.is_day,
                    temp: s(&h.temp),
                    pop: s(&h.pop),
                    pop_high: h.pop_high,
                })
                .collect(),
        ),
        days: model(
            v.days
                .iter()
                .map(|d| ui::DayCell {
                    label: s(&d.label),
                    kind: weather_kind(d.kind),
                    high: s(&d.high),
                    low: s(&d.low),
                    pop: s(&d.pop),
                })
                .collect(),
        ),
        stale: v.stale,
        stale_text: s(&v.stale_text),
        dim: v.dim,
        attribution: s(&v.attribution),
    }
}

pub fn quake(v: &view::QuakeView) -> ui::QuakeView {
    ui::QuakeView {
        state: panel(v.state),
        message: s(&v.message),
        detail: s(&v.detail),
        has_latest: v.has_latest,
        shindo: s(&v.shindo),
        level: v.level,
        place: s(&v.place),
        occurred: s(&v.occurred),
        relative: s(&v.relative),
        magnitude: s(&v.magnitude),
        depth: s(&v.depth),
        tsunami: s(&v.tsunami),
        tsunami_alert: v.tsunami_alert,
        emphasis: v.emphasis,
        history: model(
            v.history
                .iter()
                .map(|q| ui::QuakeRow {
                    time: s(&q.time),
                    place: s(&q.place),
                    magnitude: s(&q.magnitude),
                    shindo: s(&q.shindo),
                    level: q.level,
                })
                .collect(),
        ),
        updated: s(&v.updated),
        stale: v.stale,
        stale_text: s(&v.stale_text),
        source: s(&v.source),
    }
}

pub fn footer(v: &view::FooterView) -> ui::FooterView {
    ui::FooterView {
        sources: model(
            v.sources
                .iter()
                .map(|c| ui::SourceChip {
                    name: s(&c.name),
                    health: health(c.health),
                    time: s(&c.time),
                })
                .collect(),
        ),
        right_text: s(&v.right_text),
    }
}

pub fn radar(v: &view::RadarView, decoding: bool) -> ui::RadarView {
    let (state, message) = if decoding && v.state == view::Panel::Ready {
        (ui::PanelState::Loading, s("画像を準備中…"))
    } else {
        (panel(v.state), s(&v.message))
    };
    ui::RadarView {
        state,
        message,
        detail: s(&v.detail),
        frame_times: model(v.frame_times.iter().map(|t| s(t)).collect()),
        frame_forecast: model(v.frame_forecast.clone()),
        frame_offsets: model(v.frame_offsets.iter().map(|t| s(t)).collect()),
        tick_labels: model(v.tick_labels.iter().map(|t| s(t)).collect()),
        now_index: v.now_index,
        source_note: s(&v.source_note),
        progress: v.progress,
        latest_label: s(&v.latest_label),
        age_text: s(&v.age_text),
        sample: v.sample,
        stale: v.stale,
        attribution: s(&v.attribution),
        location: s(&v.location),
        decoding,
    }
}

pub fn settings(v: &view::SettingsView) -> ui::SettingsView {
    ui::SettingsView {
        profile_index: v.profile.index(),
        profile_name: s(v.profile.name()),
        profile_detail: s(&v.profile_detail),
        rotation_enabled: v.rotation_enabled,
        rotation_paused: v.rotation_paused,
        overview_seconds: v.overview_seconds as i32,
        radar_seconds: v.radar_seconds as i32,
        radar_auto_seconds: v.radar_auto_seconds as i32,
        radar_zoom: v.radar_zoom as i32,
        radar_zoom_text: s(&v.radar_zoom_text),
        mode_text: s(&v.mode_text),
        config_path: s(&v.config_path),
        overrides_path: s(&v.overrides_path),
        cache_dir: s(&v.cache_dir),
        version: s(&v.version),
        radar_memory: s(&v.radar_memory),
        save_message: s(&v.save_message),
        ical_text: s(&v.ical_text),
        sources: model(
            v.sources
                .iter()
                .map(|r| ui::DiagRow {
                    name: s(&r.name),
                    health: health(r.health),
                    origin: s(&r.origin),
                    last_success: s(&r.last_success),
                    next: s(&r.next),
                    error: s(&r.error),
                })
                .collect(),
        ),
        logs: model(v.logs.iter().map(|l| s(l)).collect()),
    }
}

pub fn screen(sc: crate::screen::Screen) -> ui::ScreenKind {
    match sc {
        crate::screen::Screen::Overview => ui::ScreenKind::Overview,
        crate::screen::Screen::Radar => ui::ScreenKind::Radar,
        crate::screen::Screen::Settings => ui::ScreenKind::Settings,
    }
}
