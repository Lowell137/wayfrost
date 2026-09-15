use anyhow::{Context, Result};
use ashpd::desktop::screenshot::Screenshot;
use image::DynamicImage;
use std::process::Command;

/// Captures the full Wayland screen.
/// 1. Tries `grim` (direct pipe into memory on wlroots / Hyprland / Sway).
/// 2. Falls back to XDG Desktop Portal (`ashpd`) for GNOME / KDE Wayland.
pub fn capture_screen() -> Result<DynamicImage> {
    // Try fast native grim first
    if let Ok(output) = Command::new("grim").arg("-").output() {
        if output.status.success() {
            if let Ok(img) = image::load_from_memory(&output.stdout) {
                log::info!("Screen captured via `grim`");
                return Ok(img);
            }
        }
    }

    log::info!("`grim` not available or failed, falling back to XDG Desktop Portal");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("Failed to create tokio runtime for portal capture")?;

    runtime.block_on(async {
        let response = Screenshot::request()
            .interactive(false)
            .modal(false)
            .send()
            .await
            .context("Portal screenshot request failed")?
            .response()
            .context("Portal screenshot response failed")?;

        let uri = response.uri();
        let path = uri
            .to_file_path()
            .map_err(|_| anyhow::anyhow!("Invalid file URI: {}", uri))?;

        let img = image::open(&path).context("Failed to open portal screenshot image")?;
        let _ = std::fs::remove_file(&path);
        Ok(img)
    })
}
