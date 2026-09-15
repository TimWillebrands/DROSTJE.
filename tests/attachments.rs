use drost::attachments;
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use std::{fs, path::PathBuf};
use uuid::Uuid;

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("drost-images-{}", Uuid::new_v4()));
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
        &[attachment.clone()],
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
        drost::harness::submission_size(request_id, "Describe this", &prepared).unwrap()
            <= attachments::FRAME_LIMIT
    );
}
