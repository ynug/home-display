use std::path::PathBuf;
use std::process::ExitCode;

use home_display::app::{self, Options};
use home_display::config::Profile;
use home_display::fixtures::Scenario;
use home_display::screen::Screen;

const HELP: &str = "\
home-display — always-on home information display

USAGE:
    home-display [OPTIONS]

OPTIONS:
    --config <FILE>          Config file (default: ~/.config/home-display/config.toml,
                             then /etc/home-display/config.toml)
    --fixture <SCENARIO>     Use bundled sample data (see --list-fixtures)
    --list-fixtures          List fixture scenarios and exit
    --profile <P>            full | balanced | low (overrides config)
    --screen <S>             Start on overview | radar | settings
    --no-rotate              Disable automatic screen rotation
    --now <TIME>             Pretend the current time is TIME (RFC 3339 or
                             'YYYY-MM-DDTHH:MM' in the configured zone); for demos
    --scale <F>              UI scale factor (sets SLINT_SCALE_FACTOR; desktop default 1.0)
    --fullscreen             Fullscreen window (desktop backend)
    --screenshot <PNG>       Render one screen headless (software renderer) and exit
    --screenshot-dir <DIR>   Render overview, radar and settings into DIR and exit
    --screenshot-advance <T> Advance UI timers by T (e.g. 5s) before rendering
    --screenshot-wait <T>    Max wait for data before rendering (default 90s)
    --check-config           Validate the configuration and exit
    --read-sensors           Read the [sensors] devices for 15 s, print the values and exit
    -V, --version            Print version
    -h, --help               Print this help

KEYS:
    1 overview · 2 radar · S settings · Space pause rotation · U refresh · Esc back
    (settings) P profile · A rotation · [ ] overview time · - = radar time · W save
";

fn parse_now(s: &str, tz: chrono_tz::Tz) -> Option<chrono::DateTime<chrono::Utc>> {
    use chrono::TimeZone;
    if let Ok(t) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(t.with_timezone(&chrono::Utc));
    }
    let naive = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M").ok()?;
    tz.from_local_datetime(&naive)
        .single()
        .map(|t| t.with_timezone(&chrono::Utc))
}

#[derive(Clone, Copy, PartialEq)]
enum Command {
    Run,
    CheckConfig,
    ReadSensors,
}

fn parse_args() -> Result<Option<(Options, Command)>, String> {
    let mut opts = Options::default();
    let mut command = Command::Run;
    let mut now_raw: Option<String> = None;
    let mut args = std::env::args().skip(1);
    let need = |args: &mut dyn Iterator<Item = String>, flag: &str| args.next().ok_or(format!("{flag} needs a value"));
    while let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                return Ok(None);
            }
            "-V" | "--version" => {
                println!("home-display {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            "--list-fixtures" => {
                for sc in Scenario::ALL {
                    println!("{:<10} {}", sc.name(), sc.describe());
                }
                return Ok(None);
            }
            "--config" => opts.config = Some(PathBuf::from(need(&mut args, &a)?)),
            "--fixture" => opts.fixture = Some(need(&mut args, &a)?),
            "--profile" => {
                let v = need(&mut args, &a)?;
                opts.profile = Some(Profile::parse(&v).ok_or(format!("unknown profile {v:?}"))?);
            }
            "--screen" => {
                opts.screen = Some(match need(&mut args, &a)?.as_str() {
                    "overview" => Screen::Overview,
                    "radar" => Screen::Radar,
                    "settings" => Screen::Settings,
                    other => return Err(format!("unknown screen {other:?}")),
                })
            }
            "--no-rotate" => opts.no_rotate = true,
            "--now" => now_raw = Some(need(&mut args, &a)?),
            "--scale" => {
                let v = need(&mut args, &a)?;
                let f: f32 = v.parse().map_err(|_| format!("bad scale {v:?}"))?;
                if !(0.25..=4.0).contains(&f) {
                    return Err("--scale must be between 0.25 and 4".into());
                }
                opts.scale = Some(f);
            }
            "--fullscreen" => opts.fullscreen = true,
            "--screenshot" => opts.screenshot = Some(PathBuf::from(need(&mut args, &a)?)),
            "--screenshot-dir" => opts.screenshot_dir = Some(PathBuf::from(need(&mut args, &a)?)),
            "--screenshot-wait" => {
                let v = need(&mut args, &a)?;
                let secs = home_display::config::parse_duration_secs(&v).ok_or(format!("bad duration {v:?}"))?;
                opts.screenshot_wait = Some(std::time::Duration::from_secs(secs));
            }
            "--screenshot-advance" => {
                let v = need(&mut args, &a)?;
                let secs = home_display::config::parse_duration_secs(&v).ok_or(format!("bad duration {v:?}"))?;
                opts.screenshot_advance = Some(std::time::Duration::from_secs(secs));
            }
            "--check-config" => command = Command::CheckConfig,
            "--read-sensors" => command = Command::ReadSensors,
            other => return Err(format!("unknown argument {other:?} (see --help)")),
        }
    }
    if let Some(raw) = now_raw {
        // Interpret local times in the configured zone.
        let tz = app::load(&opts).map(|l| l.tz).unwrap_or(chrono_tz::Asia::Tokyo);
        opts.now = Some(parse_now(&raw, tz).ok_or(format!("cannot parse --now {raw:?}"))?);
    }
    Ok(Some((opts, command)))
}

/// Poll the configured sensors for a while and print what they return.
fn read_sensors(opts: &Options) -> ExitCode {
    let loaded = match app::load(opts) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("config error: {e:#}");
            return ExitCode::FAILURE;
        }
    };
    let Some(cfg) = &loaded.cfg.sensors else {
        eprintln!("[sensors] が設定されていません (docs/sensors.md)");
        return ExitCode::FAILURE;
    };
    println!("bus: {} (15 秒間読み取ります。SCD41 の最初の値は約 5 秒後)", cfg.bus);
    let ok = home_display::sensors::probe(cfg, std::time::Duration::from_secs(15), |line| println!("{line}"));
    if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

fn main() -> ExitCode {
    home_display::diag::init_logging();
    let (opts, command) = match parse_args() {
        Ok(Some(v)) => v,
        Ok(None) => return ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    if command == Command::ReadSensors {
        return read_sensors(&opts);
    }
    if command == Command::CheckConfig {
        return match app::load(&opts).and_then(|l| {
            for p in Profile::ALL {
                l.cfg.profile_settings(p)?;
            }
            let ical = match l.cfg.resolve_ical_url() {
                Ok(Some(_)) => "configured (hidden)",
                Ok(None) => "not configured",
                Err(_) => "ERROR (unreadable)",
            };
            println!(
                "config OK: {}\n  mode: {:?}\n  profile: {}\n  timezone: {}\n  iCal URL: {}\n  cache: {}\n  state: {}",
                l.paths
                    .config_file
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "(built-in defaults)".into()),
                l.cfg.mode,
                l.cfg.display.profile.name(),
                l.tz,
                ical,
                l.paths.cache_dir.display(),
                l.paths.state_dir.display(),
            );
            let c = &l.cfg;
            let ps = c.profile_settings(c.display.profile)?;
            let (rlat, rlon) = c.radar_center();
            let fmt = home_display::config::fmt_secs;
            println!(
                "  location: {} ({:.4}, {:.4})\n  radar centre: ({rlat:.4}, {rlon:.4}) zoom {}\n  \
                 rotation: {} (overview {}s / radar {})\n  refresh: calendar {} · weather {} · earthquake {} · \
                 radar {} · holiday {}",
                c.location.name,
                c.location.latitude,
                c.location.longitude,
                c.radar.map_zoom,
                if c.rotation.enabled { "on" } else { "off" },
                c.rotation.overview_seconds,
                match c.rotation.radar() {
                    Some(d) => format!("{}s", d.as_secs()),
                    None => "auto (animation ×2)".into(),
                },
                fmt(c.calendar.refresh_minutes * 60),
                fmt(c.weather.refresh_minutes * 60),
                fmt(c.earthquake.refresh_seconds),
                fmt(c.radar_refresh(&ps).as_secs()),
                fmt(c.holiday.refresh_days * 86_400),
            );
            match &c.sensors {
                Some(s) => {
                    println!(
                        "  sensors: {} every {}s (stale after {}s)",
                        s.bus, s.interval_seconds, s.stale_seconds
                    );
                    for d in &s.device {
                        println!("    {} = {} @ 0x{:02X}", d.id, d.model.name(), d.address);
                    }
                    let shown: Vec<String> = home_display::config::Quantity::ALL
                        .iter()
                        .filter_map(|q| s.display.source(*q).map(|id| format!("{} ← {id}", q.key())))
                        .collect();
                    println!(
                        "    display: {}",
                        if shown.is_empty() {
                            "(none)".into()
                        } else {
                            shown.join(", ")
                        }
                    );
                }
                None => println!("  sensors: off"),
            }
            println!("  settings screen overrides: {}", l.paths.overrides_file().display());
            Ok(())
        }) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("config error: {e:#}");
                ExitCode::FAILURE
            }
        };
    }
    match app::run(opts) {
        Ok(()) if app::quit_requested() => ExitCode::from(app::QUIT_EXIT_CODE),
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e:#}");
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
