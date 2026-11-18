use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use drostje_phone::{phone_side::PairedDesktop, view::PhoneView};
use serde::{Serialize, de::DeserializeOwned};

use crate::graphics::GraphicsRecord;

const PAIRING_FILE: &str = "paired-desktop.json";
const VIEW_FILE: &str = "desktop-view.json";
const GRAPHICS_FILE: &str = "graphics.json";

#[derive(Clone, Debug)]
pub struct Storage {
    directory: PathBuf,
}

impl Storage {
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    fn read<T: DeserializeOwned>(&self, name: &str) -> Option<T> {
        let bytes = fs::read(self.directory.join(name)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    fn write<T: Serialize>(&self, name: &str, value: &T) -> Result<()> {
        fs::create_dir_all(&self.directory)
            .with_context(|| format!("Creating {}", self.directory.display()))?;
        let target = self.directory.join(name);
        let staging = self.directory.join(format!(".{name}.tmp"));
        let bytes = serde_json::to_vec(value).context("Serializing phone state")?;
        let mut file =
            fs::File::create(&staging).with_context(|| format!("Writing {}", staging.display()))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&staging, &target).with_context(|| format!("Saving {}", target.display()))?;
        Ok(())
    }

    fn remove(&self, name: &str) -> Result<()> {
        match fs::remove_file(self.directory.join(name)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).context("Removing phone state"),
        }
    }

    pub fn pairing(&self) -> Option<PairedDesktop> {
        self.read(PAIRING_FILE)
    }

    pub fn save_pairing(&self, paired: Option<&PairedDesktop>) -> Result<()> {
        match paired {
            Some(paired) => self.write(PAIRING_FILE, paired),
            None => {
                self.remove(VIEW_FILE)?;
                self.remove(PAIRING_FILE)
            }
        }
    }

    pub fn view(&self) -> Option<PhoneView> {
        self.read(VIEW_FILE)
    }

    pub fn save_view(&self, view: &PhoneView) -> Result<()> {
        let mut cached = view.clone();
        cached.transcript = None;
        self.write(VIEW_FILE, &cached)
    }

    pub fn graphics(&self) -> Option<GraphicsRecord> {
        self.read(GRAPHICS_FILE)
    }

    pub fn save_graphics(&self, record: Option<GraphicsRecord>) -> Result<()> {
        match record {
            Some(record) => self.write(GRAPHICS_FILE, &record),
            None => self.remove(GRAPHICS_FILE),
        }
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use drostje_phone::{LinkKey, Topic, link::LinkRecord, view::Transcript};

    fn temp() -> Storage {
        let directory =
            std::env::temp_dir().join(format!("drostje-mobile-{}", uuid::Uuid::new_v4()));
        Storage::new(directory)
    }

    fn paired() -> PairedDesktop {
        PairedDesktop {
            desktop: "Studio".into(),
            token: Some("tk_x".into()),
            link: LinkRecord {
                server: "https://ntfy.example".into(),
                down: Topic::random(),
                up: Topic::random(),
                key: LinkKey::from_bytes([9; 32]),
            },
            since: Some("m1".into()),
        }
    }

    #[test]
    fn a_saved_pairing_and_view_come_back_and_unpairing_removes_both() {
        let storage = temp();
        assert!(storage.pairing().is_none());
        let saved = paired();
        storage.save_pairing(Some(&saved)).unwrap();
        let view = PhoneView {
            desktop: "Studio".into(),
            transcript: Some(Transcript {
                thread: uuid::Uuid::nil(),
                earlier: false,
                items: Vec::new(),
            }),
            ..PhoneView::default()
        };
        storage.save_view(&view).unwrap();
        assert_eq!(storage.pairing(), Some(saved));
        let cached = storage.view().unwrap();
        assert_eq!(cached.desktop, "Studio");
        assert!(cached.transcript.is_none());
        storage.save_pairing(None).unwrap();
        assert!(storage.pairing().is_none());
        assert!(storage.view().is_none());
        storage.save_pairing(None).unwrap();
        let _ = fs::remove_dir_all(storage.directory());
    }

    #[test]
    fn a_damaged_file_reads_as_unpaired() {
        let storage = temp();
        fs::create_dir_all(storage.directory()).unwrap();
        fs::write(storage.directory().join(PAIRING_FILE), b"{not json").unwrap();
        assert!(storage.pairing().is_none());
        let _ = fs::remove_dir_all(storage.directory());
    }
}
