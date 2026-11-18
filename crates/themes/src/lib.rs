pub mod grain;

use serde_json::{Map, Value};

pub const BUNDLED: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/themes/drostje.json"
));

const SIGNATURE: &str = "signature";
const GRAIN: &str = "grain";
const PROSE_FAMILY: &str = "prose.font.family";
const PROSE_SIZE: &str = "prose.font.size";
const PROSE_LINE_HEIGHT: &str = "prose.line_height";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Light,
    Dark,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

pub fn bundled() -> Vec<Value> {
    let set: Value =
        serde_json::from_str(BUNDLED).expect("assets/themes/drostje.json is valid JSON");
    set.get("themes")
        .and_then(Value::as_array)
        .expect("assets/themes/drostje.json lists its themes under \"themes\"")
        .clone()
}

pub fn bundled_default(mode: Mode) -> Value {
    bundled()
        .into_iter()
        .find(|theme| {
            theme.get("is_default") == Some(&Value::Bool(true))
                && theme.get("mode").and_then(Value::as_str) == Some(mode.name())
        })
        .unwrap_or_else(|| {
            panic!(
                "assets/themes/drostje.json has a default {} theme",
                mode.name()
            )
        })
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Prose {
    pub family: Option<String>,
    pub size: Option<f32>,
    pub line_height: Option<f32>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Extensions {
    pub prose: Prose,
    pub signature: Option<String>,
    pub grain: Option<f32>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ExtensionError {
    #[error("\"signature\" is not a hex color such as \"#c07f00\"")]
    Signature,
    #[error("\"grain\" is not a strength from 0 to 1")]
    Grain,
    #[error("\"{0}\" is not a positive number")]
    NotPositive(&'static str),
    #[error("\"prose.font.family\" is not a font family name")]
    ProseFamily,
}

fn is_hex_color(color: &str) -> bool {
    color.strip_prefix('#').is_some_and(|digits| {
        matches!(digits.len(), 3 | 4 | 6 | 8) && digits.chars().all(|c| c.is_ascii_hexdigit())
    })
}

impl Extensions {
    pub fn from_theme(theme: &Value) -> Result<Self, ExtensionError> {
        let positive = |key: &'static str| -> Result<Option<f32>, ExtensionError> {
            theme
                .get(key)
                .map(|value| {
                    value
                        .as_f64()
                        .filter(|value| value.is_finite() && *value > 0.)
                        .map(|value| value as f32)
                        .ok_or(ExtensionError::NotPositive(key))
                })
                .transpose()
        };
        Ok(Self {
            prose: Prose {
                family: theme
                    .get(PROSE_FAMILY)
                    .map(|family| {
                        family
                            .as_str()
                            .filter(|family| !family.trim().is_empty())
                            .map(str::to_owned)
                            .ok_or(ExtensionError::ProseFamily)
                    })
                    .transpose()?,
                size: positive(PROSE_SIZE)?,
                line_height: positive(PROSE_LINE_HEIGHT)?,
            },
            signature: theme
                .get(SIGNATURE)
                .map(|color| {
                    color
                        .as_str()
                        .filter(|color| is_hex_color(color))
                        .map(str::to_owned)
                        .ok_or(ExtensionError::Signature)
                })
                .transpose()?,
            grain: theme
                .get(GRAIN)
                .map(|grain| {
                    grain
                        .as_f64()
                        .filter(|grain| (0. ..=1.).contains(grain))
                        .map(|grain| grain as f32)
                        .ok_or(ExtensionError::Grain)
                })
                .transpose()?,
        })
    }

    fn write_into(&self, theme: &mut Map<String, Value>) {
        let mut set = |key: &str, value: Option<Value>| match value {
            Some(value) => {
                theme.insert(key.to_owned(), value);
            }
            None => {
                theme.remove(key);
            }
        };
        set(PROSE_FAMILY, self.prose.family.clone().map(Value::from));
        set(PROSE_SIZE, self.prose.size.map(Value::from));
        set(PROSE_LINE_HEIGHT, self.prose.line_height.map(Value::from));
        set(SIGNATURE, self.signature.clone().map(Value::from));
        set(GRAIN, self.grain.map(Value::from));
    }
}

fn without_nulls(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter(|(_, value)| !value.is_null())
                .map(|(key, value)| (key, without_nulls(value)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(without_nulls).collect()),
        other => other,
    }
}

pub fn portable(config: Value, extensions: &Extensions) -> Value {
    let Value::Object(mut theme) = without_nulls(config) else {
        panic!("a Kit theme config serializes to a JSON object");
    };
    extensions.write_into(&mut theme);
    Value::Object(theme)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn delft_is_the_default_in_both_modes_and_every_bundled_extension_reads() {
        assert_eq!(bundled_default(Mode::Light)["name"], "Delft Light");
        assert_eq!(bundled_default(Mode::Dark)["name"], "Delft Dark");
        for mode in [Mode::Light, Mode::Dark] {
            let defaults = bundled()
                .into_iter()
                .filter(|theme| theme["mode"] == mode.name() && theme["is_default"] == true)
                .count();
            assert_eq!(defaults, 1, "one default {} theme", mode.name());
        }
        for theme in bundled() {
            Extensions::from_theme(&theme)
                .unwrap_or_else(|error| panic!("{}: {error}", theme["name"]));
        }
    }

    #[test]
    fn extensions_read_the_bundled_keys() {
        let ink = bundled()
            .into_iter()
            .find(|theme| theme["name"] == "Ink Light")
            .unwrap();
        assert_eq!(
            Extensions::from_theme(&ink).unwrap(),
            Extensions {
                prose: Prose {
                    family: Some("Libron".into()),
                    size: Some(17.),
                    line_height: Some(1.55),
                },
                signature: None,
                grain: Some(0.15),
            }
        );
        let cootje = bundled()
            .into_iter()
            .find(|theme| theme["name"] == "Cootje Dark")
            .unwrap();
        let cootje = Extensions::from_theme(&cootje).unwrap();
        assert_eq!(cootje.signature.as_deref(), Some("#f2b33d"));
        assert_eq!(cootje.grain, Some(0.25));
        assert_eq!(cootje.prose, Prose::default());
    }

    #[test]
    fn malformed_extensions_are_named() {
        let read = |theme: Value| Extensions::from_theme(&theme);
        assert_eq!(
            read(json!({"signature": "saffron"})),
            Err(ExtensionError::Signature)
        );
        assert_eq!(
            read(json!({"signature": "#12345"})),
            Err(ExtensionError::Signature)
        );
        assert_eq!(read(json!({"grain": 1.5})), Err(ExtensionError::Grain));
        assert_eq!(
            read(json!({"prose.font.size": 0})),
            Err(ExtensionError::NotPositive("prose.font.size"))
        );
        assert_eq!(
            read(json!({"prose.font.family": " "})),
            Err(ExtensionError::ProseFamily)
        );
        assert_eq!(read(json!({"name": "Plain"})), Ok(Extensions::default()));
    }

    #[test]
    fn a_portable_theme_drops_unset_fields_and_carries_its_extensions() {
        let config = json!({
            "name": "Delft Dark",
            "mode": "dark",
            "radius": 4,
            "radius.lg": null,
            "grain": 0.1,
            "colors": {"background": "#121b2e", "border": null},
        });
        let extensions = Extensions {
            prose: Prose {
                family: Some("Libron".into()),
                size: None,
                line_height: Some(1.5),
            },
            signature: Some("#c07f00".into()),
            grain: None,
        };
        let theme = portable(config, &extensions);
        assert_eq!(
            theme,
            json!({
                "name": "Delft Dark",
                "mode": "dark",
                "radius": 4,
                "colors": {"background": "#121b2e"},
                "prose.font.family": "Libron",
                "prose.line_height": 1.5,
                "signature": "#c07f00",
            })
        );
        assert_eq!(Extensions::from_theme(&theme), Ok(extensions));
    }
}
