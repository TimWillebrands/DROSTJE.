//! The Conversation thread surface: transcript, composer band and the cards in between.
//! `mod.rs` re-exports what the rest of the UI uses and holds the layout helpers every
//! part shares.

mod blocks;
mod composer;
mod disclosure;
pub mod editor;
mod images;
mod job_cards;
mod job_notice;
mod markdown;
mod memo;
mod panel;
mod pickers;
mod queue;
mod requests;
mod rows;
pub(super) mod slash_commands;
pub(in crate::ui) mod spinner;
mod sticky_dock;
mod todo_list;
mod todos;
mod tool_runs;
mod tools;
mod transcript;
mod usage;
mod welcome;

pub(super) use blocks::block;
pub use markdown::Markdown;
pub(super) use markdown::file_references;
pub(super) use panel::render;
pub(super) use pickers::{EffortPicker, ModelPicker, PermissionModePicker};
pub use tools::tool_summary;
pub(super) use transcript::{TranscriptView, estimate_unmeasured_rows};
pub use welcome::welcome;

use gpui_kit::{
    component::{ActiveTheme, h_flex},
    *,
};

// A full-width row preserves the transcript's edge scrollbar; only its content is centered.
fn content_column() -> Div {
    div().w_full().max_w(rems(60.)).px_3()
}

fn inset(content: impl IntoElement) -> AnyElement {
    h_flex()
        .w_full()
        .justify_center()
        .child(content_column().child(content))
        .into_any_element()
}

fn mono(cx: &App) -> Div {
    div()
        .min_w_0()
        .font_family(cx.theme().mono_font_family.clone())
        .text_size(cx.theme().mono_font_size)
        .whitespace_normal()
}
