use anyhow::{Context, Result};
use std::io::Write;
use std::process::{Command, Stdio};

/// Copies text to the Wayland clipboard via wl-copy.
pub fn copy_to_clipboard(text: &str) -> Result<()> {
    let mut child = Command::new("wl-copy")
        .stdin(Stdio::piped())
        .spawn()
        .context("Failed to spawn wl-copy")?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(text.as_bytes())?;
    }

    let status = child.wait()?;
    if !status.success() {
        anyhow::bail!("wl-copy exited with error: {:?}", status);
    }
    Ok(())
}

/// Sends a desktop notification via notify-send.
pub fn send_notification(summary: &str, body: &str) {
    let _ = Command::new("notify-send")
        .arg("-a")
        .arg("Wayfrost")
        .arg("-i")
        .arg("edit-copy-symbolic")
        .arg(summary)
        .arg(body)
        .spawn();
}
