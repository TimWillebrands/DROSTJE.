use super::Palette;
use crate::app::{AppState, BranchAction, BranchChoice, FetchStatus, NoticeLevel};
use crate::ui::workspace::Workspace;
use gpui_kit::{
    component::{
        ActiveTheme, Disableable, Sizable, WindowExt,
        alert::Alert,
        command::{Command as NativeCommand, CommandGroup, CommandItem},
        h_flex,
        spinner::Spinner,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use std::rc::Rc;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::ui) enum BranchTarget {
    Switch(String),
    Create(String),
    Open(Option<Uuid>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::ui) struct BranchRow {
    pub(in crate::ui) name: String,
    pub(in crate::ui) detail: String,
    pub(in crate::ui) target: Option<BranchTarget>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::ui) struct BranchSection {
    pub(in crate::ui) label: &'static str,
    pub(in crate::ui) rows: Vec<BranchRow>,
}

pub(in crate::ui) fn branch_sections(choices: &[BranchChoice], query: &str) -> Vec<BranchSection> {
    let row = |choice: &BranchChoice| BranchRow {
        name: choice.name.clone(),
        detail: choice.detail.clone(),
        target: match choice.action {
            BranchAction::Switch => Some(BranchTarget::Switch(choice.name.clone())),
            BranchAction::OpenWorkspace(worktree) => Some(BranchTarget::Open(worktree)),
            BranchAction::Current | BranchAction::Unavailable => None,
        },
    };
    let mut sections = vec![
        BranchSection {
            label: "Branches",
            rows: choices.iter().filter(|c| !c.remote).map(row).collect(),
        },
        BranchSection {
            label: "Remote branches",
            rows: choices.iter().filter(|c| c.remote).map(row).collect(),
        },
    ];
    let name = query.trim();
    if !name.is_empty() && choices.iter().all(|choice| choice.name != name) {
        sections.push(BranchSection {
            label: "New branch",
            rows: vec![BranchRow {
                name: format!("Create branch {name} from HEAD"),
                detail: String::new(),
                target: Some(BranchTarget::Create(name.to_owned())),
            }],
        });
    }
    sections.retain(|section| !section.rows.is_empty());
    sections
}

fn fetch_text(fetch: Option<&FetchStatus>) -> Option<(String, bool)> {
    match fetch? {
        FetchStatus::Fetching => Some(("Fetching from remotes…".into(), false)),
        FetchStatus::Fetched => Some(("Fetched from remotes".into(), false)),
        FetchStatus::NoRemotes => Some(("No remotes; local branches only".into(), false)),
        FetchStatus::Failed(error) => Some((
            format!("Fetch failed, showing the last fetched state: {error}"),
            true,
        )),
    }
}

fn tree_name(state: &AppState, thread: Uuid) -> String {
    state
        .snapshot
        .threads
        .iter()
        .find(|t| t.id == thread)
        .map(|thread| match state.thread_worktree(thread) {
            Some(worktree) => format!("worktree {}", worktree.codename),
            None => "the current checkout".to_owned(),
        })
        .unwrap_or_else(|| "this working tree".to_owned())
}

impl Palette {
    pub fn open_branches(
        owner: WeakEntity<Workspace>,
        state: Entity<AppState>,
        thread: Uuid,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        let palette = Self::open(owner, state, window, cx);
        palette.update(cx, |palette, cx| {
            palette.branches = Some(thread);
            cx.notify();
        });
        palette
    }

    pub(super) fn render_branches(&self, thread: Uuid, cx: &mut Context<Self>) -> AnyElement {
        let state = self.state.read(cx);
        let choices = state.branch_choices(thread);
        let list = state
            .snapshot
            .threads
            .iter()
            .find(|t| t.id == thread)
            .and_then(|thread| state.branch_list(thread.project_id));
        let fetching = list.is_some_and(|list| list.fetch == Some(FetchStatus::Fetching));
        let fetch = list.and_then(|list| fetch_text(list.fetch.as_ref()));
        let tree = tree_name(state, thread);
        let empty: SharedString = match &choices {
            None => "Reading branches…".into(),
            Some(Err(error)) => format!("Branches are unavailable: {error}").into(),
            Some(Ok(_)) => "Type a name to create a branch".into(),
        };
        let query = self.command.read(cx).query(cx);
        let sections = match &choices {
            Some(Ok(choices)) => branch_sections(choices, &query),
            _ => Vec::new(),
        };
        let mut command = NativeCommand::new(&self.command)
            .bordered(false)
            .placeholder("Search branches or name a new one…")
            .max_h(rems(26.));
        let mut targets = Vec::new();
        for section in sections {
            let mut group = CommandGroup::new().label(section.label);
            let mut rows = Vec::new();
            for row in section.rows {
                let name = row.name.clone();
                let detail = row.detail.clone();
                group = group.item(
                    CommandItem::new()
                        .label(row.name)
                        .keywords([row.detail])
                        .disabled(row.target.is_none())
                        .child(move |_, cx| branch_row(&name, &detail, cx)),
                );
                rows.push(row.target);
            }
            command = command.group(group);
            targets.push(rows);
        }
        let targets = Rc::new(targets);
        let confirm = cx.entity().downgrade();
        let cancel = cx.entity().downgrade();
        let requery = cx.entity().downgrade();
        div()
            .id("branch-picker")
            .test_support()
            .key_context("BranchPicker")
            .w_full()
            .child(
                command
                    .empty(move |_, _, cx| {
                        div()
                            .id("branch-picker-empty")
                            .test_support()
                            .p_3()
                            .text_color(cx.theme().muted_foreground)
                            .child(empty.clone())
                    })
                    .footer(move |_, _, cx| {
                        let (note, failure) = match fetch.clone() {
                            Some((text, true)) => (None, Some(text)),
                            Some((text, false)) => (Some(text), None),
                            None => (None, None),
                        };
                        v_flex()
                            .id("branch-picker-status")
                            .test_support()
                            .w_full()
                            .gap_2()
                            .px_3()
                            .py_2()
                            .border_t_1()
                            .border_color(cx.theme().border)
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                h_flex()
                                    .w_full()
                                    .min_w_0()
                                    .gap_2()
                                    .when(fetching, |row| row.child(Spinner::new().xsmall()))
                                    .child(div().flex_1().min_w_0().truncate().child(format!(
                                        "Switches {tree}; uncommitted changes block it"
                                    )))
                                    .when_some(note, |row, note| {
                                        row.child(
                                            div()
                                                .id("branch-picker-fetch")
                                                .test_support()
                                                .flex_shrink_0()
                                                .child(note),
                                        )
                                    }),
                            )
                            .when_some(failure, |column, failure| {
                                column.child(div().id("branch-picker-fetch").test_support().child(
                                    Alert::warning("branch-picker-fetch-failure", failure).small(),
                                ))
                            })
                    })
                    .on_query(move |_, _, cx| {
                        let _ = requery.update(cx, |_, cx| cx.notify());
                    })
                    .on_confirm(move |index, window, cx| {
                        let Some(target) = targets
                            .get(index.section)
                            .and_then(|rows| rows.get(index.row))
                            .cloned()
                            .flatten()
                        else {
                            return;
                        };
                        let _ = confirm.update(cx, |palette, cx| {
                            palette.run_branch(thread, target, window, cx)
                        });
                    })
                    .on_cancel(move |_, cx| {
                        let _ = cancel.update(cx, |palette, cx| palette.cleanup(cx));
                    }),
            )
            .into_any_element()
    }

    fn run_branch(
        &mut self,
        thread: Uuid,
        target: BranchTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.closed || !self.hosted {
            return;
        }
        self.closed = true;
        self.hosted = false;
        let id = cx.entity().entity_id();
        let _ = self.owner.update(cx, |workspace, cx| {
            if !workspace
                .palette
                .as_ref()
                .is_some_and(|palette| palette.entity_id() == id)
            {
                return;
            }
            workspace.palette = None;
            window.close_dialog(cx);
            let selected = workspace
                .state
                .read(cx)
                .conversation_thread()
                .is_some_and(|t| t.id == thread);
            let (branch, create) = match target {
                _ if !selected => {
                    workspace.state.update(cx, |state, cx| {
                        state.notice(
                            NoticeLevel::Warning,
                            "Thread selection changed; reopen Switch branch.",
                        );
                        cx.notify();
                    });
                    return;
                }
                BranchTarget::Open(worktree) => {
                    workspace.open_conversation_in(worktree, window, cx);
                    return;
                }
                BranchTarget::Switch(branch) => (branch, false),
                BranchTarget::Create(branch) => (branch, true),
            };
            workspace.state.update(cx, |state, cx| {
                if let Err(error) = state.switch_branch(&branch, create) {
                    state.notice(NoticeLevel::Warning, format!("{error:#}"));
                }
                cx.notify();
            });
            cx.notify();
        });
    }
}

fn branch_row(name: &str, detail: &str, cx: &App) -> Div {
    let theme = cx.theme();
    h_flex()
        .w_full()
        .min_w_0()
        .gap_3()
        .child(
            div()
                .flex_shrink_0()
                .min_w_0()
                .truncate()
                .font_family(theme.mono_font_family.clone())
                .child(name.to_owned()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_right()
                .text_color(theme.muted_foreground)
                .child(detail.to_owned()),
        )
}

#[cfg(all(test, feature = "runtime-qa"))]
#[path = "branches_ui_tests.rs"]
mod ui_tests;

#[cfg(test)]
mod tests {
    use super::{BranchTarget, branch_sections};
    use crate::app::{BranchAction, BranchChoice};
    use uuid::Uuid;

    fn choice(name: &str, action: BranchAction, remote: bool) -> BranchChoice {
        BranchChoice {
            name: name.into(),
            action,
            detail: String::new(),
            remote,
        }
    }

    #[test]
    fn local_and_remote_branches_are_grouped_and_a_new_name_offers_creation() {
        let otter = Uuid::new_v4();
        let choices = [
            choice("main", BranchAction::Current, false),
            choice("feature", BranchAction::OpenWorkspace(Some(otter)), false),
            choice("elsewhere", BranchAction::Unavailable, false),
            choice("topic", BranchAction::Switch, false),
            choice("theirs", BranchAction::Switch, true),
        ];
        let sections = branch_sections(&choices, "");
        assert_eq!(
            sections
                .iter()
                .map(|section| (
                    section.label,
                    section
                        .rows
                        .iter()
                        .map(|row| (row.name.as_str(), row.target.clone()))
                        .collect::<Vec<_>>()
                ))
                .collect::<Vec<_>>(),
            [
                (
                    "Branches",
                    vec![
                        ("main", None),
                        ("feature", Some(BranchTarget::Open(Some(otter)))),
                        ("elsewhere", None),
                        ("topic", Some(BranchTarget::Switch("topic".into()))),
                    ]
                ),
                (
                    "Remote branches",
                    vec![("theirs", Some(BranchTarget::Switch("theirs".into())))]
                ),
            ]
        );

        let typed = branch_sections(&choices, "  release/2 ");
        let created = typed.last().unwrap();
        assert_eq!(created.label, "New branch");
        assert_eq!(created.rows[0].name, "Create branch release/2 from HEAD");
        assert_eq!(
            created.rows[0].target,
            Some(BranchTarget::Create("release/2".into()))
        );

        assert!(
            branch_sections(&choices, "topic")
                .iter()
                .all(|section| section.label != "New branch"),
            "an existing name is switched to, not created"
        );
        assert!(branch_sections(&[], "").is_empty());
    }
}
