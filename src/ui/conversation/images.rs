use crate::model::ImageAttachment;
use crate::ui::workspace::Workspace;
use gpui_kit::{
    component::{
        Sizable,
        attachment::{
            Attachment, AttachmentActions, AttachmentContent, AttachmentDescription,
            AttachmentMedia, AttachmentTitle,
        },
        button::{Button, ButtonVariants},
    },
    *,
};

/// The images one transcript shows, loaded once and freed from every window's atlas when
/// the transcript closes. Without a cache of their own, thumbnails load into the app-wide
/// asset cache, which never lets them go.
pub(super) fn thumbnail_cache(cx: &mut App) -> Entity<RetainAllImageCache> {
    RetainAllImageCache::new(cx)
}

pub fn image_thumbnail(
    image: &ImageAttachment,
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
    Attachment::new()
        .id(SharedString::from(format!("image-{}", image.id)))
        .media(
            AttachmentMedia::new().w(rems(8.)).h(rems(6.)).child(
                img(image.path.clone())
                    .w_full()
                    .h_full()
                    .object_fit(ObjectFit::Contain),
            ),
        )
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
                    .small()
                    .ghost()
                    .label("Preview")
                    .tooltip("Preview image")
                    .on_click(open_preview),
            ),
        )
        .into_any_element()
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
        /// The decoded thumbnail as the cache holds it, read while painting.
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
                .child(image_thumbnail(&self.image, &owner, cx))
                .child(canvas(
                    move |_, window, cx| {
                        let image = cache.update(cx, |cache, cx| cache.load(&resource, window, cx));
                        *loaded.borrow_mut() = image.and_then(Result::ok);
                    },
                    |_, _, _, _| {},
                ))
        }
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

        // The transcript closes: its cache goes with it, and so do its images.
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
