use crate::model::ImageAttachment;
use crate::ui::{
    commands::{FitImage, ImageCommand, ShowImageActualSize, ZoomImageIn, ZoomImageOut},
    workspace::Workspace,
};
use gpui_kit::{
    assets::IconName as Lucide,
    component::{
        ActiveTheme, Disableable, Sizable,
        attachment::{
            Attachment, AttachmentActions, AttachmentContent, AttachmentDescription,
            AttachmentMedia, AttachmentTitle,
        },
        button::{Button, ButtonVariants},
        h_flex, v_flex,
    },
    prelude::FluentBuilder,
    *,
};

pub(super) fn thumbnail_cache(cx: &mut App) -> Entity<RetainAllImageCache> {
    RetainAllImageCache::new(cx)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThumbnailLayout {
    Card,
    Chip,
}

pub fn image_thumbnail(
    image: &ImageAttachment,
    layout: ThumbnailLayout,
    owner: &WeakEntity<Workspace>,
    _cx: &App,
) -> AnyElement {
    let preview = image.clone();
    let owner = owner.clone();
    let open_preview = move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
        cx.stop_propagation();
        let _ = owner.update(cx, |workspace, cx| {
            workspace.image_preview = Some(preview.clone());
            workspace.sync_detail_surface(window, cx);
            cx.notify();
        });
    };
    let attachment = Attachment::new()
        .id(SharedString::from(format!("image-{}", image.id)))
        .tooltip("Preview image");
    let attachment = match layout {
        ThumbnailLayout::Card => attachment.axis(Axis::Vertical).w(THUMBNAIL_WIDTH).media(
            AttachmentMedia::new().child(
                img(image.path.clone())
                    .size_full()
                    .object_fit(ObjectFit::Contain),
            ),
        ),
        ThumbnailLayout::Chip => attachment
            .small()
            .media(AttachmentMedia::new().src(image.path.clone())),
    };
    attachment
        .content(
            AttachmentContent::new()
                .title(AttachmentTitle::new(format!(
                    "{} × {}",
                    image.width, image.height
                )))
                .description(AttachmentDescription::new(format!(
                    "{}{}",
                    image.mime,
                    if image.original.is_some() {
                        " · prepared"
                    } else {
                        ""
                    }
                ))),
        )
        .on_click(open_preview.clone())
        .actions(
            AttachmentActions::new().child(
                Button::new(SharedString::from(format!("preview-image-{}", image.id)))
                    .xsmall()
                    .icon(Lucide::Maximize2)
                    .tooltip("Preview image")
                    .on_click(open_preview),
            ),
        )
        .into_any_element()
}

const THUMBNAIL_WIDTH: Rems = Rems(9.);
const MIN_ZOOM: f32 = 0.05;
const MAX_ZOOM: f32 = 16.;
const ZOOM_STEPS: [f32; 15] = [
    0.05, 0.1, 0.25, 0.33, 0.5, 0.67, 0.75, 1., 1.5, 2., 3., 4., 6., 8., 16.,
];
const WHEEL_ZOOM_PER_PIXEL: f32 = 0.005;
const VIEWPORT_BORDER: f32 = 1.;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::ui) enum ImageZoom {
    Fit,
    Scale(f32),
}

impl ImageZoom {
    pub(in crate::ui) fn scale(self, fit: f32) -> f32 {
        match self {
            Self::Fit => fit,
            Self::Scale(scale) => scale,
        }
    }

    pub(in crate::ui) fn zoomed_in(self, fit: f32) -> Option<Self> {
        let current = self.scale(fit);
        ZOOM_STEPS
            .into_iter()
            .find(|step| *step > current * 1.001)
            .map(Self::Scale)
    }

    pub(in crate::ui) fn zoomed_out(self, fit: f32) -> Option<Self> {
        let current = self.scale(fit);
        ZOOM_STEPS
            .into_iter()
            .rev()
            .find(|step| *step < current * 0.999)
            .map(Self::Scale)
    }

    pub(in crate::ui) fn scaled_by(self, fit: f32, factor: f32) -> Self {
        Self::Scale((self.scale(fit) * factor).clamp(MIN_ZOOM, MAX_ZOOM))
    }

    pub(in crate::ui) fn is_actual_size(self, fit: f32) -> bool {
        (self.scale(fit) - 1.).abs() < 0.001
    }
}

pub(in crate::ui) fn fit_scale(image: (f32, f32), viewport: (f32, f32)) -> f32 {
    let (width, height) = (image.0.max(1.), image.1.max(1.));
    (viewport.0 / width)
        .min(viewport.1 / height)
        .clamp(MIN_ZOOM, 1.)
}

pub(in crate::ui) fn anchored_offset(
    offset: f32,
    anchor: f32,
    viewport: f32,
    before: f32,
    after: f32,
) -> f32 {
    let origin = |image: f32| (viewport.max(image) - image) / 2.;
    let fraction = ((anchor - offset - origin(before)) / before.max(1.)).clamp(0., 1.);
    let target = origin(after) + fraction * after;
    clamped_offset(anchor - target, viewport, after)
}

pub(in crate::ui) fn clamped_offset(offset: f32, viewport: f32, image: f32) -> f32 {
    offset.clamp(-(image - viewport).max(0.), 0.)
}

pub(in crate::ui) struct ImagePreview {
    image: ImageAttachment,
    zoom: ImageZoom,
    scroll: ScrollHandle,
    focus: FocusHandle,
    pan: Option<(Point<Pixels>, Point<Pixels>)>,
}

impl ImagePreview {
    pub(in crate::ui) fn new(image: ImageAttachment, cx: &mut Context<Self>) -> Self {
        Self {
            image,
            zoom: ImageZoom::Fit,
            scroll: ScrollHandle::new(),
            focus: cx.focus_handle(),
            pan: None,
        }
    }

    pub(in crate::ui) fn image(&self) -> &ImageAttachment {
        &self.image
    }

    #[cfg(all(test, feature = "runtime-qa"))]
    pub(in crate::ui) fn zoom(&self) -> ImageZoom {
        self.zoom
    }

    pub(in crate::ui) fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    fn natural(&self) -> (f32, f32) {
        (
            self.image.width.max(1) as f32,
            self.image.height.max(1) as f32,
        )
    }

    fn viewport(&self) -> (f32, f32) {
        let size = self.scroll.bounds().size;
        (
            (size.width.as_f32() - VIEWPORT_BORDER * 2.).max(0.),
            (size.height.as_f32() - VIEWPORT_BORDER * 2.).max(0.),
        )
    }

    fn fit(&self) -> f32 {
        let viewport = self.viewport();
        if viewport.0 <= 0. || viewport.1 <= 0. {
            return 1.;
        }
        fit_scale(self.natural(), viewport)
    }

    pub(in crate::ui) fn disabled_reason(&self, command: ImageCommand) -> Option<&'static str> {
        command.disabled_reason(self.zoom, self.fit())
    }

    fn set_zoom(&mut self, zoom: ImageZoom, anchor: Option<Point<Pixels>>, cx: &mut Context<Self>) {
        let fit = self.fit();
        let (width, height) = self.natural();
        let (before, after) = (self.zoom.scale(fit), zoom.scale(fit));
        let viewport = self.viewport();
        let origin = self.scroll.bounds().origin;
        let anchor = anchor.map_or((viewport.0 / 2., viewport.1 / 2.), |point| {
            (
                (point.x - origin.x).as_f32() - VIEWPORT_BORDER,
                (point.y - origin.y).as_f32() - VIEWPORT_BORDER,
            )
        });
        let offset = self.scroll.offset();
        self.scroll.set_offset(point(
            px(anchored_offset(
                offset.x.as_f32(),
                anchor.0,
                viewport.0,
                width * before,
                width * after,
            )),
            px(anchored_offset(
                offset.y.as_f32(),
                anchor.1,
                viewport.1,
                height * before,
                height * after,
            )),
        ));
        self.zoom = zoom;
        cx.notify();
    }

    pub(in crate::ui) fn run(&mut self, command: ImageCommand, cx: &mut Context<Self>) {
        if self.disabled_reason(command).is_some() {
            return;
        }
        let fit = self.fit();
        let zoom = match command {
            ImageCommand::ZoomIn => self.zoom.zoomed_in(fit),
            ImageCommand::ZoomOut => self.zoom.zoomed_out(fit),
            ImageCommand::Fit => Some(ImageZoom::Fit),
            ImageCommand::ActualSize => Some(ImageZoom::Scale(1.)),
        };
        if let Some(zoom) = zoom {
            self.set_zoom(zoom, None, cx);
        }
    }

    fn pan_to(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some((start, origin)) = self.pan else {
            return;
        };
        let scale = self.zoom.scale(self.fit());
        let (width, height) = self.natural();
        let viewport = self.viewport();
        let moved = origin + (position - start);
        self.scroll.set_offset(point(
            px(clamped_offset(moved.x.as_f32(), viewport.0, width * scale)),
            px(clamped_offset(moved.y.as_f32(), viewport.1, height * scale)),
        ));
        cx.notify();
    }

    fn command_button(&self, command: ImageCommand, id: &'static str) -> Button {
        let reason = self.disabled_reason(command);
        let button = Button::new(id).small().ghost().disabled(reason.is_some());
        let button = match command {
            ImageCommand::ZoomIn => button.icon(Lucide::ZoomIn),
            ImageCommand::ZoomOut => button.icon(Lucide::ZoomOut),
            ImageCommand::Fit => button.label("Fit"),
            ImageCommand::ActualSize => button.label("100%"),
        };
        match reason {
            Some(reason) => button.tooltip(reason),
            None => button.tooltip_with_action(
                command.label(),
                command.action().as_ref(),
                Some(IMAGE_PREVIEW_CONTEXT),
            ),
        }
    }
}

pub(in crate::ui) const IMAGE_PREVIEW_CONTEXT: &str = "ImagePreview";

impl Render for ImagePreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let measured = self.viewport();
        if measured.0 <= 0. || measured.1 <= 0. {
            window.request_animation_frame();
        }
        let fit = self.fit();
        let scale = self.zoom.scale(fit);
        let (width, height) = self.natural();
        let (shown_width, shown_height) = (width * scale, height * scale);
        let rem = window.rem_size().as_f32();
        let max_height = (window.viewport_size().height.as_f32() * 0.65).min(rem * 42.);
        let viewport_height = height.clamp(rem * 12., max_height.max(rem * 12.));
        let pannable = shown_width > measured.0 + 0.5 || shown_height > measured.1 + 0.5;
        let entity = cx.entity();
        let control = |command: ImageCommand| {
            let entity = entity.clone();
            move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                entity.update(cx, |preview, cx| preview.run(command, cx));
            }
        };
        let percent = format!("{:.0}%", scale * 100.);
        let toolbar = h_flex()
            .w_full()
            .items_center()
            .gap_1()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "{} × {} · {}",
                        self.image.width, self.image.height, self.image.mime
                    )),
            )
            .child(
                self.command_button(ImageCommand::ZoomOut, "image-preview-zoom-out")
                    .on_click(control(ImageCommand::ZoomOut)),
            )
            .child(
                div()
                    .debug_selector(|| "image-preview-zoom".into())
                    .w(rems(3.5))
                    .text_center()
                    .text_sm()
                    .font_family(cx.theme().mono_font_family.clone())
                    .child(percent),
            )
            .child(
                self.command_button(ImageCommand::ZoomIn, "image-preview-zoom-in")
                    .on_click(control(ImageCommand::ZoomIn)),
            )
            .child(
                self.command_button(ImageCommand::Fit, "image-preview-fit")
                    .on_click(control(ImageCommand::Fit)),
            )
            .child(
                self.command_button(ImageCommand::ActualSize, "image-preview-actual-size")
                    .on_click(control(ImageCommand::ActualSize)),
            );
        let content = div()
            .flex()
            .items_center()
            .justify_center()
            .w(px(shown_width.max(measured.0)))
            .h(px(shown_height.max(measured.1)))
            .child(
                img(self.image.path.clone())
                    .flex_none()
                    .w(px(shown_width))
                    .h(px(shown_height))
                    .object_fit(ObjectFit::Fill),
            )
            .on_scroll(cx.listener(|preview, event: &ScrollEvent, window, cx| {
                if !event.modifiers.secondary(cx) {
                    return;
                }
                cx.stop_propagation();
                let delta = event.delta.pixel_delta(window.line_height()).y.as_f32();
                let zoom = preview
                    .zoom
                    .scaled_by(preview.fit(), (delta * WHEEL_ZOOM_PER_PIXEL).exp());
                preview.set_zoom(zoom, Some(event.position), cx);
            }))
            .on_pinch(cx.listener(|preview, event: &PinchEvent, _, cx| {
                cx.stop_propagation();
                let zoom = preview.zoom.scaled_by(preview.fit(), 1. + event.delta);
                preview.set_zoom(zoom, Some(event.position), cx);
            }));
        let viewport = div()
            .id("image-preview-viewport")
            .debug_selector(|| "image-preview-viewport".into())
            .relative()
            .w_full()
            .h(px(viewport_height))
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted)
            .overflow_scroll()
            .track_scroll(&self.scroll)
            .when(pannable, |viewport| {
                viewport.cursor(if self.pan.is_some() {
                    CursorStyle::ClosedHand
                } else {
                    CursorStyle::OpenHand
                })
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|preview, event: &MouseDownEvent, window, cx| {
                    if event.click_count == 2 {
                        let fit = preview.fit();
                        let zoom = if preview.zoom.is_actual_size(fit) {
                            ImageZoom::Fit
                        } else {
                            ImageZoom::Scale(1.)
                        };
                        preview.pan = None;
                        preview.set_zoom(zoom, Some(event.position), cx);
                    } else {
                        preview.pan = Some((event.position, preview.scroll.offset()));
                        cx.notify();
                    }
                    preview.focus.focus(window, cx);
                }),
            )
            .on_mouse_move(cx.listener(|preview, event: &MouseMoveEvent, _, cx| {
                if preview.pan.is_some() {
                    if event.pressed_button == Some(MouseButton::Left) {
                        preview.pan_to(event.position, cx);
                    } else {
                        preview.pan = None;
                        cx.notify();
                    }
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|preview, _: &MouseUpEvent, _, cx| {
                    preview.pan = None;
                    cx.notify();
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|preview, _: &MouseUpEvent, _, cx| {
                    preview.pan = None;
                    cx.notify();
                }),
            )
            .child(content);
        v_flex()
            .id("image-preview")
            .key_context(IMAGE_PREVIEW_CONTEXT)
            .track_focus(&self.focus)
            .on_action(
                cx.listener(|preview, _: &ZoomImageIn, _, cx| {
                    preview.run(ImageCommand::ZoomIn, cx)
                }),
            )
            .on_action(cx.listener(|preview, _: &ZoomImageOut, _, cx| {
                preview.run(ImageCommand::ZoomOut, cx)
            }))
            .on_action(
                cx.listener(|preview, _: &FitImage, _, cx| preview.run(ImageCommand::Fit, cx)),
            )
            .on_action(cx.listener(|preview, _: &ShowImageActualSize, _, cx| {
                preview.run(ImageCommand::ActualSize, cx)
            }))
            .w_full()
            .gap_2()
            .child(toolbar)
            .child(viewport)
    }
}

#[cfg(test)]
mod zoom_tests {
    use super::{ImageZoom, MAX_ZOOM, anchored_offset, clamped_offset, fit_scale};

    #[test]
    fn fit_shrinks_large_images_and_never_enlarges_small_ones() {
        assert_eq!(fit_scale((2000., 500.), (1000., 500.)), 0.5);
        assert_eq!(fit_scale((100., 4000.), (800., 400.)), 0.1);
        assert_eq!(fit_scale((32., 32.), (800., 400.)), 1.);
        assert_eq!(fit_scale((0., 0.), (800., 400.)), 1.);
    }

    #[test]
    fn zoom_steps_move_from_the_fitted_scale_to_the_next_preset() {
        assert_eq!(ImageZoom::Fit.zoomed_in(0.4), Some(ImageZoom::Scale(0.5)));
        assert_eq!(ImageZoom::Fit.zoomed_out(0.4), Some(ImageZoom::Scale(0.33)));
        assert_eq!(
            ImageZoom::Scale(1.).zoomed_in(0.4),
            Some(ImageZoom::Scale(1.5))
        );
        assert_eq!(ImageZoom::Scale(MAX_ZOOM).zoomed_in(0.4), None);
        assert_eq!(ImageZoom::Scale(0.05).zoomed_out(0.4), None);
    }

    #[test]
    fn wheel_zoom_stays_within_its_limits() {
        assert_eq!(
            ImageZoom::Scale(12.).scaled_by(1., 4.),
            ImageZoom::Scale(MAX_ZOOM)
        );
        assert_eq!(ImageZoom::Fit.scaled_by(0.5, 2.), ImageZoom::Scale(1.));
    }

    #[test]
    fn zooming_keeps_the_point_under_the_anchor_in_place() {
        assert_eq!(anchored_offset(0., 200., 400., 400., 800.), -200.);
        assert_eq!(anchored_offset(0., 0., 400., 400., 800.), 0.);
        assert_eq!(anchored_offset(-200., 200., 400., 800., 400.), 0.);
        assert_eq!(anchored_offset(0., 100., 400., 100., 200.), 0.);
    }

    #[test]
    fn panning_stops_at_the_image_edges() {
        assert_eq!(clamped_offset(50., 400., 800.), 0.);
        assert_eq!(clamped_offset(-900., 400., 800.), -400.);
        assert_eq!(clamped_offset(-50., 400., 300.), 0.);
    }
}

#[cfg(all(test, feature = "runtime-qa"))]
mod tests {
    use super::{image_thumbnail, thumbnail_cache};
    use crate::{model::ImageAttachment, ui::workspace::Workspace};
    use gpui_kit::{
        AppContext, Context, Entity, IntoElement, ParentElement, Render, RenderImage, Resource,
        RetainAllImageCache, Styled, WeakEntity, Window, canvas, component::Root, div,
    };
    use std::{cell::RefCell, rc::Rc, sync::Arc};

    struct Thumbnails {
        cache: Option<Entity<RetainAllImageCache>>,
        image: ImageAttachment,
        loaded: Rc<RefCell<Option<Arc<RenderImage>>>>,
    }
    impl Render for Thumbnails {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let owner = WeakEntity::<Workspace>::new_invalid();
            let Some(cache) = self.cache.clone() else {
                return div();
            };
            let (loaded, resource) = (self.loaded.clone(), Resource::from(self.image.path.clone()));
            div()
                .size_full()
                .image_cache(cache.clone())
                .child(image_thumbnail(
                    &self.image,
                    super::ThumbnailLayout::Card,
                    &owner,
                    cx,
                ))
                .child(canvas(
                    move |_, window, cx| {
                        let image = cache.update(cx, |cache, cx| cache.load(&resource, window, cx));
                        *loaded.borrow_mut() = image.and_then(Result::ok);
                    },
                    |_, _, _, _| {},
                ))
        }
    }

    struct PreviewHost(Entity<super::ImagePreview>);
    impl Render for PreviewHost {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().p_4().child(self.0.clone())
        }
    }

    #[gpui_kit::test]
    fn the_image_preview_zooms_from_keyboard_wheel_and_double_click(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use super::{ImageCommand, ImageZoom};
        use gpui_kit::{
            Modifiers, MouseButton, MouseDownEvent, ScrollDelta, ScrollEvent, VisualTestContext,
            point, px,
        };
        cx.dispatcher.allow_parking();
        let directory =
            std::env::temp_dir().join(format!("drostje-preview-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("icon.png");
        std::fs::copy(
            concat!(env!("CARGO_MANIFEST_DIR"), "/assets/drostje-app-icon.png"),
            &path,
        )
        .unwrap();
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::ui::theme::initialize(None, None, cx);
            crate::ui::commands::bind_keys(cx);
        });
        let image = ImageAttachment {
            id: uuid::Uuid::from_u128(2),
            path,
            mime: "image/png".into(),
            width: 1024,
            height: 1024,
            original: None,
        };
        let mut preview = None;
        let window = cx.add_window(|window, cx| {
            let entity = cx.new(|cx| super::ImagePreview::new(image, cx));
            preview = Some(entity.clone());
            let host = cx.new(|_| PreviewHost(entity));
            Root::new(host, window, cx)
        });
        let preview = preview.unwrap();
        let cx = &mut VisualTestContext::from_window(window.into(), cx);
        let settle = |cx: &mut VisualTestContext| {
            for _ in 0..3 {
                cx.run_until_parked();
                cx.update(|window, cx| {
                    window.refresh();
                    window.draw(cx).clear(cx);
                });
            }
        };
        settle(cx);
        cx.update(|window, cx| preview.read(cx).focus_handle().focus(window, cx));
        settle(cx);
        let zoom = |cx: &mut VisualTestContext| cx.read(|cx| preview.read(cx).zoom());
        let reason = |cx: &mut VisualTestContext, command| {
            cx.read(|cx| preview.read(cx).disabled_reason(command))
        };
        let viewport = cx
            .debug_bounds("image-preview-viewport")
            .expect("the preview renders its viewport");
        assert!(viewport.size.height < px(1024.));
        assert_eq!(zoom(cx), ImageZoom::Fit);
        assert_eq!(
            reason(cx, ImageCommand::Fit),
            Some("The image already fits the window")
        );
        assert_eq!(reason(cx, ImageCommand::ActualSize), None);

        cx.simulate_keystrokes("=");
        settle(cx);
        let ImageZoom::Scale(zoomed) = zoom(cx) else {
            panic!("zooming in leaves fit mode");
        };
        assert!(
            zoomed < 1.,
            "the first step from a shrunk fit stays below 100%"
        );
        assert_eq!(reason(cx, ImageCommand::Fit), None);

        cx.simulate_keystrokes("1");
        settle(cx);
        assert_eq!(zoom(cx), ImageZoom::Scale(1.));
        assert_eq!(
            reason(cx, ImageCommand::ActualSize),
            Some("The image is at actual size")
        );

        cx.simulate_keystrokes("0");
        settle(cx);
        assert_eq!(zoom(cx), ImageZoom::Fit);

        for _ in 0..20 {
            cx.simulate_keystrokes("-");
        }
        settle(cx);
        assert_eq!(zoom(cx), ImageZoom::Scale(0.05));
        assert_eq!(
            reason(cx, ImageCommand::ZoomOut),
            Some("Already at the smallest zoom")
        );

        let centre = viewport.center();
        let secondary = cx.update(|_, cx| Modifiers::secondary_key(cx));
        cx.simulate_event(ScrollEvent {
            position: centre,
            delta: ScrollDelta::Pixels(point(px(0.), px(200.))),
            modifiers: secondary,
            ..Default::default()
        });
        settle(cx);
        let ImageZoom::Scale(wheeled) = zoom(cx) else {
            panic!("wheel zoom sets an explicit scale");
        };
        assert!(wheeled > 0.05, "secondary-wheel up zooms in: {wheeled}");

        for count in 1..=2 {
            cx.simulate_event(MouseDownEvent {
                position: centre,
                modifiers: Modifiers::none(),
                button: MouseButton::Left,
                click_count: count,
                first_mouse: false,
            });
            cx.simulate_mouse_up(centre, MouseButton::Left, Modifiers::none());
        }
        settle(cx);
        assert_eq!(
            zoom(cx),
            ImageZoom::Scale(1.),
            "double-click shows actual size"
        );
        cx.simulate_keystrokes("secondary-0");
        settle(cx);
        assert_eq!(zoom(cx), ImageZoom::Fit);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[gpui_kit::test]
    fn closing_a_transcript_frees_its_thumbnails(cx: &mut gpui_kit::TestAppContext) {
        cx.dispatcher.allow_parking();
        let directory =
            std::env::temp_dir().join(format!("drostje-thumbnails-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("icon.png");
        std::fs::copy(
            concat!(env!("CARGO_MANIFEST_DIR"), "/assets/drostje-app-icon.png"),
            &path,
        )
        .unwrap();
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::ui::theme::initialize(None, None, cx);
        });
        let image = ImageAttachment {
            id: uuid::Uuid::from_u128(1),
            path,
            mime: "image/png".into(),
            width: 1,
            height: 1,
            original: None,
        };
        let cache = cx.update(thumbnail_cache);
        let loaded = Rc::new(RefCell::new(None));
        let mut view = None;
        let window = cx.add_window(|window, cx| {
            let thumbnails = cx.new(|_| Thumbnails {
                cache: Some(cache.clone()),
                image,
                loaded: loaded.clone(),
            });
            view = Some(thumbnails.clone());
            Root::new(thumbnails, window, cx)
        });
        let view = view.unwrap();
        let draw = |cx: &mut gpui_kit::TestAppContext| {
            cx.run_until_parked();
            cx.update_window(window.into(), |_, window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            })
            .unwrap();
        };
        for _ in 0..100 {
            draw(cx);
            if loaded.borrow().is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let image = loaded
            .borrow_mut()
            .take()
            .expect("the thumbnail loads through the transcript's cache");
        draw(cx);
        let in_atlas = |cx: &mut gpui_kit::TestAppContext| {
            cx.update_window(window.into(), |_, window, _| {
                window.has_image_atlas_entry(&image)
            })
            .unwrap()
        };
        assert!(in_atlas(cx), "a shown thumbnail is in the atlas");

        drop(cache);
        cx.update(|cx| {
            view.update(cx, |view, cx| {
                view.cache = None;
                cx.notify();
            })
        });
        draw(cx);
        assert!(
            !in_atlas(cx),
            "a closed transcript's thumbnails leave the atlas"
        );
        std::fs::remove_dir_all(directory).unwrap();
    }
}
