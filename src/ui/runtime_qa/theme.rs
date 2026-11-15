//! Theme editor preview, cancel and save by keyboard.
use super::*;

#[gpui_kit::test]
#[ignore = "requires disposable DROSTJE_RUNTIME_FIXTURE; theme editor previews, cancels and saves by keyboard"]
async fn theme_editor_previews_cancels_and_saves_by_keyboard(cx: &mut TestAppContext) {
    use crate::ui::theme::{self, Adjustments};
    let fixture = open_fixture(cx, None).await;
    let window = fixture.window.into();
    let primary = |cx: &mut TestAppContext| cx.read(|cx| Theme::global(cx).primary);
    let saved = |cx: &mut TestAppContext| {
        cx.read(|cx| {
            Adjustments::from_setting(
                fixture
                    .state
                    .read(cx)
                    .snapshot
                    .settings
                    .get(theme::ADJUSTMENTS_SETTING),
            )
        })
    };
    let open = |cx: &mut TestAppContext| {
        cx.dispatch_action(window, commands::CustomizeTheme);
        settle_frames(cx, &fixture);
        assert!(cx.read(|cx| fixture.workspace.read(cx).theme_editor.is_some()));
    };
    let original = primary(cx);
    let theme_grain = cx.read(theme::theme_grain);

    // The field previews live; Escape restores the saved theme and writes nothing.
    open(cx);
    cx.simulate_keystrokes(window, "shift-l");
    settle_frames(cx, &fixture);
    assert_ne!(primary(cx), original, "the accent previews while editing");
    cx.simulate_keystrokes(window, "escape");
    settle_frames(cx, &fixture);
    assert!(cx.read(|cx| fixture.workspace.read(cx).theme_editor.is_none()));
    assert_eq!(primary(cx), original, "Escape restores the saved accent");
    assert_eq!(saved(cx), Adjustments::default());

    // Save keeps the preview: an accent from the field and grain from its row.
    open(cx);
    cx.simulate_keystrokes(window, "shift-l");
    settle_frames(cx, &fixture);
    let previewed = primary(cx);
    cx.simulate_keystrokes(window, "tab tab l");
    settle_frames(cx, &fixture);
    assert!((cx.read(theme::grain) - (theme_grain + 0.01)).abs() < 1e-4);
    cx.simulate_keystrokes(window, "secondary-enter");
    settle_frames(cx, &fixture);
    assert!(cx.read(|cx| fixture.workspace.read(cx).theme_editor.is_none()));
    assert_eq!(primary(cx), previewed, "Save keeps the previewed accent");
    let adjustments = saved(cx);
    assert!(adjustments.accent.is_some());
    assert!((adjustments.grain.unwrap() - (theme_grain + 0.01)).abs() < 1e-4);

    // The first swatch is the theme's own accent.
    open(cx);
    cx.simulate_keystrokes(window, "tab g g secondary-enter");
    settle_frames(cx, &fixture);
    assert_eq!(primary(cx), original);
    assert_eq!(saved(cx).accent, None);
    quit(cx, &fixture).await;
}
