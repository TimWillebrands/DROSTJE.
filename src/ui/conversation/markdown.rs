use crate::{
    app::ConversationSession,
    file_refs::{self, FileReference},
    harness::ContentBlock,
    model::ThreadId,
};
use gpui_kit::{
    component::{
        ActiveTheme,
        button::{Button, ButtonVariants},
        text::{
            InlineElement, InlineRenderContext, MarkdownNode, MarkdownParseContext, MarkdownPlugin,
            TextView, TextViewState, TextViewStyle, markdown_ast,
        },
    },
    *,
};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
};

/// Keep HTML and Markdown image references inert. Only decoded provider attachments
/// may load image resources; ordinary assistant prose cannot name a local file to load.
struct InertResources;
impl MarkdownPlugin for InertResources {
    fn name(&self) -> &str {
        "drostje-inert-resources"
    }
    fn is_block(&self) -> bool {
        true
    }
    fn parse(
        &self,
        node: &markdown_ast::Node,
        context: &MarkdownParseContext<'_>,
    ) -> Option<MarkdownNode> {
        fn unsafe_resource(node: &markdown_ast::Node) -> bool {
            matches!(
                node,
                markdown_ast::Node::Html(_)
                    | markdown_ast::Node::Image(_)
                    | markdown_ast::Node::ImageReference(_)
            ) || node
                .children()
                .is_some_and(|children| children.iter().any(unsafe_resource))
        }
        if matches!(node, markdown_ast::Node::Root(_)) || !unsafe_resource(node) {
            return None;
        }
        let text = context.node_source(node).unwrap_or_default().to_owned();
        Some(
            MarkdownNode::new(self.name(), ())
                .text(text.clone())
                .markdown(text),
        )
    }
    fn render(&self, node: &MarkdownNode, _: &mut Window, _: &mut App) -> impl IntoElement {
        div().whitespace_normal().child(node.as_text().to_owned())
    }
}
/// Inline code that names a file (`src/app.rs:42`) opens it in Neovim at that line.
/// Parsed from the Markdown tree, so code blocks and identifiers like `cx.notify` stay
/// plain code; whether the file exists is checked only when it is opened.
struct FileReferences {
    thread: Option<ThreadId>,
}
const FILE_REFERENCE: &str = "drostje-file-reference";
impl MarkdownPlugin for FileReferences {
    fn name(&self) -> &str {
        FILE_REFERENCE
    }
    fn parse(
        &self,
        node: &markdown_ast::Node,
        _: &MarkdownParseContext<'_>,
    ) -> Option<MarkdownNode> {
        let markdown_ast::Node::InlineCode(code) = node else {
            return None;
        };
        let reference = file_refs::parse(&code.value)?;
        Some(
            MarkdownNode::new(FILE_REFERENCE, reference)
                .text(code.value.clone())
                .markdown(format!("`{}`", code.value)),
        )
    }
    fn render_inline(
        &self,
        node: &MarkdownNode,
        context: &InlineRenderContext,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<InlineElement> {
        let reference = node.data::<FileReference>()?.clone();
        let thread = self.thread;
        let offset = node.source_range().map_or(0, |range| range.start);
        let tooltip: SharedString = match reference.line {
            Some(line) => format!("Open in Neovim at line {line}").into(),
            None => "Open in Neovim".into(),
        };
        Some(InlineElement::new(
            Button::new(SharedString::from(format!("file-reference-{offset}")))
                .link()
                .tab_stop(false)
                .label(node.as_text().to_owned())
                .font_family(cx.theme().mono_font_family.clone())
                .text_size(context.font_size())
                .h(context.line_height())
                .debug_selector(|| "conversation-file-reference".into())
                .tooltip(tooltip)
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    window.dispatch_action(
                        Box::new(crate::ui::commands::OpenFileReference {
                            reference: reference.clone(),
                            thread,
                        }),
                        cx,
                    );
                }),
        ))
    }
}

/// Every file a Conversation displayed, in inline code or as a tool's file, most
/// recent first and each once: the rows of "Open file from conversation…".
pub(in crate::ui) fn file_references(session: &ConversationSession) -> Vec<FileReference> {
    let mut seen = std::collections::HashSet::new();
    let mut references = Vec::new();
    let tools = session
        .tools
        .values()
        .filter_map(|tool| file_refs::tool_reference(&tool.name, &tool.input));
    let messages = session.messages.iter().rev().flat_map(|message| {
        let mut found = Vec::new();
        for block in &message.content {
            match block {
                ContentBlock::Text(text) => found.extend(file_refs::inline_code_references(text)),
                ContentBlock::PeerMessage(peer) => {
                    found.extend(file_refs::inline_code_references(&peer.body))
                }
                ContentBlock::ToolCall { name, input, .. } => {
                    found.extend(file_refs::tool_reference(name, input))
                }
                ContentBlock::Thinking(_)
                | ContentBlock::Image(_)
                | ContentBlock::ImageError(_) => {}
            }
        }
        found.into_iter().rev()
    });
    for reference in tools.chain(messages) {
        if seen.insert(reference.label()) {
            references.push(reference);
        }
    }
    references
}

/// Transcript prose stays close to body size: headings step up modestly and
/// paragraphs sit half a line apart, so a reply reads as a message, not a page.
/// `body` is the size the text inherits, so prose type scales its headings too.
fn transcript_text_style(body: Pixels) -> TextViewStyle {
    TextViewStyle::default()
        .paragraph_gap(rems(0.5))
        .heading_font_size(move |level, _| {
            body * match level {
                1 => 1.3,
                2 => 1.2,
                3 => 1.1,
                _ => 1.,
            }
        })
}
/// Where a Markdown block keeps its parsed state.
#[derive(Clone, Copy)]
pub enum Markdown<'a> {
    /// With its element, for as long as the element renders in consecutive frames.
    Element,
    /// With the transcript, for the list row it renders in.
    Row(&'a MarkdownStates, usize),
}

impl Markdown<'_> {
    /// The Conversation whose transcript renders this Markdown, if any.
    pub(super) fn thread(self) -> Option<ThreadId> {
        match self {
            Markdown::Row(states, _) => Some(states.thread),
            Markdown::Element => None,
        }
    }
}

/// `TextView` parses longer documents on a background executor; shorter full replacements
/// parse synchronously and notify their state at once.
pub(super) const SYNC_PARSE_LIMIT: usize = 4 * 1024;

/// Renders after which an entry no row asked for may go. Only entries that reparse in the
/// same frame (short, settled, nothing selected) are dropped; a long document keeps its
/// state so a returning row is measured at its real height.
const EVICT_AFTER_RENDERS: u64 = 300;
/// Renders between two eviction sweeps.
const SWEEP_EVERY_RENDERS: u64 = 64;

/// How a block's Markdown derives from the text it shows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Derivation<'a> {
    /// The text is Markdown.
    Prose,
    /// The text is plain output inside a code fence of this language.
    Code(&'a str),
}

impl Derivation<'_> {
    fn markdown(self, source: &str) -> String {
        match self {
            Derivation::Prose => source.to_owned(),
            Derivation::Code(language) => code_fence(source, language),
        }
    }
}

/// Parsed transcript Markdown, kept for as long as the transcript.
///
/// Element state lasts only while its element renders in consecutive frames, and a long
/// document lays out empty until its background parse lands. A row that left the list's
/// render window and came back would be measured at zero height, and scrolling across it
/// would skip it. Here each text parses once, and a landing parse remeasures its row.
///
/// Entries are keyed by element ID and remember the source text they were derived from, so
/// an unchanged block costs a comparison per frame rather than a copy, a derivation and a
/// parse. Text that extends the previous text is appended, which reparses only the last
/// block.
#[derive(Clone)]
pub struct MarkdownStates {
    /// The Conversation whose working tree resolves file references in this text.
    pub(super) thread: ThreadId,
    list: ListState,
    view: EntityId,
    entries: Rc<RefCell<HashMap<ElementId, MarkdownEntry>>>,
    derived: Rc<RefCell<HashMap<SharedString, DerivedText>>>,
    /// Renders of the transcript so far; entries remember the last one that used them.
    renders: Rc<Cell<u64>>,
}

struct MarkdownEntry {
    state: Entity<TextViewState>,
    /// The text shown, before any code fence was added.
    source: SharedString,
    language: Option<SharedString>,
    /// Bytes of Markdown the state holds: whether it would parse at once again.
    markdown_len: usize,
    row: Rc<Cell<usize>>,
    /// Set until a background parse lands and the row can be measured at its real height.
    parsing: Rc<Cell<bool>>,
    used: u64,
    /// Notifies the view of changes the view did not make itself.
    parsed: Subscription,
}

/// Text derived from a block's source (reasoning as shown), kept while the source holds.
struct DerivedText {
    source: SharedString,
    derived: SharedString,
    used: u64,
}

/// What a `TranscriptMarkdown` element hands its `TextView`.
enum Prepared {
    /// The transcript's state, already showing this text.
    Current(Entity<TextViewState>),
    /// The transcript's state at `id`, to be brought to this text while rendering.
    Update {
        states: MarkdownStates,
        id: ElementId,
        source: SharedString,
        language: Option<SharedString>,
        row: usize,
    },
    /// Element-scoped state for this Markdown.
    Element(ElementId, SharedString),
}

impl MarkdownStates {
    pub(super) fn new(thread: ThreadId, list: ListState, view: EntityId) -> Self {
        Self {
            thread,
            list,
            view,
            entries: Rc::default(),
            derived: Rc::default(),
            renders: Rc::default(),
        }
    }

    /// Starts a render of the transcript and now and then drops entries no row used for
    /// `EVICT_AFTER_RENDERS` renders and that would reparse at once if their row returned.
    pub(super) fn begin_render(&self, cx: &App) {
        let render = self.renders.get() + 1;
        self.renders.set(render);
        if !render.is_multiple_of(SWEEP_EVERY_RENDERS) {
            return;
        }
        let stale = |used: u64| render.saturating_sub(used) > EVICT_AFTER_RENDERS;
        self.entries.borrow_mut().retain(|_, entry| {
            !stale(entry.used)
                || entry.parsing.get()
                || entry.markdown_len > SYNC_PARSE_LIMIT
                || entry.state.read(cx).is_selecting()
                || !entry.state.read(cx).selected_text().is_empty()
        });
        self.derived
            .borrow_mut()
            .retain(|_, derived| !stale(derived.used));
    }

    /// Looks up the state for a block without copying its text when the text is unchanged.
    fn prepare(&self, id: ElementId, source: &str, derivation: Derivation, row: usize) -> Prepared {
        let language = match derivation {
            Derivation::Prose => None,
            Derivation::Code(language) => Some(language),
        };
        if let Some(entry) = self.entries.borrow_mut().get_mut(&id) {
            entry.row.set(row);
            entry.used = self.renders.get();
            if entry.source.as_str() == source && entry.language.as_deref() == language {
                return Prepared::Current(entry.state.clone());
            }
        }
        Prepared::Update {
            states: self.clone(),
            id,
            source: SharedString::from(source.to_owned()),
            language: language.map(|language| SharedString::from(language.to_owned())),
            row,
        }
    }

    fn state(
        &self,
        id: ElementId,
        source: SharedString,
        language: Option<SharedString>,
        row: usize,
        cx: &mut App,
    ) -> Entity<TextViewState> {
        let derivation = match &language {
            Some(language) => Derivation::Code(language),
            None => Derivation::Prose,
        };
        let mut entries = self.entries.borrow_mut();
        let Some(entry) = entries.get_mut(&id) else {
            drop(entries);
            return self.insert(id, source, language, row, cx);
        };
        // Streaming prose only grows: append the new tail instead of reparsing it all.
        let appends = derivation == Derivation::Prose
            && entry.language.is_none()
            && !entry.source.is_empty()
            && source.len() > entry.source.len()
            && source.starts_with(entry.source.as_str());
        let state = entry.state.clone();
        if appends {
            let tail = &source[entry.source.len()..];
            // An append always parses in the background.
            entry.parsing.set(true);
            entry.markdown_len += tail.len();
            state.update(cx, |state, cx| state.push_str(tail, cx));
        } else {
            let text = derivation.markdown(&source);
            entry.parsing.set(text.len() > SYNC_PARSE_LIMIT);
            entry.markdown_len = text.len();
            // A short replacement parses at once and notifies while this view renders it.
            // An observer made after that notification becomes active only once it has been
            // delivered, so the view is not asked to render the change a second time; later
            // changes reach it as before.
            state.update(cx, |state, cx| state.set_text(&text, cx));
            entry.parsed = self.observe(&state, &entry.row, &entry.parsing, cx);
        }
        entry.source = source;
        entry.language = language;
        state
    }

    fn insert(
        &self,
        id: ElementId,
        source: SharedString,
        language: Option<SharedString>,
        row: usize,
        cx: &mut App,
    ) -> Entity<TextViewState> {
        let derivation = match &language {
            Some(language) => Derivation::Code(language),
            None => Derivation::Prose,
        };
        let text = derivation.markdown(&source);
        // The observer becomes active after the state's first notification, as in `state`.
        let state = cx.new(|cx| TextViewState::markdown(&text, cx));
        let row = Rc::new(Cell::new(row));
        let parsing = Rc::new(Cell::new(text.len() > SYNC_PARSE_LIMIT));
        let parsed = self.observe(&state, &row, &parsing, cx);
        self.entries.borrow_mut().insert(
            id,
            MarkdownEntry {
                state: state.clone(),
                source,
                language,
                markdown_len: text.len(),
                row,
                parsing,
                used: self.renders.get(),
                parsed,
            },
        );
        state
    }

    /// Re-renders the transcript when the state changes, and remeasures the row once a
    /// background parse lands.
    fn observe(
        &self,
        state: &Entity<TextViewState>,
        row: &Rc<Cell<usize>>,
        parsing: &Rc<Cell<bool>>,
        cx: &mut App,
    ) -> Subscription {
        let (list, view, row, parsing) =
            (self.list.clone(), self.view, row.clone(), parsing.clone());
        cx.observe(state, move |_, cx| {
            if parsing.replace(false) {
                let row = row.get();
                if row < list.item_count() {
                    list.remeasure_items(row..row + 1);
                }
            }
            cx.notify(view);
        })
    }

    /// `derive(source)`, recomputed only when `source` differs from last time at `key`.
    pub(super) fn derived(
        &self,
        key: &str,
        source: &str,
        derive: impl FnOnce(&str) -> String,
    ) -> SharedString {
        let mut derived = self.derived.borrow_mut();
        let used = self.renders.get();
        if let Some(entry) = derived.get_mut(key)
            && entry.source.as_str() == source
        {
            entry.used = used;
            return entry.derived.clone();
        }
        let text = SharedString::from(derive(source));
        derived.insert(
            SharedString::from(key.to_owned()),
            DerivedText {
                source: SharedString::from(source.to_owned()),
                derived: text.clone(),
                used,
            },
        );
        text
    }

    pub(super) fn clear(&self) {
        self.entries.borrow_mut().clear();
        self.derived.borrow_mut().clear();
    }
}

/// Markdown `text` in the transcript's type; a row's state is reused while `text` holds.
pub fn markdown_text(id: impl Into<ElementId>, text: &str, markdown: Markdown) -> impl IntoElement {
    TranscriptMarkdown::new(id.into(), text, Derivation::Prose, markdown)
}

#[derive(IntoElement)]
struct TranscriptMarkdown {
    prepared: Prepared,
    /// The Conversation whose working tree resolves file references, if the text has one.
    thread: Option<ThreadId>,
}

impl TranscriptMarkdown {
    fn new(id: ElementId, source: &str, derivation: Derivation, markdown: Markdown) -> Self {
        match markdown {
            Markdown::Element => Self {
                prepared: Prepared::Element(id, derivation.markdown(source).into()),
                thread: None,
            },
            Markdown::Row(states, row) => Self {
                prepared: states.prepare(id, source, derivation, row),
                thread: Some(states.thread),
            },
        }
    }
}

impl RenderOnce for TranscriptMarkdown {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let body = window.text_style().font_size.to_pixels(window.rem_size());
        // Element-scoped text has no Conversation of its own: the selected one resolves it.
        let thread = self.thread;
        match self.prepared {
            Prepared::Current(state) => TextView::new(&state),
            Prepared::Update {
                states,
                id,
                source,
                language,
                row,
            } => TextView::new(&states.state(id, source, language, row, cx)),
            Prepared::Element(id, text) => TextView::markdown(id, text),
        }
        .style(transcript_text_style(body))
        .plugin(InertResources)
        .plugin(FileReferences { thread })
        .selectable(true)
        .on_link_click(move |url, event, window, cx| {
            if event.is_right_click() {
                return;
            }
            if url.starts_with("https://") || url.starts_with("http://") {
                window.dispatch_action(
                    Box::new(crate::ui::browser::OpenBrowserUrl {
                        url: url.to_string(),
                    }),
                    cx,
                );
            } else if let Some(reference) = file_refs::parse_link(url) {
                window.dispatch_action(
                    Box::new(crate::ui::commands::OpenFileReference { reference, thread }),
                    cx,
                );
            }
        })
    }
}

/// Plain output (compiler messages, grep hits, file contents) rendered through
/// the kit's Markdown code block, which brings the code font, background,
/// highlighting for a known language and copy support.
pub(super) fn code_block(
    identity: impl Into<ElementId>,
    text: &str,
    language: &str,
    markdown: Markdown,
) -> impl IntoElement {
    TranscriptMarkdown::new(identity.into(), text, Derivation::Code(language), markdown)
}
/// Wrap `text` in a fence longer than any backtick run it contains, so the
/// block always closes where intended.
fn code_fence(text: &str, language: &str) -> String {
    let longest = text.split(|ch| ch != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    let text = text.strip_suffix('\n').unwrap_or(text);
    format!("{fence}{language}\n{text}\n{fence}\n")
}

#[cfg(test)]
mod tests {
    use super::code_fence;

    #[cfg(feature = "runtime-qa")]
    fn states(
        cx: &mut gpui_kit::TestAppContext,
    ) -> (
        super::MarkdownStates,
        std::rc::Rc<std::cell::Cell<usize>>,
        gpui_kit::Entity<()>,
    ) {
        use gpui_kit::{AppContext, ListAlignment, ListState, Pixels};
        cx.update(gpui_kit::init);
        // Stands in for the transcript view: counts the renders the states ask of it.
        let view = cx.new(|_| ());
        let notified = std::rc::Rc::new(std::cell::Cell::new(0));
        cx.update(|cx| {
            let notified = notified.clone();
            cx.observe(&view, move |_, _| notified.set(notified.get() + 1))
                .detach();
        });
        let list = ListState::new(1, ListAlignment::Top, Pixels::ZERO);
        let states =
            super::MarkdownStates::new(crate::model::ThreadId::nil(), list, view.entity_id());
        (states, notified, view)
    }

    #[cfg(feature = "runtime-qa")]
    #[gpui_kit::test]
    fn streamed_text_appends_and_the_view_renders_only_changes_it_did_not_make(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use gpui_kit::ElementId;
        let (states, notified, _view) = states(cx);
        let id = ElementId::from("reply");
        let state = cx.update(|cx| states.state(id.clone(), "Hello".into(), None, 0, cx));
        cx.run_until_parked();
        assert_eq!(
            notified.get(),
            0,
            "the view renders the text it just created"
        );

        // A selection is a change the view did not make.
        cx.update(|cx| state.update(cx, |state, cx| state.select_all(cx)));
        cx.run_until_parked();
        assert_eq!(notified.get(), 1);

        // Streaming appends: the selection survives, which a replacement would clear, and
        // the background parse that lands renders the row again.
        cx.update(|cx| states.state(id.clone(), "Hello world".into(), None, 0, cx));
        cx.run_until_parked();
        assert_eq!(
            cx.read(|cx| state.read(cx).selected_text()).trim(),
            "Hello world"
        );
        assert_eq!(notified.get(), 2, "a landed append re-renders its row");

        // A short replacement parses at once while the view renders it; no second render.
        cx.update(|cx| states.state(id.clone(), "Goodbye".into(), None, 0, cx));
        cx.run_until_parked();
        assert_eq!(cx.read(|cx| state.read(cx).selected_text()), "");
        assert_eq!(notified.get(), 2);
        // Later changes still reach the view.
        cx.update(|cx| state.update(cx, |state, cx| state.select_all(cx)));
        cx.run_until_parked();
        assert_eq!(notified.get(), 3);
        assert_eq!(
            cx.read(|cx| state.read(cx).selected_text()).trim(),
            "Goodbye"
        );
    }

    #[cfg(feature = "runtime-qa")]
    #[gpui_kit::test]
    fn unchanged_text_reuses_its_state_and_derived_text(cx: &mut gpui_kit::TestAppContext) {
        use super::{Derivation, Prepared};
        use gpui_kit::ElementId;
        let (states, _, _view) = states(cx);
        let id = ElementId::from("output");
        let state =
            cx.update(|cx| states.state(id.clone(), "a ``` b".into(), Some("text".into()), 0, cx));
        match states.prepare(id.clone(), "a ``` b", Derivation::Code("text"), 0) {
            Prepared::Current(current) => assert_eq!(current, state),
            _ => panic!("an unchanged source keeps its state without copying the text"),
        }
        assert!(matches!(
            states.prepare(id.clone(), "a ``` c", Derivation::Code("text"), 0),
            Prepared::Update { .. }
        ));
        assert!(matches!(
            states.prepare(id, "a ``` b", Derivation::Code("rust"), 0),
            Prepared::Update { .. }
        ));

        let derived = std::cell::Cell::new(0);
        let derive = |source: &str| {
            derived.set(derived.get() + 1);
            source.to_uppercase()
        };
        let first = states.derived("thinking", "plan", derive);
        let again = states.derived("thinking", "plan", derive);
        assert_eq!((first.as_str(), derived.get()), ("PLAN", 1));
        assert_eq!(again, first);
        assert_eq!(
            states.derived("thinking", "plans", derive).as_str(),
            "PLANS"
        );
        assert_eq!(derived.get(), 2);
    }

    #[cfg(feature = "runtime-qa")]
    #[gpui_kit::test]
    fn entries_no_row_uses_leave_unless_they_would_parse_late(cx: &mut gpui_kit::TestAppContext) {
        use super::{EVICT_AFTER_RENDERS, SWEEP_EVERY_RENDERS, SYNC_PARSE_LIMIT};
        use gpui_kit::ElementId;
        let (states, _, _view) = states(cx);
        let long = "Paragraph of a long reply.\n\n".repeat(SYNC_PARSE_LIMIT / 20);
        let (short, selected, large) = (
            ElementId::from("short"),
            ElementId::from("selected"),
            ElementId::from("large"),
        );
        cx.update(|cx| {
            states.state(short.clone(), "short".into(), None, 0, cx);
            states
                .state(selected.clone(), "selected".into(), None, 0, cx)
                .update(cx, |state, cx| state.select_all(cx));
            states.state(large.clone(), long.into(), None, 0, cx);
        });
        cx.run_until_parked();
        let renders = EVICT_AFTER_RENDERS + SWEEP_EVERY_RENDERS;
        cx.update(|cx| {
            for _ in 0..renders {
                states.begin_render(cx);
            }
        });
        let entries = states.entries.borrow();
        assert!(
            !entries.contains_key(&short),
            "a short unused entry reparses at once"
        );
        assert!(entries.contains_key(&selected), "a selection stays");
        assert!(
            entries.contains_key(&large),
            "a long document keeps its parse"
        );
    }

    #[cfg(feature = "runtime-qa")]
    #[gpui_kit::test]
    fn inline_code_paths_open_in_neovim_and_identifiers_stay_code(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use super::{Markdown, markdown_text};
        use crate::{file_refs::FileReference, ui::commands::OpenFileReference};
        use gpui_kit::{
            AppContext, Context, FocusHandle, InteractiveElement, IntoElement, ParentElement,
            Render, Styled, Window, component::Root, div, test::TestWindowExt,
        };

        const TEXT: &str = "The integration lives in `src/browser_tools.rs:149-210`; \
                            call `cx.notify` after `assets/browser-tools/tools.json` changes.";
        struct Prose {
            focus: FocusHandle,
            opened: Vec<OpenFileReference>,
        }
        impl Render for Prose {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                div()
                    .size_full()
                    .track_focus(&self.focus)
                    .on_action(cx.listener(|this, action: &OpenFileReference, _, _| {
                        this.opened.push(action.clone())
                    }))
                    .child(markdown_text("prose", TEXT, Markdown::Element))
            }
        }

        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::ui::theme::initialize(None, None, cx);
        });
        let mut view = None;
        let window = cx.add_window(|window, cx| {
            let prose = cx.new(|cx| Prose {
                focus: cx.focus_handle(),
                opened: Vec::new(),
            });
            view = Some(prose.clone());
            Root::new(prose, window, cx)
        });
        let view = view.unwrap();
        let id = |code: &str| format!("file-reference-{}", TEXT.find(code).unwrap());
        cx.update_window(window.into(), |_, window, cx| {
            view.read(cx).focus.clone().focus(window, cx);
            window.render_frame(cx);
            assert!(window.try_find(id("`cx.notify`")).is_none());
            window.click(id("`src/browser_tools.rs"), cx);
            window.click(id("`assets/"), cx);
        })
        .unwrap();
        let opened = cx.read(|cx| view.read(cx).opened.clone());
        let reference = |path: &str, line| FileReference {
            path: path.into(),
            line,
            column: None,
        };
        assert!(
            opened
                == [
                    OpenFileReference {
                        reference: reference("src/browser_tools.rs", Some(149)),
                        thread: None,
                    },
                    OpenFileReference {
                        reference: reference("assets/browser-tools/tools.json", None),
                        thread: None,
                    },
                ]
        );
    }

    #[cfg(feature = "runtime-qa")]
    #[gpui_kit::test]
    fn scrolling_across_long_markdown_lands_inside_it(cx: &mut gpui_kit::TestAppContext) {
        use super::{Markdown, MarkdownStates, SYNC_PARSE_LIMIT, markdown_text};
        use gpui_kit::{
            AppContext, Context, FollowMode, IntoElement, ListAlignment, ListOffset, ListState,
            ParentElement, Render, Styled, Window, component::Root, div, list, px,
        };

        const LONG: usize = 10;
        struct Rows {
            list: ListState,
            markdown: MarkdownStates,
            text: String,
        }
        impl Render for Rows {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                let (markdown, text) = (self.markdown.clone(), self.text.clone());
                list(self.list.clone(), move |index, _, _| {
                    if index == LONG {
                        let markdown = Markdown::Row(&markdown, index);
                        div()
                            .w_full()
                            .child(markdown_text("long", &text, markdown))
                            .into_any_element()
                    } else {
                        div().h(px(40.)).w_full().into_any_element()
                    }
                })
                .size_full()
            }
        }

        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::ui::theme::initialize(None, None, cx);
        });
        let text = (0..200)
            .map(|line| format!("Paragraph {line} of a long reply."))
            .collect::<Vec<_>>()
            .join("\n\n");
        assert!(text.len() > SYNC_PARSE_LIMIT, "parses in the background");
        let rows = ListState::new(200, ListAlignment::Bottom, px(500.));
        rows.set_follow_mode(FollowMode::Tail);
        let window = cx.add_window({
            let rows = rows.clone();
            |window, cx| {
                let view = cx.new(|cx| Rows {
                    markdown: MarkdownStates::new(
                        crate::model::ThreadId::nil(),
                        rows.clone(),
                        cx.entity_id(),
                    ),
                    list: rows,
                    text,
                });
                Root::new(view, window, cx)
            }
        });
        let draw = |cx: &mut gpui_kit::TestAppContext| {
            cx.update_window(window.into(), |_, window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            })
            .unwrap();
        };
        // The reader sits 10px into the row below, so 30px up is 20px into the long row.
        let scroll_up_into_long_row = |cx: &mut gpui_kit::TestAppContext| {
            rows.scroll_to(ListOffset {
                item_ix: LONG + 1,
                offset_in_item: px(10.),
            });
            draw(cx);
            rows.scroll_by(px(-30.));
            draw(cx);
            rows.logical_scroll_top().item_ix
        };
        draw(cx);

        // First seen above the viewport, so it lays out empty until its parse lands.
        rows.scroll_to(ListOffset {
            item_ix: LONG + 1,
            offset_in_item: px(10.),
        });
        draw(cx);
        cx.run_until_parked();
        assert_eq!(
            scroll_up_into_long_row(cx),
            LONG,
            "a landed parse must remeasure its row"
        );

        // Away long enough for element state to lapse, and every row invalidated.
        rows.scroll_to(ListOffset {
            item_ix: 150,
            offset_in_item: px(0.),
        });
        draw(cx);
        draw(cx);
        rows.remeasure_items(0..200);
        assert_eq!(
            scroll_up_into_long_row(cx),
            LONG,
            "a row coming back must not wait for a second parse"
        );
    }

    #[test]
    fn code_fences_outrun_backticks_inside_the_text() {
        assert_eq!(code_fence("a\nb\n", "text"), "```text\na\nb\n```\n");
        assert_eq!(
            code_fence("see ```rust\nfn x() {}\n```", "text"),
            "````text\nsee ```rust\nfn x() {}\n```\n````\n"
        );
        assert_eq!(code_fence("", "json"), "```json\n\n```\n");
    }
}
