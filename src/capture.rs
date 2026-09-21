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

    // 2. XDG Desktop Portal, SILENT (interactive:false) — the extension-free,
    // picker-free path. Works on recent GNOME (49+); older GNOME returns code 2,
    // which is why an interactive fallback exists at the end of this chain.
    if let Ok(img) = capture_via_portal(false) {
        return Ok(img);
    }

    // 3. Wayfrost GNOME extension (instant, silent) — fallback when the portal is
    // unavailable or the caller had no focused surface.
    if let Ok(img) = capture_via_gnome_extension() {
        return Ok(img);
    }

    // 4. GNOME Screenshot (reliable in GNOME / VMs)
    if let Ok(img) = capture_via_gnome_screenshot() {
        return Ok(img);
    }

    // 5. ImageMagick import (universal fallback for X11/XWayland/VMs)
    if let Ok(img) = capture_via_imagemagick() {
        return Ok(img);
    }

    // 6. GUARANTEED fallback: portal interactive (region picker). Works on every
    // GNOME version without an extension — silent capture failed above, so at
    // least let the user pick a region and actually get text out.
    log::info!("No silent backend available; falling back to portal region picker");
    if let Ok(img) = capture_via_portal(true) {
        return Ok(img);
    }

    Err(anyhow::anyhow!(
        "no screen-capture backend worked (portal silent + extension + gnome-screenshot + imagemagick + portal picker all failed)"
    ))
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

/// Extension-free capture via the XDG Desktop Portal.
/// `interactive=false` (silent, full-screen) works only on recent GNOME (49+);
/// older GNOME (46/48) returns code 2 for it. `interactive=true` pops the
/// desktop's region picker but works on every GNOME version — used as the
/// guaranteed fallback so capture never dead-ends. The response is a `file://`
/// URI GNOME writes into ~/Pictures/Screenshots, readable via xdg-pictures.
fn capture_via_portal(interactive: bool) -> Result<DynamicImage> {
    use gtk4::gio::{self, DBusCallFlags, DBusSignalFlags};
    use glib::prelude::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    let conn = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)
        .context("no session D-Bus connection")?;

    let mut options: HashMap<String, glib::Variant> = HashMap::new();
    options.insert("interactive".to_string(), glib::Variant::from(interactive));
    let params = (String::new(), options).to_variant(); // (s parent_window, a{sv})

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

    // Await Request::Response(u response, a{sv} results) on that object path.
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
            let results: HashMap<String, glib::Variant> = parameters.child_get(1);
            let uri = results
                .get("uri")
                .and_then(|v| v.str().map(str::to_string))
                .unwrap_or_default();
            *cb_out.lock().unwrap() = Some((response, uri));
            cb_loop.quit();
        },
    );

    let to_loop = loop_.clone();
    let to_out = outcome.clone();
    let secs = if interactive { 120 } else { 20 };
    glib::timeout_add_local(Duration::from_secs(secs), move || {
        if to_out.lock().unwrap().is_none() {
            log::warn!("portal screenshot timed out after {secs}s");
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
    anyhow::ensure!(!uri.is_empty(), "portal returned an empty uri");

    // The URI is percent-encoded (GNOME uses spaces in filenames). Decode it.
    let (path, _frag) =
        glib::filename_from_uri(&uri).context("portal returned a non-local file uri")?;
    let img = image::open(&path)
        .with_context(|| format!("cannot open portal screenshot: {}", path.display()))?;
    let _ = std::fs::remove_file(&path);
    log::info!(
        "Screen captured via XDG Desktop Portal ({})",
        if interactive { "interactive picker" } else { "silent, extension-free" }
    );
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
