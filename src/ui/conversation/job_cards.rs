use super::{
    blocks::guarded_text,
    disclosure::{disclosure, is_expanded, toggle},
    job_notice::{JobOutput, JobResult},
    markdown::{Markdown, code_block, markdown_text},
    mono,
};
use crate::ui::workspace::Workspace;
use gpui_kit::{
    assets::IconName as Lucide,
    component::{ActiveTheme, Icon, Sizable, h_flex, tag::Tag, v_flex},
    prelude::FluentBuilder,
    *,
};
use serde_json::Value;

/// Unrecognised notices remain inspectable, without exposing transport markup
/// or pretending that a new harness format is a completed background job.
pub(super) fn system_notice(
    key: String,
    text: &str,
    owner: &WeakEntity<Workspace>,
    markdown: Markdown,
    cx: &App,
) -> AnyElement {
    let open = is_expanded(owner, &key, cx);
    let mut card = v_flex()
        .id(SharedString::from(format!("{key}:notice")))
        .test_support()
        .w_full()
        .min_w_0()
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .py_1()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(Icon::new(Lucide::Info).xsmall())
                .child(div().flex_1().child("System notice"))
                .child(disclosure(
                    key.clone(),
                    open,
                    owner,
                    markdown.thread(),
                    if open { "Hide notice" } else { "Show notice" },
                )),
        );
    if open {
        card = card.child(
            div()
                .id(SharedString::from(format!("{key}:notice-body")))
                .test_support()
                .min_w_0()
                .px_3()
                .py_2()
                .border_1()
                .border_color(cx.theme().border)
                .rounded(cx.theme().radius)
                .child(guarded_text(
                    key.clone(),
                    text,
                    owner,
                    markdown.thread(),
                    cx,
                    || code_block(format!("{key}:text"), text, "text", markdown).into_any_element(),
                )),
        );
    }
    card.into_any_element()
}

/// One finished background job from a `<system-notice>`: a task agent's report
/// or a shell command's output. A structured report opens by default; raw
/// output folds behind the disclosure like any tool result.
pub(super) fn job_card(
    key: String,
    job: &JobResult,
    owner: &WeakEntity<Workspace>,
    markdown: Markdown,
    cx: &App,
) -> AnyElement {
    let structured = matches!(job.output, JobOutput::Structured(_));
    let has_output = !matches!(job.output, JobOutput::Empty);
    let open = has_output && (structured != is_expanded(owner, &key, cx));
    let status = job.status.as_deref().unwrap_or("Finished");
    let tag = if status.eq_ignore_ascii_case("failed") || status.eq_ignore_ascii_case("error") {
        Tag::danger()
    } else {
        Tag::secondary()
    }
    .outline()
    .small()
    .child(status.to_owned());
    let mut header = h_flex()
        .id(SharedString::from(format!("{key}:header")))
        .test_support()
        .w_full()
        .items_center()
        .gap_2()
        .py_1()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(
            Icon::new(if job.agent.is_some() {
                Lucide::Workflow
            } else {
                Lucide::Terminal
            })
            .xsmall(),
        )
        .child(
            div()
                .flex_shrink_0()
                .max_w(relative(0.4))
                .truncate()
                .child(format!("Background job {}", job.id)),
        )
        .child(
            mono(cx)
                .flex_1()
                .truncate()
                .when_some(job.label.as_ref(), |label, text| label.child(text.clone())),
        );
    for detail in [job.agent.as_deref(), job.duration.as_deref()]
        .into_iter()
        .flatten()
    {
        header = header.child(div().flex_shrink_0().child(detail.to_owned()));
    }
    if !has_output {
        header = header.child(div().flex_shrink_0().child("No output"));
    }
    header = header.child(tag);
    if has_output {
        header = header
            .on_click(toggle(owner, markdown.thread(), key.clone()))
            .child(disclosure(
                key.clone(),
                open,
                owner,
                markdown.thread(),
                if open { "Hide output" } else { "Show output" },
            ));
    }
    let mut card = v_flex()
        .id(SharedString::from(format!("{key}:card")))
        .test_support()
        .w_full()
        .min_w_0()
        .child(header);
    if open {
        let mut output = v_flex()
            .id(SharedString::from(format!("{key}:body")))
            .test_support()
            .w_full()
            .min_w_0()
            .gap_2()
            .mb_1()
            .px_3()
            .py_2()
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius);
        if let Some(label) = &job.label {
            output = output.child(mono(cx).child(label.clone()));
        }
        if let Some(full) = &job.full_output {
            output = output.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("Preview only; the full output is at {full}")),
            );
        }
        output = match &job.output {
            JobOutput::Structured(value) => output.child(structured_view(
                format!("{key}:structured"),
                value,
                0,
                markdown,
                cx,
            )),
            JobOutput::Text(text) => {
                let language = if text.trim_start().starts_with(['{', '[']) {
                    "json"
                } else {
                    "text"
                };
                let identity = format!("{key}:output");
                let markdown_id = format!("{identity}:markdown");
                output.child(guarded_text(
                    identity,
                    text,
                    owner,
                    markdown.thread(),
                    cx,
                    || code_block(markdown_id, text, language, markdown).into_any_element(),
                ))
            }
            JobOutput::Empty => output,
        };
        card = card.child(output);
    }
    card.into_any_element()
}
/// Generic reading view for a job's JSON report: keys become captions, strings
/// render as Markdown, string lists as bullets, nested objects indent. Beyond a
/// few levels the raw JSON is shown so nothing is hidden.
fn structured_view(
    identity: String,
    value: &Value,
    depth: usize,
    markdown: Markdown,
    cx: &App,
) -> AnyElement {
    const MAX_DEPTH: usize = 3;
    match value {
        Value::Object(fields) if depth < MAX_DEPTH && !fields.is_empty() => {
            let mut column = v_flex().gap_2().min_w_0();
            for (key, value) in fields {
                let caption = humanise_key(key);
                column = column.child(
                    v_flex()
                        .gap_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(caption),
                        )
                        .child(structured_view(
                            format!("{identity}:field:{key}"),
                            value,
                            depth + 1,
                            markdown,
                            cx,
                        )),
                );
            }
            column.into_any_element()
        }
        Value::Array(items)
            if depth < MAX_DEPTH
                && !items.is_empty()
                && items.iter().all(|item| item.is_string()) =>
        {
            let list = items
                .iter()
                .filter_map(Value::as_str)
                .map(|item| format!("- {}", item.replace('\n', "\n  ")))
                .collect::<Vec<_>>()
                .join("\n");
            markdown_text(identity, &list, markdown).into_any_element()
        }
        Value::Array(items)
            if depth < MAX_DEPTH
                && !items.is_empty()
                && items.iter().all(|item| item.is_object()) =>
        {
            let mut column = v_flex().gap_2().min_w_0();
            for (index, item) in items.iter().enumerate() {
                column = column.child(
                    div()
                        .pl_3()
                        .border_l_1()
                        .border_color(cx.theme().border)
                        .child(structured_view(
                            format!("{identity}:item:{index}"),
                            item,
                            depth + 1,
                            markdown,
                            cx,
                        )),
                );
            }
            column.into_any_element()
        }
        Value::String(text) => markdown_text(identity, text, markdown).into_any_element(),
        Value::Null => div()
            .text_color(cx.theme().muted_foreground)
            .child("None")
            .into_any_element(),
        Value::Bool(_) | Value::Number(_) => mono(cx).child(value.to_string()).into_any_element(),
        Value::Object(_) | Value::Array(_) => code_block(
            identity,
            &serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()),
            "json",
            markdown,
        )
        .into_any_element(),
    }
}
/// `tests_added` → `Tests added`, `main_required` → `Main required`.
fn humanise_key(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    for (index, ch) in key.chars().enumerate() {
        let ch = if ch == '_' || ch == '-' { ' ' } else { ch };
        if index == 0 {
            out.extend(ch.to_uppercase());
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::humanise_key;

    #[test]
    fn report_keys_read_as_captions() {
        assert_eq!(humanise_key("tests_added"), "Tests added");
        assert_eq!(humanise_key("main-required"), "Main required");
        assert_eq!(humanise_key("Summary"), "Summary");
        assert_eq!(humanise_key(""), "");
    }
}
