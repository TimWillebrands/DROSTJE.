use qrcodegen::{QrCode, QrCodeEcc};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QrMatrix {
    size: usize,
    modules: Vec<bool>,
}

impl QrMatrix {
    pub fn encode(text: &str) -> Option<Self> {
        let code = QrCode::encode_text(text, QrCodeEcc::Medium).ok()?;
        let size = usize::try_from(code.size()).ok()?;
        let modules = (0..size)
            .flat_map(|y| (0..size).map(move |x| (x, y)))
            .map(|(x, y)| code.get_module(x as i32, y as i32))
            .collect();
        Some(Self { size, modules })
    }

    pub fn size(&self) -> usize {
        self.size
    }

    pub fn dark(&self, x: usize, y: usize) -> bool {
        x < self.size && y < self.size && self.modules[y * self.size + x]
    }
}

#[cfg(feature = "scan")]
pub struct LumaFrame<'a> {
    pub width: usize,
    pub height: usize,
    pub row_stride: usize,
    pub pixels: &'a [u8],
}

#[cfg(feature = "scan")]
pub fn decode_frame(frame: &LumaFrame<'_>) -> Vec<String> {
    let LumaFrame {
        width,
        height,
        row_stride,
        pixels,
    } = *frame;
    if width == 0 || height == 0 || row_stride < width {
        return Vec::new();
    }
    let needed = row_stride
        .checked_mul(height - 1)
        .and_then(|rows| rows.checked_add(width));
    if needed.is_none_or(|needed| pixels.len() < needed) {
        return Vec::new();
    }
    let mut image = rqrr::PreparedImage::prepare_from_greyscale(width, height, |x, y| {
        pixels[y * row_stride + x]
    });
    image
        .detect_grids()
        .into_iter()
        .filter_map(|grid| grid.decode().ok().map(|(_, text)| text))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(
        matrix: &QrMatrix,
        scale: usize,
        quiet: usize,
        stride_pad: usize,
    ) -> (usize, usize, Vec<u8>) {
        let side = (matrix.size() + quiet * 2) * scale;
        let stride = side + stride_pad;
        let mut pixels = vec![255u8; stride * side];
        for y in 0..side {
            for x in 0..side {
                let module_x = (x / scale).checked_sub(quiet);
                let module_y = (y / scale).checked_sub(quiet);
                if let (Some(mx), Some(my)) = (module_x, module_y)
                    && matrix.dark(mx, my)
                {
                    pixels[y * stride + x] = 20;
                }
            }
        }
        (side, stride, pixels)
    }

    #[test]
    fn a_pairing_link_encodes_into_a_square_matrix() {
        let link = crate::DesktopPairing::new("https://seintje.example.org".into(), None)
            .offer
            .to_link();
        let matrix = QrMatrix::encode(&link).unwrap();
        assert!(matrix.size() >= 21 && matrix.size() <= 177);
        assert!(matrix.dark(0, 0));
        assert!(!matrix.dark(matrix.size(), 0));
    }

    #[test]
    fn a_camera_frame_of_the_desktop_code_decodes_back_to_the_link() {
        let link =
            crate::DesktopPairing::new("https://seintje.example.org".into(), Some("tk_x".into()))
                .offer
                .to_link();
        let matrix = QrMatrix::encode(&link).unwrap();
        let (side, stride, pixels) = render(&matrix, 4, 4, 16);
        let frame = LumaFrame {
            width: side,
            height: side,
            row_stride: stride,
            pixels: &pixels,
        };
        assert_eq!(decode_frame(&frame), vec![link]);
    }

    #[test]
    fn frames_without_a_code_or_with_bad_geometry_decode_to_nothing() {
        let blank = vec![200u8; 64 * 64];
        let frame = LumaFrame {
            width: 64,
            height: 64,
            row_stride: 64,
            pixels: &blank,
        };
        assert!(decode_frame(&frame).is_empty());
        let short = LumaFrame {
            width: 64,
            height: 64,
            row_stride: 64,
            pixels: &blank[..100],
        };
        assert!(decode_frame(&short).is_empty());
        let narrow = LumaFrame {
            width: 64,
            height: 64,
            row_stride: 10,
            pixels: &blank,
        };
        assert!(decode_frame(&narrow).is_empty());
    }
}
