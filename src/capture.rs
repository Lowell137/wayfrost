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

/// XDG Desktop Portal Screenshot (interactive) — the extension-free fallback.
/// Calls org.freedesktop.portal.Screenshot.Screenshot with interactive:true, then
/// waits for the async org.freedesktop.portal.Request::Response signal on a nested
/// main loop and reads the returned file URI. GNOME ignores interactive:false
/// (returns "cancelled"), so this shows the desktop's own region picker.
fn capture_via_portal() -> Result<DynamicImage> {
    use gtk4::gio::{self, DBusCallFlags, DBusSignalFlags};
    use glib::prelude::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    let conn = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)
        .context("no session D-Bus connection")?;

    let mut options: HashMap<String, glib::Variant> = HashMap::new();
    options.insert("interactive".to_string(), glib::Variant::from(true));
    // (s parent_window, a{sv} options)
    let params = (String::new(), options).to_variant();

    // Kick off the request; the reply is the Request object path.
    let reply = conn
        .call_sync(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Screenshot",
            "Screenshot",
            Some(&params),
            glib::VariantTy::new("(o)").ok(),
            DBusCallFlags::NONE,
            5000,
            gio::Cancellable::NONE,
        )
        .context("portal Screenshot call failed (no portal backend?)")?;
    let request_path: String = reply.child_get(0);

    // Wait for Request::Response(u, a{sv}) on that path via a nested main loop.
    let loop_ = glib::MainLoop::new(None, false);
    let outcome: Arc<Mutex<Option<(u32, String)>>> = Arc::new(Mutex::new(None));

    let cb_loop = loop_.clone();
    let cb_out = outcome.clone();
    let sub = conn.signal_subscribe(
        Some("org.freedesktop.portal.Desktop"),
        Some("org.freedesktop.portal.Request"),
        Some("Response"),
        Some(&request_path),
        None,
        DBusSignalFlags::NONE,
        move |_c, _s, _p, _i, _m, parameters| {
            let response: u32 = parameters.child_get(0);
            let results: glib::Variant = parameters.child_get(1);
            let map: HashMap<String, glib::Variant> = results.get().unwrap_or_default();
            let uri = map
                .get("uri")
                .and_then(|v| v.get::<String>())
                .unwrap_or_default();
            *cb_out.lock().unwrap() = Some((response, uri));
            cb_loop.quit();
        },
    );

    // Safety net so a never-responding portal can't hang the main thread forever.
    let to_loop = loop_.clone();
    let to_out = outcome.clone();
    glib::timeout_add_local(Duration::from_secs(60), move || {
        if to_out.lock().unwrap().is_none() {
            log::warn!("portal screenshot timed out after 60s");
            to_loop.quit();
        }
        glib::ControlFlow::Break
    });

    loop_.run();
    conn.signal_unsubscribe(sub);

    let (response, uri) = outcome
        .lock()
        .unwrap()
        .take()
        .context("portal screenshot produced no response")?;
    anyhow::ensure!(
        response == 0,
        "portal screenshot cancelled or denied (code {response})"
    );

    let path = uri.strip_prefix("file://").unwrap_or(&uri).to_string();
    anyhow::ensure!(!path.is_empty(), "portal returned an empty file uri");
    let img = image::open(&path).with_context(|| format!("cannot open portal screenshot: {path}"))?;
    let _ = std::fs::remove_file(&path);
    log::info!("Screen captured via XDG Desktop Portal (interactive)");
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
