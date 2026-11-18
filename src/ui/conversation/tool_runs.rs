use super::{
    disclosure::{disclosure, toggle},
    mono,
    spinner::Spinner,
    tools::tool_icon,
};
use crate::ui::workspace::Workspace;
use crate::{
    app::ConversationSession,
    model::ThreadId,
    transcript::{RunPart, ToolRun, summary_line, tool_run, tool_summary},
};
use gpui_kit::{
    assets::IconName as Lucide,
    component::{ActiveTheme, Icon, Sizable, h_flex},
    prelude::FluentBuilder,
    *,
};
use std::{cell::RefCell, collections::HashMap, rc::Rc};
use uuid::Uuid;

#[derive(Clone, Default)]
pub(super) struct RunCache(Rc<RefCell<RunCacheState>>);

#[derive(Default)]
struct RunCacheState {
    revision: Option<(Uuid, u64)>,
    runs: HashMap<usize, Option<ToolRun>>,
}

impl RunCache {
    pub(super) fn run(&self, session: &ConversationSession, index: usize) -> Option<ToolRun> {
        let revision = (session.generation, session.transcript_revision);
        let len = session.messages.len();
        self.find(revision, len, index, |ix| session.run_part(ix))
    }

    fn find(
        &self,
        revision: (Uuid, u64),
        len: usize,
        index: usize,
        part: impl Fn(usize) -> RunPart,
    ) -> Option<ToolRun> {
        let mut cache = self.0.borrow_mut();
        if cache.revision != Some(revision) {
            cache.revision = Some(revision);
            cache.runs.clear();
        }
        if let Some(run) = cache.runs.get(&index) {
            return *run;
        }
        let run = tool_run(part, len, index);
        match run {
            Some(found) => cache
                .runs
                .extend((found.head..found.end).map(|ix| (ix, run))),
            None => {
                cache.runs.insert(index, None);
            }
        }
        run
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the run header takes the row context message() renders with"
)]
pub(super) fn tool_run_header(
    thread: ThreadId,
    key: String,
    run: ToolRun,
    expanded: bool,
    session: &ConversationSession,
    active: bool,
    owner: &WeakEntity<Workspace>,
    cx: &App,
) -> AnyElement {
    let tally = session.run_tally(run, active);
    let latest = session
        .run_calls(run)
        .last()
        .filter(|_| !expanded)
        .map(|call| {
            (
                call.intent.unwrap_or(call.name).to_owned(),
                tool_summary(call.name, call.input),
            )
        });
    let group: SharedString = format!("{key}:group").into();
    h_flex()
        .id(SharedString::from(format!("{key}:header")))
        .debug_selector(|| "conversation-tool-run".into())
        .test_support()
        .group(group.clone())
        .w_full()
        .items_center()
        .gap_2()
        .py_1()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .on_click(toggle(owner, Some(thread), key.clone()))
        .child(Icon::new(Lucide::Wrench).xsmall())
        .child(
            div()
                .flex_shrink_0()
                .text_color(cx.theme().foreground)
                .child(tally.title()),
        )
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .gap_2()
                .when_some(latest, |row, (caption, summary)| {
                    row.child(Icon::new(tool_icon(summary.kind)).xsmall())
                        .child(
                            div()
                                .flex_shrink_0()
                                .max_w(relative(0.5))
                                .truncate()
                                .child(caption),
                        )
                        .child(
                            mono(cx)
                                .flex_1()
                                .truncate()
                                .child(summary_line(&summary.body)),
                        )
                }),
        )
        .when_some(tally.without_result(), |row, label| {
            row.child(div().flex_shrink_0().text_xs().child(label))
        })
        .when_some(tally.failures(), |row, label| {
            row.child(
                h_flex()
                    .flex_shrink_0()
                    .gap_1()
                    .items_center()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child(Icon::new(Lucide::CircleX).xsmall())
                    .child(label),
            )
        })
        .when(tally.running > 0, |row| row.child(Spinner::new().xsmall()))
        .child(
            div()
                .when(!expanded, |chevron| {
                    chevron
                        .invisible()
                        .group_hover(group, |style| style.visible())
                })
                .child(disclosure(
                    key,
                    expanded,
                    owner,
                    Some(thread),
                    if expanded {
                        "Hide tool calls"
                    } else {
                        "Show tool calls"
                    },
                )),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{RunCache, RunPart, ToolRun};

    #[test]
    fn run_cache_finds_each_run_once_per_revision() {
        use RunPart::{Break, Calls, Opens};
        use std::cell::Cell;
        let parts = [Break, Opens(1), Calls(1), Calls(2), Break, Calls(1)];
        let looked = Cell::new(0);
        let part = |ix: usize| {
            looked.set(looked.get() + 1);
            parts[ix]
        };
        let cache = RunCache::default();
        let revision = (uuid::Uuid::nil(), 1);
        let run = Some(ToolRun {
            head: 1,
            end: 4,
            calls: 4,
        });
        assert_eq!(cache.find(revision, parts.len(), 2, part), run);
        let first = looked.get();
        for index in 1..4 {
            assert_eq!(cache.find(revision, parts.len(), index, part), run);
        }
        assert_eq!(cache.find(revision, parts.len(), 0, part), None);
        assert_eq!(cache.find(revision, parts.len(), 0, part), None);
        assert_eq!(
            looked.get(),
            first + 1,
            "rows of a found run and a row without one are not looked at again"
        );
        assert_eq!(
            cache.find((uuid::Uuid::nil(), 2), parts.len(), 3, part),
            run
        );
        assert!(looked.get() > first + 1);
    }
}
