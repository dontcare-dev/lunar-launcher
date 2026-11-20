use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::io::Cursor;

#[derive(Clone, Debug, Deserialize)]
pub struct Skin {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, alias = "author", alias = "publisher")]
    pub creator: String,
    #[serde(default)]
    pub arms: String,
    #[serde(default)]
    pub page: String,
    #[serde(default)]
    pub png: String,
}

pub fn search(query: &str) -> Result<Vec<Skin>> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(concat!("LUNAR/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(25))
        .build()?;
    let response = client.get("https://mosaicary.com/api/skins")
        .query(&[
            ("q", query),
            ("size", "64"),
            ("source", "published"),
            ("sort", "popular"),
            ("page", "1"),
            ("per", "24"),
        ])
        .send()?.error_for_status()?;
    let value: serde_json::Value = response.json()?;
    let entries = value.as_array()
        .or_else(|| value.get("skins").and_then(serde_json::Value::as_array))
        .or_else(|| value.get("data").and_then(|data| data.as_array()))
        .or_else(|| value.pointer("/data/skins").and_then(serde_json::Value::as_array))
        .context("Skin catalogue returned an unexpected response")?;
    let mut skins = Vec::new();
    for entry in entries {
        let Ok(mut skin) = serde_json::from_value::<Skin>(entry.clone()) else { continue };
        if skin.id.is_empty() { continue; }
        if skin.creator.is_empty() {
            skin.creator = entry.pointer("/author/name").or_else(|| entry.pointer("/publisher/name"))
                .and_then(serde_json::Value::as_str).unwrap_or("Community creator").to_owned();
        }
        if skin.page.is_empty() {
            skin.page = format!("https://mosaicary.com/skins/{}", skin.id);
        }
        if skin.png.is_empty() {
            skin.png = format!("https://mosaicary.com/api/skins/{}.png", skin.id);
        }
        if skin.name.is_empty() { skin.name = format!("Community skin {}", skin.id); }
        skins.push(skin);
    }
    Ok(skins)
}

pub fn download(skin: &Skin) -> Result<image::RgbaImage> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(concat!("LUNAR/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(25))
        .build()?;
    let response = client.get(&skin.png).send()?.error_for_status()?;
    let bytes = response.bytes()?;
    if bytes.is_empty() || bytes.len() > 8 * 1024 * 1024 {
        bail!("Skin image is empty or larger than 8 MB");
    }
    let image = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
        .context("Catalogue did not return a valid PNG skin")?
        .to_rgba8();
    Ok(image)
}
