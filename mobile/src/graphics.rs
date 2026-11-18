use serde::{Deserialize, Serialize};

use crate::storage::Storage;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphicsRecord {
    VulkanStarting,
    GlStarting,
    DrewOnVulkan,
    DrewOnGl,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartOn {
    VulkanThenGl,
    Gl,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Vulkan,
    Gl,
}

pub fn start(storage: &Storage) -> StartOn {
    let (start, starting) = match storage.graphics() {
        Some(GraphicsRecord::DrewOnVulkan) => return StartOn::VulkanThenGl,
        Some(GraphicsRecord::DrewOnGl) => return StartOn::Gl,
        Some(GraphicsRecord::VulkanStarting) => {
            log::warn!(
                "The previous launch ended before drawing on Vulkan; drawing with OpenGL ES"
            );
            (StartOn::Gl, GraphicsRecord::GlStarting)
        }
        Some(GraphicsRecord::GlStarting) => {
            log::warn!(
                "The previous launch ended before drawing on OpenGL ES; trying Vulkan again"
            );
            (StartOn::VulkanThenGl, GraphicsRecord::VulkanStarting)
        }
        None => (StartOn::VulkanThenGl, GraphicsRecord::VulkanStarting),
    };
    if let Err(error) = storage.save_graphics(Some(starting)) {
        log::warn!("Graphics start not recorded: {error:#}");
    }
    start
}

pub fn drew(storage: &Storage, backend: Backend) {
    let record = match backend {
        Backend::Vulkan => GraphicsRecord::DrewOnVulkan,
        Backend::Gl => GraphicsRecord::DrewOnGl,
    };
    if storage.graphics() == Some(record) {
        return;
    }
    if let Err(error) = storage.save_graphics(Some(record)) {
        log::warn!("Drawing on {backend:?} not recorded: {error:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> Storage {
        Storage::new(
            std::env::temp_dir().join(format!("drostje-graphics-{}", uuid::Uuid::new_v4())),
        )
    }

    #[test]
    fn a_launch_that_dies_before_drawing_on_vulkan_makes_the_next_ones_draw_with_gl() {
        let storage = temp();
        assert_eq!(start(&storage), StartOn::VulkanThenGl);
        assert_eq!(start(&storage), StartOn::Gl);
        drew(&storage, Backend::Gl);
        assert_eq!(start(&storage), StartOn::Gl);
        assert_eq!(start(&storage), StartOn::Gl);
        let _ = std::fs::remove_dir_all(storage.directory());
    }

    #[test]
    fn a_launch_that_falls_back_to_gl_by_itself_is_remembered() {
        let storage = temp();
        assert_eq!(start(&storage), StartOn::VulkanThenGl);
        drew(&storage, Backend::Gl);
        assert_eq!(start(&storage), StartOn::Gl);
        let _ = std::fs::remove_dir_all(storage.directory());
    }

    #[test]
    fn once_vulkan_drew_a_launch_that_dies_early_keeps_vulkan() {
        let storage = temp();
        assert_eq!(start(&storage), StartOn::VulkanThenGl);
        drew(&storage, Backend::Vulkan);
        assert_eq!(start(&storage), StartOn::VulkanThenGl);
        assert_eq!(start(&storage), StartOn::VulkanThenGl);
        let _ = std::fs::remove_dir_all(storage.directory());
    }

    #[test]
    fn when_gl_never_draws_either_the_launches_alternate_back_to_vulkan() {
        let storage = temp();
        assert_eq!(start(&storage), StartOn::VulkanThenGl);
        assert_eq!(start(&storage), StartOn::Gl);
        assert_eq!(start(&storage), StartOn::VulkanThenGl);
        assert_eq!(start(&storage), StartOn::Gl);
        let _ = std::fs::remove_dir_all(storage.directory());
    }

    #[test]
    fn a_damaged_record_starts_with_vulkan() {
        let storage = temp();
        std::fs::create_dir_all(storage.directory()).unwrap();
        std::fs::write(storage.directory().join("graphics.json"), b"{not json").unwrap();
        assert_eq!(start(&storage), StartOn::VulkanThenGl);
        assert_eq!(start(&storage), StartOn::Gl);
        let _ = std::fs::remove_dir_all(storage.directory());
    }
}
