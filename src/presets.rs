use std::{fs, path::Path};

use anyhow::{Context, Result};

use crate::models::{ControlDescriptor, PresetControlValue, PresetFile};

pub fn to_preset(card_name: &str, controls: &[ControlDescriptor]) -> PresetFile {
    PresetFile {
        schema_version: 1,
        card_name: card_name.to_string(),
        controls: controls
            .iter()
            .map(|c| PresetControlValue {
                numid: c.numid,
                values: c.values.clone(),
            })
            .collect(),
    }
}

pub fn save_preset(path: &Path, preset: &PresetFile) -> Result<()> {
    let text = serde_json::to_string_pretty(preset)?;
    fs::write(path, text).with_context(|| format!("Failed to write preset {:?}", path))?;
    Ok(())
}

pub fn load_preset(path: &Path) -> Result<PresetFile> {
    let text = fs::read_to_string(path).with_context(|| format!("Failed to read preset {:?}", path))?;
    let preset = serde_json::from_str::<PresetFile>(&text)?;
    Ok(preset)
}
