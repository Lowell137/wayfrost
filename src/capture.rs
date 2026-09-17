use anyhow::{Context, Result};
use image::DynamicImage;
use std::process::Command;

pub fn capture_screen() -> Result<DynamicImage> {
    // 1. Fast native grim (wlroots compositors)
    if let Ok(output) = Command::new("grim").arg("-").output() {
        if output.status.success() && !output.stdout.is_empty() {
            if let Ok(img) = image::load_from_memory(&output.stdout) {
                log::info!("Screen captured via `grim`");
                return Ok(img);
            }
        }
    }

    // 2. Wayfrost GNOME extension (instant, silent)
    if let Ok(img) = capture_via_gnome_extension() {
        return Ok(img);
    }

    // 3. GNOME Screenshot (reliable in GNOME / VMs)
    if let Ok(img) = capture_via_gnome_screenshot() {
        return Ok(img);
    }

    // 4. ImageMagick import (universal fallback for X11/XWayland/VMs)
    if let Ok(img) = capture_via_imagemagick() {
        return Ok(img);
    }

    // 5. XDG Desktop Portal fallback (interactive)
    log::info!("Using XDG Desktop Portal via gdbus (interactive)");
    capture_via_portal()
}

/// Calls org.wayfrost.Capture.CaptureScreen — wayfrost@lowell GNOME extension.
/// Returns the captured image directly from the temp file the extension wrote.
fn capture_via_gnome_extension() -> Result<DynamicImage> {
    let output = Command::new("gdbus")
        .args([
            "call",
            "--session",
            "--dest",
            "org.wayfrost.Capture",
            "--object-path",
            "/org/wayfrost/Capture",
            "--method",
            "org.wayfrost.Capture.CaptureScreen",
            "--timeout",
            "3",
        ])
        .output()
        .context("gdbus call to wayfrost extension failed")?;

    anyhow::ensure!(
        output.status.success(),
        "wayfrost extension DBus call non-zero"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Response format: (true, '/tmp/wayfrost_XXXXXXX.png',)
    let path = stdout
        .split('\'')
        .nth(1)
        .context("unexpected response from wayfrost extension")?;

    anyhow::ensure!(path.ends_with(".png"), "path doesn't look like png: {path}");

    let img = image::open(path)
        .with_context(|| format!("Cannot open extension screenshot: {path}"))?;
    let _ = std::fs::remove_file(path);
    log::info!("Screen captured via Wayfrost GNOME extension (instant, silent)");
    Ok(img)
}

/// Uses `gdbus` to call org.freedesktop.portal.Screenshot synchronously.
/// The portal drops the file in ~/Pictures; we read it and clean up.
fn capture_via_portal() -> Result<DynamicImage> {
    use std::fs;
    use std::time::{Duration, Instant, SystemTime};

    let home = dirs_home()?;
    let search_dirs = [home.join("Pictures"), home.join("Pictures").join("Screenshots")];

    let before_mtime = SystemTime::now() - Duration::from_secs(2);

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

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let mut candidates = Vec::new();
        for dir in &search_dirs {
            if let Ok(entries) = fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) == Some("png") {
                        if let Ok(meta) = fs::metadata(&path) {
                            if meta.len() > 512 {
                                if let Ok(modified) = meta.modified() {
                                    if modified > before_mtime {
                                        candidates.push((modified, path));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        candidates.sort_by(|a, b| b.0.cmp(&a.0));
        for (_, path) in candidates {
            if let Ok(img) = image::open(&path) {
                let _ = fs::remove_file(&path);
                log::info!("Screen captured via XDG Desktop Portal");
                return Ok(img);
            }
        }

        std::thread::sleep(Duration::from_millis(50));
    }
    anyhow::bail!("XDG Desktop Portal screenshot timed out — no valid PNG found in Pictures");
}

fn capture_via_gnome_screenshot() -> Result<DynamicImage> {
    let tmp_path = format!("/tmp/wayfrost_gs_{}.png", std::process::id());
    let status = Command::new("gnome-screenshot")
        .args(["-f", &tmp_path])
        .status()?;
    if status.success() {
        let img = image::open(&tmp_path)?;
        let _ = std::fs::remove_file(&tmp_path);
        log::info!("Screen captured via gnome-screenshot");
        return Ok(img);
    }
    anyhow::bail!("gnome-screenshot failed")
}

fn capture_via_imagemagick() -> Result<DynamicImage> {
    let tmp_path = format!("/tmp/wayfrost_im_{}.png", std::process::id());
    let status = Command::new("import")
        .args(["-window", "root", &tmp_path])
        .status()?;
    if status.success() {
        let img = image::open(&tmp_path)?;
        let _ = std::fs::remove_file(&tmp_path);
        log::info!("Screen captured via ImageMagick import");
        return Ok(img);
    }
    anyhow::bail!("ImageMagick import failed")
}

fn dirs_home() -> Result<std::path::PathBuf> {
    std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .context("HOME env var not set")
}
