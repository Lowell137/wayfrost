use anyhow::{Context, Result};
use image::DynamicImage;
use std::process::Command;

/// Captures the full Wayland screen.
/// 1. Tries `grim -` (wlroots / Hyprland / Sway — direct pipe, no file).
/// 2. Falls back to XDG Desktop Portal via `gdbus` CLI (GNOME / KDE Wayland).
pub fn capture_screen() -> Result<DynamicImage> {
    // Try fast native grim first
    if let Ok(output) = Command::new("grim").arg("-").output() {
        if output.status.success() && !output.stdout.is_empty() {
            if let Ok(img) = image::load_from_memory(&output.stdout) {
                log::info!("Screen captured via `grim`");
                return Ok(img);
            }
        }
    }

    log::info!("`grim` not available or failed, using XDG Desktop Portal via gdbus");
    capture_via_portal()
}

/// Uses `gdbus` to call org.freedesktop.portal.Screenshot synchronously.
/// The portal drops the file in ~/Pictures; we read it and clean up.
fn capture_via_portal() -> Result<DynamicImage> {
    use std::fs;
    use std::time::{Duration, SystemTime};

    let pictures_dir = dirs_home()?.join("Pictures");

    // Record existing files so we can find the new one
    let before_mtime = SystemTime::now() - Duration::from_secs(1);

    let status = Command::new("gdbus")
        .args([
            "call",
            "--session",
            "--dest",
            "org.freedesktop.portal.Desktop",
            "--object-path",
            "/org/freedesktop/portal/desktop",
            "--method",
            "org.freedesktop.portal.Screenshot.Screenshot",
            "",
            "{'interactive': <false>, 'modal': <false>}",
        ])
        .status()
        .context("Failed to invoke gdbus for portal screenshot")?;

    anyhow::ensure!(status.success(), "gdbus portal call returned non-zero exit");

    // Give the portal up to 3 seconds to write the file
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        if let Ok(entries) = fs::read_dir(&pictures_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("png") {
                    continue;
                }
                if let Ok(meta) = fs::metadata(&path) {
                    if let Ok(modified) = meta.modified() {
                        if modified > before_mtime {
                            let img = image::open(&path)
                                .with_context(|| format!("Cannot open portal screenshot: {path:?}"))?;
                            let _ = fs::remove_file(&path);
                            log::info!("Screen captured via XDG Desktop Portal");
                            return Ok(img);
                        }
                    }
                }
            }
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    anyhow::bail!("XDG Desktop Portal screenshot timed out — no new PNG found in ~/Pictures");
}

fn dirs_home() -> Result<std::path::PathBuf> {
    std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .context("HOME env var not set")
}
