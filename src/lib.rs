pub mod app;
pub mod attachments;
pub mod factory;
pub mod harness;
pub mod model;
pub mod neovim;
pub mod platform;
pub mod store;
pub mod titles;
pub mod ui;

use gpui_kit::{
    assets::Assets,
    component::{Root, TitleBar},
    *,
};

pub fn run() {
    let bootstrap = app::Bootstrap::load();
    if let Err(error) = &bootstrap {
        if matches!(
            error.downcast_ref::<store::StoreError>(),
            Some(store::StoreError::InstanceInUse(_))
        ) {
            eprintln!("DROSTJE. is already using this data directory: {error:#}");
            return;
        }
    }
    gpui_kit::application().with_assets(Assets).run(move |cx| {
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
            // Native window geometry is expressed in physical layout pixels.
            window_bounds: Some(WindowBounds::centered(size(px(1440.), px(900.)), cx)),
            window_min_size: Some(size(px(900.), px(600.))),
            // Ask for client-side decorations so compositors do not stack their own title bar
            // above ours; `Root` draws the Linux frame, shadow and resize edges.
            window_decorations: Some(WindowDecorations::Client),
            // The Wayland/X11 application id. Desktop launchers match it to
            // `drostje.desktop` and its icon; without it the window is anonymous.
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
