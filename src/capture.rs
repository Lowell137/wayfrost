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

    // No silent backend available. On GNOME/Wayland the only picker-free capture
    // is the Wayfrost extension; GNOME itself blocks silent portal capture
    // (interactive:false -> cancelled) and org.gnome.Shell.Screenshot (AccessDenied).
    anyhow::bail!(
        "No screen-capture backend available. On GNOME/Wayland install & enable the \
         Wayfrost extension, then log out and back in once so the shell reloads it."
    )
}

/// Calls org.wayfrost.Capture.CaptureScreen — wayfrost@lowell GNOME extension.
/// The extension captures into memory and returns the raw PNG bytes over D-Bus
/// (`(bay)`), so the sandbox shares no filesystem path with gnome-shell.
fn capture_via_gnome_extension() -> Result<DynamicImage> {
    use gtk4::gio::{self, DBusCallFlags};

    let conn = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)
        .context("no session D-Bus connection")?;
    let reply = conn
        .call_sync(
            Some("org.wayfrost.Capture"),
            "/org/wayfrost/Capture",
            "org.wayfrost.Capture",
            "CaptureScreen",
            None,
            glib::VariantTy::new("(bay)").ok(),
            DBusCallFlags::NONE,
            5000,
            gio::Cancellable::NONE,
        )
        .context("D-Bus call to wayfrost extension failed (is it installed & enabled?)")?;

    let ok: bool = reply.child_get(0);
    anyhow::ensure!(ok, "wayfrost extension reported a capture failure");

    let data: Vec<u8> = reply.child_get(1);
    anyhow::ensure!(!data.is_empty(), "wayfrost extension returned an empty image");

    let img = image::load_from_memory(&data).context("failed to decode PNG from extension")?;
    log::info!("Screen captured via Wayfrost GNOME extension (bytes over D-Bus)");
    Ok(img)
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
