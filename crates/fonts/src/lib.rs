pub struct Face {
    pub family: &'static str,
    pub file: &'static str,
    pub weight: u16,
    pub italic: bool,
    pub bytes: &'static [u8],
}

macro_rules! face {
    ($family:literal, $directory:literal, $file:literal, $weight:literal, $italic:literal) => {
        Face {
            family: $family,
            file: $file,
            weight: $weight,
            italic: $italic,
            bytes: include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../assets/fonts/",
                $directory,
                "/",
                $file
            )),
        }
    };
}

pub const FACES: [Face; 22] = [
    face!(
        "IBM Plex Sans",
        "ibm-plex-sans",
        "IBMPlexSans-Regular.ttf",
        400,
        false
    ),
    face!(
        "IBM Plex Sans",
        "ibm-plex-sans",
        "IBMPlexSans-Italic.ttf",
        400,
        true
    ),
    face!(
        "IBM Plex Sans",
        "ibm-plex-sans",
        "IBMPlexSans-Medium.ttf",
        500,
        false
    ),
    face!(
        "IBM Plex Sans",
        "ibm-plex-sans",
        "IBMPlexSans-SemiBold.ttf",
        600,
        false
    ),
    face!(
        "IBM Plex Sans",
        "ibm-plex-sans",
        "IBMPlexSans-Bold.ttf",
        700,
        false
    ),
    face!(
        "IBM Plex Sans",
        "ibm-plex-sans",
        "IBMPlexSans-BoldItalic.ttf",
        700,
        true
    ),
    face!("Lilex", "lilex", "Lilex-Regular.ttf", 400, false),
    face!("Lilex", "lilex", "Lilex-Italic.ttf", 400, true),
    face!("Lilex", "lilex", "Lilex-Bold.ttf", 700, false),
    face!("Libron", "libron", "Libron-Regular.ttf", 400, false),
    face!("Libron", "libron", "Libron-Italic.ttf", 400, true),
    face!("Libron", "libron", "Libron-Bold.ttf", 700, false),
    face!("Libron", "libron", "Libron-BoldItalic.ttf", 700, true),
    face!(
        "Monaspace Xenon",
        "monaspace-xenon",
        "MonaspaceXenon-Regular.otf",
        400,
        false
    ),
    face!(
        "Monaspace Xenon",
        "monaspace-xenon",
        "MonaspaceXenon-Italic.otf",
        400,
        true
    ),
    face!(
        "Monaspace Xenon",
        "monaspace-xenon",
        "MonaspaceXenon-Bold.otf",
        700,
        false
    ),
    face!("Dosis", "dosis", "Dosis-Medium.ttf", 500, false),
    face!("Dosis", "dosis", "Dosis-SemiBold.ttf", 600, false),
    face!("Dosis", "dosis", "Dosis-Bold.ttf", 700, false),
    face!(
        "Maple Mono NL",
        "maple-mono-nl",
        "MapleMonoNL-Regular.ttf",
        400,
        false
    ),
    face!(
        "Maple Mono NL",
        "maple-mono-nl",
        "MapleMonoNL-Italic.ttf",
        400,
        true
    ),
    face!(
        "Maple Mono NL",
        "maple-mono-nl",
        "MapleMonoNL-Bold.ttf",
        700,
        false
    ),
];

pub const ARTIFACT_FONT_ORIGIN: &str = "https://drostje-kit.invalid";
const ARTIFACT_FONT_PATH: &str = "/fonts/";

pub fn artifact_face(url: &url::Url) -> Option<&'static Face> {
    if url.origin().ascii_serialization() != ARTIFACT_FONT_ORIGIN {
        return None;
    }
    let file = url.path().strip_prefix(ARTIFACT_FONT_PATH)?;
    FACES.iter().find(|face| face.file == file)
}

pub fn artifact_font_faces(families: &[&str]) -> String {
    let mut css = String::new();
    for face in FACES.iter().filter(|face| families.contains(&face.family)) {
        let format = if face.file.ends_with(".otf") {
            "opentype"
        } else {
            "truetype"
        };
        css.push_str(&format!(
            "@font-face{{font-family:\"{}\";src:url(\"{ARTIFACT_FONT_ORIGIN}{ARTIFACT_FONT_PATH}{}\") format(\"{format}\");font-weight:{};font-style:{};font-display:swap}}",
            face.family,
            face.file,
            face.weight,
            if face.italic { "italic" } else { "normal" },
        ));
    }
    css
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_font_urls_name_exactly_one_bundled_file() {
        let mut files: Vec<_> = FACES.iter().map(|face| face.file).collect();
        files.sort_unstable();
        files.dedup();
        assert_eq!(files.len(), FACES.len(), "file names must be unique");
        assert!(FACES.iter().all(|face| !face.bytes.is_empty()));
        let url = |text: &str| url::Url::parse(text).unwrap();
        assert_eq!(
            artifact_face(&url("https://drostje-kit.invalid/fonts/Lilex-Regular.ttf"))
                .map(|face| face.family),
            Some("Lilex")
        );
        for other in [
            "https://drostje-kit.invalid/fonts/../Cargo.toml",
            "https://drostje-kit.invalid/fonts/Missing.ttf",
            "https://example.com/fonts/Lilex-Regular.ttf",
            "http://drostje-kit.invalid/fonts/Lilex-Regular.ttf",
            "file:///fonts/Lilex-Regular.ttf",
        ] {
            assert!(artifact_face(&url(other)).is_none(), "{other}");
        }
    }

    #[test]
    fn font_face_rules_cover_only_the_requested_bundled_families() {
        let css = artifact_font_faces(&["Lilex", "Not Bundled"]);
        assert_eq!(css.matches("@font-face").count(), 3);
        assert!(css.contains("font-family:\"Lilex\""));
        assert!(css.contains(
            "Lilex-Italic.ttf\") format(\"truetype\");font-weight:400;font-style:italic"
        ));
        assert!(!css.contains("Not Bundled"));
        assert!(artifact_font_faces(&["Monaspace Xenon"]).contains("format(\"opentype\")"));
    }
}
