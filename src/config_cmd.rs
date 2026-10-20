//! `home-display config edit | show | check` — the way settings are changed.
//!
//! The app never writes config.toml. `edit` works like `visudo`: the file is
//! copied, opened in an editor, checked when the editor exits, and only a
//! valid file replaces config.toml. The service is then restarted so the
//! display picks it up (a restart re-reads everything, so no setting needs
//! special "apply" code).

use std::io::{BufRead as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, bail};

use crate::app::{self, Options};
use crate::config::{self, AppConfig, Profile, Quantity};

/// systemd unit restarted after a successful edit of the system config.
pub const SERVICE: &str = "home-display";
pub const SYSTEM_CONFIG: &str = "/etc/home-display/config.toml";

/// Check `text` exactly as the app loads config.toml.
pub fn check_text(text: &str, path: &Path) -> Result<AppConfig, config::ConfigError> {
    let mut cfg = AppConfig::from_toml_str(text, path)?;
    cfg.normalize();
    cfg.validate()?;
    Ok(cfg)
}

#[derive(Debug, PartialEq, Eq)]
pub enum EditOutcome {
    /// Saved without changes: config.toml untouched, nothing restarted.
    Unchanged,
    /// A valid file replaced config.toml.
    Updated,
    /// The edit had errors and the user gave up: config.toml untouched.
    Discarded,
}

/// The edit loop, with the editor and the "edit again?" prompt injected
/// (tests drive it without a terminal).
pub fn edit_with(
    path: &Path,
    mut run_editor: impl FnMut(&Path) -> anyhow::Result<()>,
    mut edit_again: impl FnMut(&config::ConfigError) -> bool,
) -> anyhow::Result<EditOutcome> {
    let original = std::fs::read_to_string(path).with_context(|| format!("{} を読めません", path.display()))?;
    // Fail before the user spends time editing.
    std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .with_context(|| format!("{} に書き込めません（sudo を付けて実行してください）", path.display()))?;

    // Same directory, so the final rename is atomic.
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let tmp = Temp(dir.join(format!(".config.edit-{}.toml", std::process::id())));
    std::fs::write(&tmp.0, &original).with_context(|| format!("{} を作れません", tmp.0.display()))?;

    loop {
        run_editor(&tmp.0)?;
        let text = std::fs::read_to_string(&tmp.0)?;
        if text == original {
            return Ok(EditOutcome::Unchanged);
        }
        match check_text(&text, path) {
            Ok(_) => {
                replace(path, &tmp.0)?;
                return Ok(EditOutcome::Updated);
            }
            Err(e) => {
                if !edit_again(&e) {
                    return Ok(EditOutcome::Discarded);
                }
            }
        }
    }
}

/// Removes the working copy whatever happens.
struct Temp(PathBuf);

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Give the new file the original's mode and owner, then swap it in.
fn replace(path: &Path, new: &Path) -> anyhow::Result<()> {
    let meta = std::fs::metadata(path)?;
    std::fs::set_permissions(new, meta.permissions())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let new_meta = std::fs::metadata(new)?;
        if (new_meta.uid(), new_meta.gid()) != (meta.uid(), meta.gid()) {
            std::os::unix::fs::chown(new, Some(meta.uid()), Some(meta.gid()))
                .with_context(|| format!("{} の所有者を設定できません", new.display()))?;
        }
    }
    std::fs::rename(new, path).with_context(|| format!("{} を置き換えられません", path.display()))
}

/// `$SUDO_EDITOR`, `$VISUAL`, `$EDITOR`, else vi (nano if vi is missing).
fn editor() -> String {
    for var in ["SUDO_EDITOR", "VISUAL", "EDITOR"] {
        if let Ok(v) = std::env::var(var)
            && !v.trim().is_empty()
        {
            return v;
        }
    }
    let found = |name: &str| {
        std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(name).is_file()))
    };
    if found("vi") || !found("nano") {
        "vi".into()
    } else {
        "nano".into()
    }
}

fn run_editor(file: &Path) -> anyhow::Result<()> {
    let editor = editor();
    // Through the shell, so EDITOR may carry arguments ("code --wait").
    #[cfg(unix)]
    let status = Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("sh")
        .arg(file)
        .status();
    #[cfg(not(unix))]
    let status = Command::new(&editor).arg(file).status();
    let status = status.with_context(|| format!("エディタ {editor} を起動できません"))?;
    if !status.success() {
        bail!("エディタ {editor} が異常終了しました（config.toml はそのまま）");
    }
    Ok(())
}

fn ask_yes(question: &str) -> bool {
    print!("{question} [Y/n] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).unwrap_or(0) == 0 {
        return false;
    }
    !matches!(line.trim().to_ascii_lowercase().as_str(), "n" | "no")
}

fn config_path(opts: &Options) -> anyhow::Result<PathBuf> {
    match config::Paths::discover(opts.config.clone()).config_file {
        Some(p) => Ok(p),
        None => bail!(
            "設定ファイルがありません（{SYSTEM_CONFIG} か ~/.config/home-display/config.toml）。\n\
             テンプレート packaging/config/config.toml（Pi では /usr/share/doc/home-display/config.example.toml）\n\
             をコピーしてから実行してください"
        ),
    }
}

/// `home-display config edit`
pub fn edit(opts: &Options, restart: bool) -> anyhow::Result<()> {
    let path = config_path(opts)?;
    println!(
        "{} を編集します（保存してエディタを閉じるとチェックします）",
        path.display()
    );
    let outcome = edit_with(&path, run_editor, |e| {
        eprintln!("\n✗ 設定にエラーがあります:\n{e}\n");
        ask_yes("もう一度編集しますか？")
    })?;
    match outcome {
        EditOutcome::Unchanged => {
            println!("変更はありません（config.toml はそのまま。再起動もしません）");
            Ok(())
        }
        EditOutcome::Discarded => bail!("編集を破棄しました（config.toml はそのまま）"),
        EditOutcome::Updated => {
            println!("✓ チェック OK: {} を更新しました", path.display());
            apply(&path, restart)
        }
    }
}

/// Restart the service when the system config changed; otherwise explain.
fn apply(path: &Path, restart: bool) -> anyhow::Result<()> {
    let is_system = path == Path::new(SYSTEM_CONFIG);
    let has_systemd = Path::new("/run/systemd/system").exists();
    if !(is_system && has_systemd) {
        println!("アプリを起動し直すと反映されます");
        return Ok(());
    }
    if !restart {
        println!("反映するには: sudo systemctl restart {SERVICE}");
        return Ok(());
    }
    println!("{SERVICE} を再起動して反映します（雨雲の取り直しに 1 分ほどかかります）");
    let status = Command::new("systemctl")
        .args(["restart", SERVICE])
        .status()
        .context("systemctl を実行できません")?;
    if !status.success() {
        bail!(
            "再起動に失敗しました。状態を確認してください: systemctl status {SERVICE} / journalctl -u {SERVICE} -n 50"
        );
    }
    println!("✓ 再起動しました");
    Ok(())
}

/// `home-display config show`: every effective value, defaults included.
pub fn show(opts: &Options) -> anyhow::Result<()> {
    let loaded = app::load(opts)?;
    let mut cfg = loaded.cfg;
    if cfg.calendar.ical_url.is_some() {
        cfg.calendar.ical_url = Some("(hidden)".into());
    }
    let file = loaded
        .paths
        .config_file
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "(なし — 組み込みの既定値)".into());
    println!("# 実際に使われている設定（{file}）。書いていない項目は既定値です。");
    println!("# 項目の説明と選べる値は config.toml のテンプレートのコメントを参照してください。\n");
    print!(
        "{}",
        toml::to_string_pretty(&cfg).context("serializing the configuration")?
    );
    let ps = cfg.profile_settings(cfg.display.profile)?;
    println!(
        "\n# プロファイル {} の実際の値（[profiles.{}] で変更可）",
        cfg.display.profile.name(),
        cfg.display.profile.name().to_lowercase()
    );
    print!("{}", toml::to_string_pretty(&ps).context("serializing the profile")?);
    Ok(())
}

/// `home-display config check` (also `--check-config`): validate and summarise.
pub fn check(opts: &Options) -> anyhow::Result<()> {
    let l = app::load(opts)?;
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
    let fmt = config::fmt_secs;
    println!(
        "  location: {} ({:.4}, {:.4})\n  radar centre: ({rlat:.4}, {rlon:.4}) zoom {}\n  \
         rotation: {} (overview {}s / radar {})\n  refresh: calendar {} · weather {} · earthquake {} · \
         radar {} · holiday {}",
        c.location.name,
        c.location.latitude,
        c.location.longitude,
        c.radar.map_zoom,
        if c.rotation.enabled { "on" } else { "off" },
        c.rotation.overview().as_secs(),
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
            let shown: Vec<String> = Quantity::ALL
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
    let legacy = l.paths.legacy_overrides_file();
    if legacy.exists() {
        println!(
            "  note: {} is no longer read (settings live in config.toml only)",
            legacy.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_config(name: &str, text: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hd-config-cmd-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, text).unwrap();
        path
    }

    fn leftovers(path: &Path) -> Vec<String> {
        std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n != "config.toml")
            .collect()
    }

    const BASE: &str = "mode = \"live\"\n\n[display]\n# profile = \"balanced\"\n";

    #[test]
    fn valid_edit_replaces_the_file() {
        let path = temp_config("valid", BASE);
        let out = edit_with(
            &path,
            |f| {
                Ok(std::fs::write(
                    f,
                    BASE.replace("# profile = \"balanced\"", "profile = \"low\""),
                )?)
            },
            |_| panic!("no error expected"),
        )
        .unwrap();
        assert_eq!(out, EditOutcome::Updated);
        assert!(std::fs::read_to_string(&path).unwrap().contains("profile = \"low\""));
        assert!(leftovers(&path).is_empty(), "working copy removed");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn invalid_edit_never_reaches_the_file() {
        let path = temp_config("invalid", BASE);
        let mut errors = Vec::new();
        let out = edit_with(
            &path,
            |f| Ok(std::fs::write(f, "mode = \"live\"\n[display]\nprofile = \"fast\"\n")?),
            |e| {
                errors.push(e.to_string());
                false
            },
        )
        .unwrap();
        assert_eq!(out, EditOutcome::Discarded);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), BASE, "original kept");
        assert!(errors[0].contains("line 3"), "error points at the line: {}", errors[0]);
        assert!(leftovers(&path).is_empty());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_failed_check_can_be_fixed_in_the_same_session() {
        let path = temp_config("retry", BASE);
        let mut round = 0;
        let out = edit_with(
            &path,
            |f| {
                round += 1;
                let zoom = if round == 1 { 42 } else { 8 };
                Ok(std::fs::write(f, format!("{BASE}\n[radar]\nmap_zoom = {zoom}\n"))?)
            },
            |_| true,
        )
        .unwrap();
        assert_eq!((out, round), (EditOutcome::Updated, 2));
        assert!(std::fs::read_to_string(&path).unwrap().contains("map_zoom = 8"));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn saving_without_changes_does_nothing() {
        let path = temp_config("same", BASE);
        let out = edit_with(&path, |_| Ok(()), |_| panic!("no check expected")).unwrap();
        assert_eq!(out, EditOutcome::Unchanged);
        assert!(leftovers(&path).is_empty());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
