//! Builds the real Slint UI on the headless testing backend and pushes the
//! view models of every fixture scenario through it.

use std::sync::Arc;

use chrono::{TimeZone, Utc};
use home_display::config::{AppConfig, Profile, ProfileSettings};
use home_display::fixtures::{FixtureSet, Scenario};
use home_display::model::RadarData;
use home_display::repository::{Payload, Repository, Snapshot};
use home_display::services::radar::{FixtureRadar, fixture_data};
use home_display::status::{Origin, SourceId, SourceStatus};
use home_display::ui::{self, MainWindow};
use home_display::{ui_bridge, view};
use slint::{ComponentHandle, Model};

fn snapshot_for(scenario: Scenario, now: chrono::DateTime<chrono_tz::Tz>) -> Snapshot {
    let repo = Repository::new(Snapshot::default());
    let t = now.with_timezone(&Utc);
    for id in SourceId::ALL {
        repo.set_status(id, SourceStatus::new(Origin::Fixture, chrono::Duration::minutes(60)));
    }
    let set = FixtureSet::load(scenario, now);
    set.apply_calendar(&repo, t);
    set.apply_weather(&repo, t);
    set.apply_earthquake(&repo, t);
    set.apply_holidays(&repo, t);
    let ps = ProfileSettings::defaults(Profile::Low);
    match fixture_data(set.radar_spec(), &ps) {
        Ok(d) => repo.store(Payload::Radar(Arc::new(d)), t, None),
        Err(e) => repo.fail(SourceId::Radar, e, t, None),
    }
    repo.snapshot()
}

#[test]
fn every_scenario_renders_through_the_real_ui() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = MainWindow::new().expect("window");
    let cfg = AppConfig::default();
    let now = chrono_tz::Asia::Tokyo
        .with_ymd_and_hms(2026, 10, 11, 10, 40, 0)
        .unwrap();

    for scenario in Scenario::ALL {
        for profile in Profile::ALL {
            let ps = ProfileSettings::defaults(profile);
            let snap = snapshot_for(scenario, now);
            let cx = view::ViewContext {
                snap: &snap,
                now,
                cfg: &cfg,
                profile,
                settings: &ps,
                fake_clock: true,
                radar_progress: None,
            };

            ui.set_header(ui_bridge::header(&view::header(&cx)));
            ui.set_clock(ui_bridge::clock(&view::clock(&cx)));
            let cal = view::calendar(&cx);
            ui.set_calendar(ui_bridge::calendar(&cal));
            ui.set_weather(ui_bridge::weather(&view::weather(&cx)));
            ui.set_quake(ui_bridge::quake(&view::quake(&cx)));
            ui.set_footer(ui_bridge::footer(&view::footer(&cx)));
            ui.set_radar(ui_bridge::radar(&view::radar(&cx), false));
            ui.global::<ui::Theme>().set_effects(ps.visual_effects);

            for screen in [
                ui::ScreenKind::Overview,
                ui::ScreenKind::Radar,
                ui::ScreenKind::Settings,
            ] {
                ui.set_screen(screen);
                ui.show().unwrap();
                ui.hide().unwrap();
            }

            // Round-trip: what the UI holds is what the view model said.
            let got = ui.get_calendar();
            assert_eq!(got.rows.row_count(), cal.rows.len(), "{scenario:?}/{profile:?}");
            assert_eq!(got.has_next, cal.next.is_some());
            assert_eq!(ui.get_clock().time.as_str(), "10:40");
        }
    }
}

#[test]
fn radar_frames_can_be_set_and_released() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = MainWindow::new().expect("window");
    let ps = ProfileSettings::defaults(Profile::Balanced);
    let data: RadarData = fixture_data(FixtureRadar::Frames(0.6), &ps).unwrap();
    let images: Vec<slint::Image> = data
        .frames
        .iter()
        .map(|f| {
            let img = image::load_from_memory(&f.png).unwrap().to_rgb8();
            slint::Image::from_rgb8(slint::SharedPixelBuffer::clone_from_slice(
                img.as_raw(),
                img.width(),
                img.height(),
            ))
        })
        .collect();
    ui.set_screen(ui::ScreenKind::Radar);
    ui.set_radar_frames(slint::ModelRc::new(slint::VecModel::from(images)));
    assert_eq!(ui.get_radar_frames().row_count(), ps.radar_total_frames() as usize);
    ui.set_radar_frames(slint::ModelRc::default());
    assert_eq!(ui.get_radar_frames().row_count(), 0);
}
