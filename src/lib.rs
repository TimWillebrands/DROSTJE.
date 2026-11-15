pub mod app;
pub use drostje_browser as browser;
pub use drostje_browser_tools as browser_tools;
pub use drostje_diff as diff;
pub use drostje_factory as factory;
pub use drostje_file_refs as file_refs;
pub use drostje_fonts as fonts;
pub use drostje_harness as harness;
pub use drostje_harness::attachments;
pub use drostje_model as model;
pub use drostje_neovim as neovim;
pub use drostje_network as network;
pub use drostje_platform as platform;
pub use drostje_remote as remote;
pub use drostje_store as store;
pub use drostje_titles as titles;
pub use drostje_tracker as tracker;
pub mod ui;
pub use drostje_worktrees as worktrees;

use gpui_kit::{
    assets::AllAssets,
    component::{Root, TitleBar},
    *,
};

const APP_ASSETS: AllAssets = AllAssets;

pub fn run() {
    if browser::serve_content_process() {
        return;
    }
    if let Err(error) = platform::import_login_shell_environment() {
        eprintln!("Could not import the login shell environment: {error:#}");
    }
    let bootstrap = app::Bootstrap::load();
    if let Err(error) = &bootstrap
        && matches!(
            error.downcast_ref::<store::StoreError>(),
            Some(store::StoreError::InstanceInUse(_))
        )
    {
        eprintln!("DROSTJE. is already using this data directory: {error:#}");
        return;
    }
    gpui_kit::application()
        .with_assets(APP_ASSETS)
        .run(move |cx| {
            gpui_kit::init(cx);
            match &bootstrap {
                Ok(bootstrap) => ui::theme::initialize(
                    Some(&bootstrap.snapshot.settings),
                    Some(&bootstrap.data_dir),
                    cx,
                ),
                Err(_) => ui::theme::initialize(None, None, cx),
            }
            if let Err(error) = platform::set_application_icon() {
                eprintln!("Could not set the DROSTJE. application icon: {error:#}");
            }
            ui::commands::bind_keys(cx);
            cx.set_menus(vec![Menu::new("DROSTJE.").items([
                MenuItem::action("Settings…", ui::commands::Settings),
                MenuItem::separator(),
                MenuItem::action("Quit DROSTJE.", ui::commands::Quit),
            ])]);
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(1440.), px(900.)), cx)),
                window_min_size: Some(size(px(900.), px(600.))),
                window_decorations: Some(WindowDecorations::Client),
                app_id: Some("drostje".into()),
                ..TitleBar::window_options()
            };
            cx.spawn(async move |cx| {
                match cx.open_window(options, move |window, cx| {
                    let view: AnyView = match bootstrap {
                        Ok(bootstrap) => {
                            let state = cx.new(|cx| app::AppState::new(bootstrap, cx));
                            let workspace =
                                cx.new(|cx| ui::workspace::Workspace::new(state, window, cx));
                            cx.new(|_| ui::workspace::WorkspaceRoot { workspace })
                                .into()
                        }
                        Err(error) => {
                            cx.on_action(|_: &ui::commands::Quit, cx| cx.quit());
                            ui::theme::attach_recovery(window, cx).detach();
                            cx.new(|_| ui::workspace::Recovery {
                                error: format!("{error:#}"),
                            })
                            .into()
                        }
                    };
                    cx.new(|cx| Root::new(view, window, cx))
                }) {
                    Ok(_) => {
                        cx.update(|cx| cx.activate(true));
                    }
                    Err(error) => eprintln!("Could not open DROSTJE.: {error:#}"),
                }
            })
            .detach();
        });
}

#[cfg(test)]
mod tests {
    use super::APP_ASSETS;
    use gpui_kit::{AssetSource, SvgRenderer, assets::IconName};
    use std::sync::Arc;

    #[test]
    fn application_icons_resolve_and_paint_visible_pixels() {
        let renderer = SvgRenderer::new(Arc::new(APP_ASSETS));
        for icon in IconName::ALL {
            let path = icon.path();
            let bytes = APP_ASSETS
                .load(&path)
                .unwrap_or_else(|error| panic!("Cannot load {path}: {error}"))
                .unwrap_or_else(|| panic!("Missing icon {path}"));
            let image = renderer
                .render_single_frame(&bytes, 1.0)
                .unwrap_or_else(|error| panic!("Cannot render {path}: {error}"));
            assert!(
                image
                    .as_bytes(0)
                    .expect("SVG renderer returns one frame")
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|pixel| pixel[3] != 0),
                "{path} rendered entirely transparent"
            );
        }
    }
}
