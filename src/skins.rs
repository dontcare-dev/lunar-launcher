use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct Skin {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(skip)]
    pub creator: String,
    #[serde(default)]
    pub arms: String,
    #[serde(default)]
    pub page: String,
    #[serde(default)]
    pub png: String,
    #[serde(skip)]
    pub preview_rgba: Option<(usize, usize, Vec<u8>)>,
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
    let entries = value.get("skins").and_then(serde_json::Value::as_array)
        .context("Skin catalogue returned an unexpected response")?;
    let mut skins = Vec::new();
    for entry in entries {
        let Ok(mut skin) = serde_json::from_value::<Skin>(entry.clone()) else { continue };
        if skin.id.is_empty() { continue; }
        skin.creator = entry.pointer("/author/name")
            .or_else(|| entry.pointer("/publisher/name"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Community creator").to_owned();
        if skin.page.is_empty() {
            skin.page = format!("https://mosaicary.com/models/{}", skin.id);
        }
        if skin.png.is_empty() {
            skin.png = format!("https://mosaicary.com/api/skins/{}.png", skin.id);
        }
        if skin.name.is_empty() { skin.name = format!("Community skin {}", skin.id); }
        if let Some(preview_url) = entry.get("preview").and_then(serde_json::Value::as_str)
            && let Ok(response) = client.get(preview_url).send().and_then(reqwest::blocking::Response::error_for_status)
            && let Ok(bytes) = response.bytes()
            && let Ok(decoded) = image::load_from_memory(&bytes)
        {
            let preview = decoded.resize(64, 96, image::imageops::FilterType::Nearest).to_rgba8();
            skin.preview_rgba = Some((preview.width() as usize, preview.height() as usize, preview.into_raw()));
        }
        skins.push(skin);
    }
    Ok(skins)
}

pub fn validate_skin(image: &image::RgbaImage) -> Result<()> {
    let (width, height) = image.dimensions();
    if !((width == 64 && height == 64) || (width == 64 && height == 32)) {
        bail!("Unsupported skin dimensions: {width}×{height}. Use a 64×64 or 64×32 Minecraft skin.");
    }
    Ok(())
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
    validate_skin(&image)?;
    Ok(image)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_supported_skin_layouts() {
        assert!(validate_skin(&image::RgbaImage::new(64, 64)).is_ok());
        assert!(validate_skin(&image::RgbaImage::new(64, 32)).is_ok());
    }

    #[test]
    fn rejects_unsupported_skin_dimensions() {
        assert!(validate_skin(&image::RgbaImage::new(128, 128)).is_err());
        assert!(validate_skin(&image::RgbaImage::new(64, 128)).is_err());
    }
}
