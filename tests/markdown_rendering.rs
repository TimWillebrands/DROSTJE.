fn main() {
    #[cfg(target_os = "macos")]
    macos::run();
    #[cfg(not(target_os = "macos"))]
    println!("markdown_rendering: skipped; GPUI has no headless renderer on this platform");
}

#[cfg(target_os = "macos")]
mod macos {
    use gpui_kit::{
        AppContext, Context, HeadlessAppContext, InteractiveElement, IntoElement, ParentElement,
        Render, Styled, TestSupportExt, Window,
        assets::AllAssets,
        component::{ActiveTheme, Root, Theme, ThemeMode},
        div, px, size,
        test::TestWindowExt,
    };
    use std::sync::Arc;

    struct Sample {
        text: String,
        width: f32,
        font_size: f32,
    }

    impl Render for Sample {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .bg(cx.theme().background)
                .text_color(cx.theme().foreground)
                .font_family(cx.theme().font_family.clone())
                .text_size(px(self.font_size))
                .p_4()
                .child(
                    div()
                        .id("paragraph")
                        .test_support()
                        .w(px(self.width))
                        .child(drostje::ui::conversation::markdown_text(
                            "markdown",
                            self.text.clone(),
                            drostje::ui::conversation::Markdown::Element,
                        )),
                )
        }
    }

    pub fn run() {
        let mut cx = HeadlessAppContext::with_platform(
            gpui_kit::platform::current_platform(true).text_system(),
            Arc::new(AllAssets),
            gpui_kit::platform::current_headless_renderer,
        );
        cx.update(|cx| {
            gpui_kit::init(cx);
            drostje::ui::theme::initialize(None, None, cx);
        });
        let mut sample = None;
        let window = cx
            .open_window(size(px(640.), px(320.)), |window, cx| {
                let view = cx.new(|_| Sample {
                    text: String::new(),
                    width: 420.,
                    font_size: 15.,
                });
                sample = Some(view.clone());
                cx.new(|cx| Root::new(view, window, cx))
            })
            .unwrap();
        let sample = sample.unwrap();
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            for (text, width, font_size) in [
                ("with the `Skill`/`Part` split", 420., 15.),
                ("with the `Skill`/`Part` split", 420.25, 17.),
                ("**Bold** and *italic* with `codeblock` suffix", 420., 16.),
                (
                    "before `a_very_long_inline_identifier_that_must_wrap_without_losing_its_suffix` after",
                    137.,
                    15.,
                ),
                (
                    "Unicode `café_λ_日本語` and [a link](https://example.com)",
                    137.5,
                    17.,
                ),
            ] {
                let bounds = cx
                    .update_window(window.into(), |_, window, cx| {
                        Theme::change(mode, Some(window), cx);
                        sample.update(cx, |view, cx| {
                            view.text = text.into();
                            view.width = width;
                            view.font_size = font_size;
                            cx.notify();
                        });
                        window.render_frame(cx);
                        window.find("paragraph").bounds()
                    })
                    .unwrap();
                let image = cx.capture_screenshot(window.into()).unwrap();
                let scale = image.width() as f32 / 640.;
                let background = *image.get_pixel(0, 0);
                let right = ((bounds.right() + px(1.)).as_f32() * scale).ceil() as u32;
                let bottom = ((bounds.bottom() + px(1.)).as_f32() * scale).ceil() as u32;
                let spill = image
                    .enumerate_pixels()
                    .filter(|(x, y, pixel)| (*x >= right || *y >= bottom) && **pixel != background)
                    .count();
                assert_eq!(
                    spill, 0,
                    "{mode:?}, {font_size}px, {width}px: glyphs escaped paragraph bounds {bounds:?}: {text}"
                );

                if text == "with the `Skill`/`Part` split" {
                    let (left, right, foreground) = cx
                        .update_window(window.into(), |_, window, cx| {
                            let body = gpui_kit::TextStyle {
                                font_family: cx.theme().font_family.clone(),
                                ..Default::default()
                            };
                            let mono = gpui_kit::TextStyle {
                                font_family: cx.theme().mono_font_family.clone(),
                                ..Default::default()
                            };
                            let shape = |text: &'static str, code: bool| {
                                let style = if code { &mono } else { &body };
                                window.text_system().shape_line(
                                    text.into(),
                                    px(font_size * if code { 0.875 } else { 1. }),
                                    &[style.to_run(text.len())],
                                    None,
                                )
                            };
                            let part = shape("Part", true);
                            let start = bounds.left()
                                + shape("with the ", false).width()
                                + shape("Skill", true).width()
                                + px(4.)
                                + shape("/", false).width()
                                + px(2.);
                            (
                                start + part.x_for_index(3),
                                start + part.width(),
                                cx.theme().foreground.to_rgb(),
                            )
                        })
                        .unwrap();
                    let expected_ink =
                        [foreground.r, foreground.g, foreground.b].map(|channel| channel * 255.);
                    let suffix_is_visible = image.enumerate_pixels().any(|(x, y, pixel)| {
                        let x = x as f32 / scale;
                        let y = y as f32 / scale;
                        x >= left.as_f32() + 0.5
                            && x < right.as_f32()
                            && y >= bounds.top().as_f32()
                            && y < bounds.bottom().as_f32()
                            && pixel.0[..3]
                                .iter()
                                .zip(expected_ink)
                                .all(|(actual, expected)| {
                                    (f32::from(*actual) - expected).abs() < 32.
                                })
                    });
                    assert!(
                        suffix_is_visible,
                        "the final t of Part must paint on its own line"
                    );
                }

                cx.update_window(window.into(), |_, window, cx| {
                    window.drag(
                        bounds.origin + gpui_kit::point(px(0.5), px(8.)),
                        bounds.bottom_right() - gpui_kit::point(px(0.5), px(4.)),
                        cx,
                    );
                    window.render_frame(cx);
                    let selected = gpui_kit::base::TextSelection::selected_text(window, cx);
                    let expected = match text {
                        "with the `Skill`/`Part` split" => "with the Skill/Part split",
                        "**Bold** and *italic* with `codeblock` suffix" => "Bold and italic with codeblock suffix",
                        "before `a_very_long_inline_identifier_that_must_wrap_without_losing_its_suffix` after" => "before a_very_long_inline_identifier_that_must_wrap_without_losing_its_suffix after",
                        _ => "Unicode café_λ_日本語 and a link",
                    };
                    assert_eq!(selected.trim(), expected);
                    gpui_kit::base::TextSelection::clear(window, cx);
                }).unwrap();
            }
        }
        println!("markdown_rendering: real-font bounds and selection passed in both themes");
    }
}
