pub mod app;
pub mod attachments;
pub mod factory;
pub mod harness;
pub mod model;
pub mod neovim;
pub mod platform;
pub mod store;
pub mod ui;

use gpui_kit::{
    assets::Assets,
    component::{Root, Theme, ThemeMode},
    *,
};

pub fn run() {
    let bootstrap = app::Bootstrap::load();
    if let Err(error) = &bootstrap {
        if matches!(
            error.downcast_ref::<store::StoreError>(),
            Some(store::StoreError::InstanceInUse(_))
        ) {
            eprintln!("Drost is already using this data directory: {error:#}");
            return;
        }
    }
    gpui_kit::application().with_assets(Assets).run(move |cx| {
        gpui_kit::init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        {
            let theme = Theme::global_mut(cx);
            theme.font_family = ".SystemUIFont".into();
            theme.font_size = px(13.);
            theme.mono_font_family = "Menlo".into();
            theme.colors.background = rgb(0x181B20).into();
            theme.colors.foreground = rgb(0xD8DEE9).into();
            theme.colors.border = rgb(0x343A46).into();
            theme.colors.muted = rgb(0x20242B).into();
            theme.colors.muted_foreground = rgb(0x8B95A5).into();
            theme.colors.primary = rgb(0x7AA2F7).into();
            theme.colors.accent = rgb(0x343A46).into();
        }
        Theme::sync_base(cx);
        ui::commands::bind_keys(cx);
        cx.on_action(|_: &ui::commands::Quit, cx| cx.quit());
        cx.set_menus(vec![Menu::new("Drost").items([
            MenuItem::action("Settings…", ui::commands::Settings),
            MenuItem::separator(),
            MenuItem::action("Quit Drost", ui::commands::Quit),
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
            titlebar: Some(TitlebarOptions {
                title: Some("Drost".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        cx.spawn(async move |cx| {
            match cx.open_window(options, move |window, cx| {
                let view: AnyView = match bootstrap {
                    Ok(bootstrap) => {
                        let state = cx.new(|cx| app::AppState::new(bootstrap, cx));
                        cx.new(|cx| ui::workspace::Workspace::new(state, window, cx))
                            .into()
                    }
                    Err(error) => cx
                        .new(|_| ui::workspace::Recovery {
                            error: format!("{error:#}"),
                        })
                        .into(),
                };
                cx.new(|cx| Root::new(view, window, cx))
            }) {
                Ok(_) => {
                    let _ = cx.update(|cx| cx.activate(true));
                }
                Err(error) => eprintln!("Could not open Drost: {error:#}"),
            }
        })
        .detach();
    });
}
