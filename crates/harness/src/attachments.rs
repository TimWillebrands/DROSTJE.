use anyhow::{Context, Result, bail, ensure};
use drostje_model::ImageAttachment;
use image::{DynamicImage, ImageFormat, ImageReader, Rgb, RgbImage, imageops::FilterType};
use std::{
    fs::{self, OpenOptions},
    io::{Cursor, Read, Write},
    path::Path,
};
use uuid::Uuid;

pub const FILE_LIMIT: u64 = 20 * 1024 * 1024;
pub const PIXEL_LIMIT: u64 = 32_000_000;
pub const FRAME_LIMIT: u64 = 1_048_576;

pub fn decode(bytes: &[u8]) -> Result<(DynamicImage, &'static str, &'static str)> {
    ensure!(bytes.len() as u64 <= FILE_LIMIT, "Image exceeds 20 MiB");
    let format = image::guess_format(bytes).context("Unrecognized image bytes")?;
    let (mime, extension) = match format {
        ImageFormat::Png => ("image/png", "png"),
        ImageFormat::Jpeg => ("image/jpeg", "jpg"),
        ImageFormat::WebP => ("image/webp", "webp"),
        _ => bail!("Only PNG, JPEG and WebP images are supported"),
    };
    let (width, height) = ImageReader::with_format(Cursor::new(bytes), format).into_dimensions()?;
    ensure!(
        u64::from(width) * u64::from(height) <= PIXEL_LIMIT,
        "Image exceeds 32 megapixels"
    );
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    Ok((reader.decode().context("Decode image")?, mime, extension))
}
pub fn read_original(path: &Path) -> Result<Vec<u8>> {
    let file = fs::File::open(path).with_context(|| format!("Open image {}", path.display()))?;
    ensure!(
        file.metadata()?.len() <= FILE_LIMIT,
        "Selected file exceeds 20 MiB"
    );
    let mut bytes = Vec::new();
    file.take(FILE_LIMIT + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= FILE_LIMIT,
        "Selected file exceeds 20 MiB"
    );
    Ok(bytes)
}
fn own(
    bytes: &[u8],
    image: &DynamicImage,
    mime: &str,
    ext: &str,
    data: &Path,
    original: Option<std::path::PathBuf>,
) -> Result<ImageAttachment> {
    let directory = data.join("attachments");
    fs::create_dir_all(&directory)?;
    let id = Uuid::new_v4();
    let path = directory.join(format!("{id}.{ext}"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(ImageAttachment {
        id,
        path,
        mime: mime.into(),
        width: image.width(),
        height: image.height(),
        original,
    })
}
pub fn import(path: &Path, data: &Path) -> Result<ImageAttachment> {
    let bytes = read_original(path)?;
    let (image, mime, extension) = decode(&bytes)?;
    own(&bytes, &image, mime, extension, data, None)
}
pub fn import_bytes(bytes: &[u8], declared_mime: &str, data: &Path) -> Result<ImageAttachment> {
    let (image, mime, extension) = decode(bytes)?;
    ensure!(
        mime == declared_mime,
        "Image bytes do not match the declared MIME type {declared_mime}"
    );
    own(bytes, &image, mime, extension, data, None)
}
/// Clipboard images have no source file. PNG, JPEG and WebP are stored as
/// pasted; TIFF (what macOS offers for many copied bitmaps) is re-encoded as
/// PNG so the stored copy stays within the formats the harness accepts.
pub fn import_pasted(bytes: &[u8], data: &Path) -> Result<ImageAttachment> {
    ensure!(
        bytes.len() as u64 <= FILE_LIMIT,
        "Pasted image exceeds 20 MiB"
    );
    let format =
        image::guess_format(bytes).context("Clipboard bytes are not a recognized image")?;
    match format {
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP => {
            let (image, mime, extension) = decode(bytes)?;
            own(bytes, &image, mime, extension, data, None)
        }
        ImageFormat::Tiff => {
            let (width, height) =
                ImageReader::with_format(Cursor::new(bytes), format).into_dimensions()?;
            ensure!(
                u64::from(width) * u64::from(height) <= PIXEL_LIMIT,
                "Pasted image exceeds 32 megapixels"
            );
            let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
            let mut limits = image::Limits::default();
            limits.max_alloc = Some(256 * 1024 * 1024);
            reader.limits(limits);
            let tiff = reader.decode().context("Decode pasted TIFF")?;
            let mut png = Vec::new();
            tiff.write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
                .context("Encode pasted TIFF as PNG")?;
            let (image, mime, extension) = decode(&png)?;
            own(&png, &image, mime, extension, data, None)
        }
        _ => bail!("Only PNG, JPEG, WebP and TIFF images can be pasted"),
    }
}
fn flatten(image: DynamicImage) -> DynamicImage {
    let rgba = image.to_rgba8();
    let mut rgb = RgbImage::new(rgba.width(), rgba.height());
    for (out, input) in rgb.pixels_mut().zip(rgba.pixels()) {
        let alpha = u32::from(input[3]);
        *out = Rgb(std::array::from_fn(|i| {
            ((u32::from(input[i]) * alpha + 255 * (255 - alpha) + 127) / 255) as u8
        }));
    }
    DynamicImage::ImageRgb8(rgb)
}
/// Explicit user action only. Originals remain untouched; returned files are the reviewed send copies.
pub fn optimize(
    request_id: Uuid,
    text: &str,
    images: &[ImageAttachment],
    data: &Path,
) -> Result<Vec<ImageAttachment>> {
    ensure!(!images.is_empty(), "No images to optimize");
    ensure!(
        crate::submission_size(request_id, text, &[])? <= FRAME_LIMIT,
        "Text alone exceeds the 1 MiB request budget; reduce the draft"
    );
    let originals = images
        .iter()
        .map(|attachment| {
            let path = attachment.original.as_ref().unwrap_or(&attachment.path);
            let (image, _, _) = decode(&read_original(path)?)?;
            Ok((flatten(image), path.clone()))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut side = 2048;
    loop {
        let mut encoded = Vec::with_capacity(originals.len());
        for (image, _) in &originals {
            let prepared = if image.width().max(image.height()) > side {
                image.resize(side, side, FilterType::Lanczos3)
            } else {
                image.clone()
            };
            let mut bytes = Vec::new();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 85)
                .encode_image(&prepared)?;
            encoded.push((prepared, bytes));
        }
        let lengths: Vec<_> = encoded
            .iter()
            .map(|(_, bytes)| ("image/jpeg", bytes.len() as u64))
            .collect();
        let size = crate::submission_size_for_lengths(request_id, text, &lengths)?;
        if size <= FRAME_LIMIT {
            return encoded
                .iter()
                .zip(&originals)
                .map(|((image, bytes), (_, path))| {
                    own(bytes, image, "image/jpeg", "jpg", data, Some(path.clone()))
                })
                .collect();
        }
        ensure!(
            side > 256,
            "Prepared images still exceed 1 MiB at 256px; remove images or reduce text"
        );
        side /= 2;
    }
}
