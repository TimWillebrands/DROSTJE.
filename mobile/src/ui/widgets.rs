use gpui_kit::{
    assets::IconName,
    base::TestSupportExt,
    component::{ActiveTheme, Icon, Sizable, spinner::Spinner},
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

pub fn icon_button(id: impl Into<ElementId>, name: IconName, cx: &App) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .size(px(TOUCH))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .cursor_pointer()
        .active(|style| style.bg(cx.theme().list_active))
        .child(icon(name, 24., cx.theme().foreground))
}

pub fn app_bar(
    leading: Option<Stateful<Div>>,
    title: impl Into<SharedString>,
    subtitle: Option<(SharedString, Tone)>,
    trailing: Option<Stateful<Div>>,
    cx: &App,
) -> Div {
    let leading = leading.map(TestSupportExt::test_support);
    let trailing = trailing.map(TestSupportExt::test_support);
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

pub fn banner(name: IconName, text: impl Into<SharedString>, tone: Tone, cx: &App) -> Div {
    let color = tone_color(tone, cx);
    div()
        .flex_none()
        .mx_4()
        .mb_2()
        .px_3()
        .py_2()
        .rounded(cx.theme().radius_lg)
        .bg(color.opacity(0.12))
        .flex()
        .flex_row()
        .items_center()
        .gap_3()
        .child(icon(name, 20., color))
        .child(div().flex_1().min_w_0().text_sm().child(text.into()))
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

pub fn wide_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    leading: Option<IconName>,
    primary: bool,
    danger: bool,
    cx: &App,
) -> Stateful<Div> {
    let (background, foreground, pressed) = if danger {
        (
            cx.theme().danger,
            cx.theme().danger_foreground,
            cx.theme().danger_active,
        )
    } else if primary {
        (
            cx.theme().primary,
            cx.theme().primary_foreground,
            cx.theme().primary_active,
        )
    } else {
        (
            cx.theme().secondary,
            cx.theme().secondary_foreground,
            cx.theme().secondary_active,
        )
    };
    div()
        .id(id)
        .w_full()
        .h(px(TOUCH + 4.))
        .rounded_full()
        .bg(background)
        .text_color(foreground)
        .flex()
        .flex_row()
        .items_center()
        .justify_center()
        .gap_2()
        .cursor_pointer()
        .active(move |style| style.bg(pressed))
        .when_some(leading, |button, name| {
            button.child(icon(name, 22., foreground))
        })
        .child(
            div()
                .text_base()
                .font_weight(FontWeight::SEMIBOLD)
                .child(label.into()),
        )
}
