use anyhow::{Context, Result, ensure};
use image::{ImageFormat, RgbaImage, imageops};

/// The installed desktop icon and embedded window/tray icons share one source.
pub fn render(size: u32) -> Result<RgbaImage> {
    ensure!(size > 0, "Invalid icon dimensions");
    let source = image::load_from_memory_with_format(
        include_bytes!("../../../packaging/clash-oxide.png"),
        ImageFormat::Png,
    )
    .context("Failed to decode application icon")?;
    let scaled = source
        .resize(size, size, imageops::FilterType::Lanczos3)
        .to_rgba8();
    let mut icon = RgbaImage::new(size, size);
    // Center the non-square source on a transparent square without stretching.
    // PNG decoding provides the straight RGBA expected by GPUI and tray-icon.
    imageops::overlay(
        &mut icon,
        &scaled,
        i64::from((size - scaled.width()) / 2),
        i64::from((size - scaled.height()) / 2),
    );
    Ok(icon)
}
