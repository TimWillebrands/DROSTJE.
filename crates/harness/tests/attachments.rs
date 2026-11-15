use drostje_harness::attachments;
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use std::{fs, path::PathBuf};
use uuid::Uuid;

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("drostje-images-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn accepted_image_is_decoded_from_bytes_and_survives_source_removal() {
    let temp = Temp::new();
    let source = temp.0.join("misleading.jpg");
    let image = RgbaImage::from_pixel(3, 2, Rgba([12, 34, 56, 255]));
    image.save_with_format(&source, ImageFormat::Png).unwrap();
    let accepted = attachments::import(&source, &temp.0.join("data")).unwrap();
    fs::remove_file(source).unwrap();
    let (decoded, mime, _) = attachments::decode(&fs::read(accepted.path).unwrap()).unwrap();
    assert_eq!(mime, "image/png");
    assert_eq!(decoded.to_rgba8(), image);
}

#[test]
fn explicit_optimization_retains_original_and_flattens_transparency() {
    let temp = Temp::new();
    let source = temp.0.join("source.png");
    let original = DynamicImage::ImageRgba8(RgbaImage::from_pixel(64, 32, Rgba([255, 0, 0, 0])));
    original
        .save_with_format(&source, ImageFormat::Png)
        .unwrap();
    let attachment = attachments::import(&source, &temp.0.join("data")).unwrap();
    let original_bytes = fs::read(&attachment.path).unwrap();
    let request_id = Uuid::new_v4();
    let prepared = attachments::optimize(
        request_id,
        "Describe this",
        std::slice::from_ref(&attachment),
        &temp.0.join("data"),
    )
    .unwrap();
    let (decoded, mime, _) = attachments::decode(&fs::read(&prepared[0].path).unwrap()).unwrap();
    assert_eq!(mime, "image/jpeg");
    assert!(
        decoded
            .to_rgb8()
            .pixels()
            .all(|p| p.0.iter().all(|v| *v >= 250))
    );
    assert_eq!(fs::read(&attachment.path).unwrap(), original_bytes);
    assert!(
        drostje_harness::submission_size(request_id, "Describe this", &prepared).unwrap()
            <= attachments::FRAME_LIMIT
    );
}

#[test]
fn pasted_tiff_is_stored_as_png_and_pasted_png_keeps_its_bytes() {
    let temp = Temp::new();
    let image = RgbaImage::from_pixel(4, 3, Rgba([200, 100, 50, 255]));
    let mut tiff = Vec::new();
    DynamicImage::ImageRgba8(image.clone())
        .write_to(&mut std::io::Cursor::new(&mut tiff), ImageFormat::Tiff)
        .unwrap();
    let pasted = attachments::import_pasted(&tiff, &temp.0.join("data")).unwrap();
    assert_eq!(pasted.mime, "image/png");
    assert_eq!((pasted.width, pasted.height), (4, 3));
    assert!(pasted.original.is_none());
    let (decoded, mime, _) = attachments::decode(&fs::read(&pasted.path).unwrap()).unwrap();
    assert_eq!(mime, "image/png");
    assert_eq!(decoded.to_rgba8(), image);

    let mut png = Vec::new();
    DynamicImage::ImageRgba8(image)
        .write_to(&mut std::io::Cursor::new(&mut png), ImageFormat::Png)
        .unwrap();
    let pasted = attachments::import_pasted(&png, &temp.0.join("data")).unwrap();
    assert_eq!(pasted.mime, "image/png");
    assert_eq!(fs::read(&pasted.path).unwrap(), png);
}

#[test]
fn pasted_bytes_outside_the_supported_formats_are_rejected() {
    let temp = Temp::new();
    let gif = b"GIF89a\x01\x00\x01\x00\x00\x00\x00;";
    let error = attachments::import_pasted(gif, &temp.0.join("data")).unwrap_err();
    assert!(error.to_string().contains("Only PNG, JPEG, WebP and TIFF"));
    let error = attachments::import_pasted(b"not an image", &temp.0.join("data")).unwrap_err();
    assert!(error.to_string().contains("not a recognized image"));
    assert!(!temp.0.join("data").join("attachments").exists());
}
