use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::constants::{
    CACHE_DIR_NAME, DET_MODEL_FILENAME, DET_MODEL_URL, KEYS_DICT_FILENAME, KEYS_DICT_URL,
    MODELS_SUBDIR, REC_MODEL_FILENAME, REC_MODEL_URL,
};

#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct ModelPaths {
    pub det_model: PathBuf,
    pub rec_model: PathBuf,
    pub dict_file: PathBuf,
}

pub fn get_models_dir() -> Result<PathBuf> {
    let xdg_dirs = xdg::BaseDirectories::with_prefix(CACHE_DIR_NAME)?;
    let dir = xdg_dirs.get_data_home().join(MODELS_SUBDIR);
    fs::create_dir_all(&dir).context("Failed to create models directory")?;
    Ok(dir)
}

fn download_if_missing(url: &str, target: &Path) -> Result<()> {
    if target.exists() && fs::metadata(target)?.len() > 1024 {
        log::info!("Model exists: {:?}", target);
        return Ok(());
    }

    log::info!("Downloading {} -> {:?}", url, target);
    let tmp_target = target.with_extension("download");
    let status = Command::new("curl")
        .arg("-sL")
        .arg("--fail")
        .arg(url)
        .arg("-o")
        .arg(&tmp_target)
        .status()
        .context("Failed to invoke curl for downloading model")?;

    if !status.success() {
        let _ = fs::remove_file(&tmp_target);
        anyhow::bail!("Failed to download {} (curl status: {:?})", url, status);
    }

    fs::rename(&tmp_target, target).context("Failed to finalize downloaded model file")?;
    log::info!("Downloaded successfully: {:?}", target);
    Ok(())
}

pub fn ensure_models() -> Result<ModelPaths> {
    let models_dir = get_models_dir()?;
    let det_model = models_dir.join(DET_MODEL_FILENAME);
    let rec_model = models_dir.join(REC_MODEL_FILENAME);
    let dict_file = models_dir.join(KEYS_DICT_FILENAME);

    download_if_missing(DET_MODEL_URL, &det_model)?;
    download_if_missing(REC_MODEL_URL, &rec_model)?;
    download_if_missing(KEYS_DICT_URL, &dict_file)?;

    let tessdata_dir = models_dir.join("tessdata");
    let _ = fs::create_dir_all(&tessdata_dir);
    let tur_trained = tessdata_dir.join("tur.traineddata");
    let eng_trained = tessdata_dir.join("eng.traineddata");
    let _ = download_if_missing("https://github.com/tesseract-ocr/tessdata_best/raw/main/tur.traineddata", &tur_trained);
    let _ = download_if_missing("https://github.com/tesseract-ocr/tessdata_best/raw/main/eng.traineddata", &eng_trained);
    let sys_configs = Path::new("/usr/share/tessdata/configs");
    let target_configs = tessdata_dir.join("configs");
    if sys_configs.exists() && !target_configs.exists() {
        let _ = Command::new("cp").arg("-r").arg(sys_configs).arg(&target_configs).status();
    }

    Ok(ModelPaths {
        det_model,
        rec_model,
        dict_file,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ensure_models() {
        let paths = ensure_models().expect("Failed to ensure models");
        assert!(paths.det_model.exists());
        assert!(paths.rec_model.exists());
        assert!(paths.dict_file.exists());
    }
}
