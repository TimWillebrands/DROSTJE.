use gpui_kit::Rgba;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Oklch {
    pub l: f32,
    pub c: f32,
    pub h: f32,
}

fn to_linear(channel: f32) -> f32 {
    if channel <= 0.04045 {
        channel / 12.92
    } else {
        ((channel + 0.055) / 1.055).powf(2.4)
    }
}

fn from_linear(channel: f32) -> f32 {
    if channel <= 0.003_130_8 {
        channel * 12.92
    } else {
        1.055 * channel.powf(1. / 2.4) - 0.055
    }
}

impl Oklch {
    pub fn from_rgba(color: Rgba) -> Self {
        let (r, g, b) = (to_linear(color.r), to_linear(color.g), to_linear(color.b));
        let l = (0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
        let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
        let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
        let lightness = 0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s;
        let a = 1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s;
        let b = 0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s;
        Self {
            l: lightness,
            c: a.hypot(b),
            h: b.atan2(a).to_degrees().rem_euclid(360.),
        }
    }

    fn linear(self) -> [f32; 3] {
        let (sin, cos) = self.h.to_radians().sin_cos();
        let (a, b) = (self.c * cos, self.c * sin);
        let l = (self.l + 0.396_337_78 * a + 0.215_803_76 * b).powi(3);
        let m = (self.l - 0.105_561_346 * a - 0.063_854_17 * b).powi(3);
        let s = (self.l - 0.089_484_18 * a - 1.291_485_5 * b).powi(3);
        [
            4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s,
            -1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s,
            -0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s,
        ]
    }

    fn in_gamut(self) -> bool {
        const EPSILON: f32 = 1e-4;
        self.linear()
            .iter()
            .all(|channel| (-EPSILON..=1. + EPSILON).contains(channel))
    }

    pub fn to_rgba(self, alpha: f32) -> Rgba {
        let mut color = Self {
            l: self.l.clamp(0., 1.),
            c: self.c.max(0.),
            h: self.h.rem_euclid(360.),
        };
        if !color.in_gamut() {
            let (mut low, mut high) = (0., color.c);
            for _ in 0..24 {
                let mid = (low + high) / 2.;
                if (Self { c: mid, ..color }).in_gamut() {
                    low = mid;
                } else {
                    high = mid;
                }
            }
            color.c = low;
        }
        let [r, g, b] = color
            .linear()
            .map(|channel| from_linear(channel).clamp(0., 1.));
        Rgba { r, g, b, a: alpha }
    }
}

pub fn hex(color: Rgba) -> String {
    let byte = |channel: f32| (channel.clamp(0., 1.) * 255.).round() as u8;
    let rgb = format!(
        "#{:02x}{:02x}{:02x}",
        byte(color.r),
        byte(color.g),
        byte(color.b)
    );
    if byte(color.a) == 255 {
        rgb
    } else {
        format!("{rgb}{:02x}", byte(color.a))
    }
}

#[cfg(test)]
mod tests {
    use super::{Oklch, hex};
    use gpui_kit::Rgba;

    fn rgba(hex: &str) -> Rgba {
        Rgba::try_from(hex).unwrap()
    }

    #[test]
    fn round_trips_theme_colors() {
        for color in [
            "#2453a6", "#8aaceb", "#f7f9fc", "#1b2740", "#c07f00", "#000000",
        ] {
            let oklch = Oklch::from_rgba(rgba(color));
            assert_eq!(hex(oklch.to_rgba(1.)), color, "{oklch:?}");
        }
    }

    #[test]
    fn matches_reference_values() {
        let white = Oklch::from_rgba(rgba("#ffffff"));
        assert!((white.l - 1.).abs() < 1e-3 && white.c < 1e-3, "{white:?}");
        let red = Oklch::from_rgba(rgba("#ff0000"));
        assert!((red.l - 0.628).abs() < 1e-3, "{red:?}");
        assert!((red.c - 0.258).abs() < 1e-3, "{red:?}");
        assert!((red.h - 29.23).abs() < 0.1, "{red:?}");
    }

    #[test]
    fn out_of_gamut_colors_keep_lightness_and_hue() {
        let wanted = Oklch {
            l: 0.45,
            c: 0.4,
            h: 140.,
        };
        let fitted = Oklch::from_rgba(wanted.to_rgba(1.));
        assert!((fitted.l - wanted.l).abs() < 2e-3, "{fitted:?}");
        assert!((fitted.h - wanted.h).abs() < 1., "{fitted:?}");
        assert!(fitted.c < wanted.c);
    }

    #[test]
    fn hex_keeps_translucency() {
        assert_eq!(hex(rgba("#2453a680")), "#2453a680");
        assert_eq!(hex(rgba("#2453a6")), "#2453a6");
    }
}
