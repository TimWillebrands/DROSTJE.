use gpui_kit::{
    assets::IconName,
    component::{
        ActiveTheme, Icon, Sizable, Size,
        alert::Alert,
        button::{Button, ButtonVariants},
        spinner::Spinner,
    },
    prelude::FluentBuilder,
    *,
};

pub const BAR_HEIGHT: f32 = 56.;
pub const ROW_HEIGHT: f32 = 64.;
pub const TOUCH: f32 = 48.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Good,
    Warn,
    Bad,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Emphasis {
    Primary,
    Secondary,
    Danger,
}

pub fn tone_color(tone: Tone, cx: &App) -> Hsla {
    match tone {
        Tone::Neutral => cx.theme().muted_foreground,
        Tone::Good => cx.theme().success,
        Tone::Warn => cx.theme().warning,
        Tone::Bad => cx.theme().danger,
    }
}

pub fn icon(name: IconName, size: f32, color: Hsla) -> Icon {
    Icon::new(name).size(px(size)).text_color(color)
}

pub fn spinner(size: f32, color: Hsla) -> Spinner {
    Spinner::new().with_size(px(size)).color(color)
}

pub fn dot(color: Hsla) -> Div {
    div().flex_none().size(px(10.)).rounded_full().bg(color)
}

pub fn icon_button(id: impl Into<ElementId>, name: IconName, label: &'static str) -> Button {
    Button::new(id)
        .ghost()
        .with_size(Size::Large)
        .icon(name)
        .accessibility_label(label)
        .flex_none()
        .size(px(TOUCH))
}

pub fn wide_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    leading: Option<IconName>,
    emphasis: Emphasis,
) -> Button {
    let button = Button::new(id)
        .label(label)
        .with_size(Size::Large)
        .w_full()
        .h(px(TOUCH));
    let button = match emphasis {
        Emphasis::Primary => button.primary(),
        Emphasis::Secondary => button.secondary(),
        Emphasis::Danger => button.danger(),
    };
    match leading {
        Some(name) => button.icon(name),
        None => button,
    }
}

pub fn app_bar(
    leading: Option<Button>,
    title: impl Into<SharedString>,
    subtitle: Option<(SharedString, Tone)>,
    trailing: Option<Button>,
    cx: &App,
) -> Div {
    let title = title.into();
    div()
        .flex_none()
        .h(px(BAR_HEIGHT))
        .w_full()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .px_1()
        .when(leading.is_none(), |bar| bar.pl_4())
        .when_some(leading, |bar, leading| bar.child(leading))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .truncate()
                        .child(title),
                )
                .when_some(subtitle, |column, (subtitle, tone)| {
                    column.child(
                        div()
                            .text_sm()
                            .truncate()
                            .text_color(tone_color(tone, cx))
                            .child(subtitle),
                    )
                }),
        )
        .when_some(trailing, |bar, trailing| bar.child(trailing))
}

pub fn banner(
    id: impl Into<ElementId>,
    name: IconName,
    text: impl Into<SharedString>,
    tone: Tone,
) -> Alert {
    let text: SharedString = text.into();
    let alert = match tone {
        Tone::Neutral => Alert::info(id, text),
        Tone::Good => Alert::success(id, text),
        Tone::Warn => Alert::warning(id, text),
        Tone::Bad => Alert::error(id, text),
    };
    alert.icon(name)
}

pub fn section(title: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .px_4()
        .pt_5()
        .pb_1()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(cx.theme().muted_foreground)
        .child(title.into().to_uppercase())
}

pub fn row(
    id: impl Into<ElementId>,
    leading: AnyElement,
    title: impl Into<SharedString>,
    subtitle: Option<SharedString>,
    trailing: Option<AnyElement>,
    cx: &App,
) -> Stateful<Div> {
    div()
        .id(id)
        .w_full()
        .min_h(px(ROW_HEIGHT))
        .px_4()
        .py_2()
        .flex()
        .flex_row()
        .items_center()
        .gap_4()
        .cursor_pointer()
        .active(|style| style.bg(cx.theme().list_active))
        .child(
            div()
                .flex_none()
                .size(px(28.))
                .flex()
                .items_center()
                .justify_center()
                .child(leading),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(div().text_base().truncate().child(title.into()))
                .when_some(subtitle, |column, subtitle| {
                    column.child(
                        div()
                            .text_sm()
                            .truncate()
                            .text_color(cx.theme().muted_foreground)
                            .child(subtitle),
                    )
                }),
        )
        .when_some(trailing, |row, trailing| row.child(trailing))
        .child(icon(
            IconName::ChevronRight,
            20.,
            cx.theme().muted_foreground,
        ))
}

pub fn empty_state(
    name: IconName,
    title: impl Into<SharedString>,
    body: impl Into<SharedString>,
    cx: &App,
) -> Div {
    div()
        .flex_1()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_3()
        .px_8()
        .child(icon(name, 48., cx.theme().muted_foreground))
        .child(
            div()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .text_center()
                .child(title.into()),
        )
        .child(
            div()
                .text_sm()
                .text_center()
                .text_color(cx.theme().muted_foreground)
                .child(body.into()),
        )
}

pub fn loading(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .flex_1()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_3()
        .child(spinner(32., cx.theme().muted_foreground))
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(text.into()),
        )
}
