//! End-to-end: fixture mode → services → controller → software renderer → PNG.
//! (Separate test binary because it installs its own Slint platform.)

use home_display::app::{self, Options};

#[test]
fn headless_screenshots_of_all_screens() {
    let dir = std::env::temp_dir().join(format!("home-display-shots-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    // Isolate from the developer's saved settings-screen overrides and caches.
    // SAFETY: this test binary runs a single test, before any other thread exists.
    unsafe {
        std::env::set_var("XDG_STATE_HOME", dir.join("state"));
        std::env::set_var("XDG_CACHE_HOME", dir.join("cache"));
        for (k, _) in std::env::vars() {
            if k.starts_with("HOME_DISPLAY_") {
                std::env::remove_var(k);
            }
        }
    }
    let opts = Options {
        fixture: Some("rain".into()),
        screenshot_dir: Some(dir.clone()),
        now: Some(
            chrono::DateTime::parse_from_rfc3339("2026-10-11T10:40:00+09:00")
                .unwrap()
                .into(),
        ),
        // Avoid picking up a developer's real config file.
        config: Some(write_config(&dir)),
        ..Default::default()
    };
    app::run(opts).expect("screenshot run");
    for screen in ["overview", "radar", "settings"] {
        let path = dir.join(format!("rain-balanced-{screen}.png"));
        let img = image::open(&path)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
            .to_rgb8();
        assert_eq!(img.dimensions(), (1920, 1080));
        // Not a blank frame: there must be clearly different pixels.
        let (min, max) = img.pixels().fold((255u8, 0u8), |(lo, hi), p| {
            let l = p.0[1];
            (lo.min(l), hi.max(l))
        });
        assert!(max - min > 100, "{screen} looks blank ({min}..{max})");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn write_config(dir: &std::path::Path) -> std::path::PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let p = dir.join("config.toml");
    std::fs::write(&p, "mode = \"fixture\"\n").unwrap();
    p
}
