use anyhow::{Context, Result};
use std::process::Command;

pub fn is_ollama_installed() -> bool {
    Command::new("which").arg("ollama").status().is_ok()
}

pub fn install_ollama() -> Result<()> {
    // Uses pkexec to prompt for password via GNOME popup
    let status = Command::new("pkexec")
        .args(["bash", "-c", "curl -fsSL https://ollama.com/install.sh | sh"])
        .status()
        .context("Failed to trigger Ollama installer via pkexec")?;
    
    anyhow::ensure!(status.success(), "Ollama installation failed");
    Ok(())
}

pub fn get_available_models() -> Vec<String> {
    // Simplistic mock-up: In real use, parse `ollama list`
    vec!["llava:7b".to_string(), "moondream:latest".to_string()]
}
