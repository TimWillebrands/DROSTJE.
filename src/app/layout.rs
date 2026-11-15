use super::*;

/// A tab of a thread's sidebar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarTab {
    Changes,
    Output,
    Terminal,
    /// Space network membership and the Conversation's local agent activity.
    Agents,
    Browser,
    /// Review threads of the pull request pinned to a worktree Conversation.
    Review,
    /// Inspector for the selected thread or tool call.
    Details,
}
impl SidebarTab {
    pub(super) fn from_saved(conversation: bool, saved: Option<&str>) -> Self {
        match saved {
            Some("terminal") => Self::Terminal,
            Some("output") => Self::Output,
            _ if !conversation => Self::Output,
            Some("agents") => Self::Agents,
            Some("browser") => Self::Browser,
            Some("review") => Self::Review,
            Some("details") => Self::Details,
            _ => Self::Changes,
        }
    }

    /// The sidebar `(open, tab)` after asking for `requested`: a hidden sidebar or
    /// another tab switches to it; the tab already on screen hides the sidebar.
    pub fn toggle(open: bool, current: Self, requested: Self) -> (bool, Self) {
        if open && current == requested {
            (false, current)
        } else {
            (true, requested)
        }
    }
}

/// How the Changes tab lays out a diff; one preference for every thread.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChangesLayout {
    /// One column: removed lines above the added lines that replace them.
    #[default]
    Inline,
    /// Old file left, new file right, a removal beside its replacement.
    SideBySide,
}
impl ChangesLayout {
    pub(super) const KEY: &str = "changes_layout";

    pub(super) fn from_saved(saved: Option<&str>) -> Self {
        match saved {
            Some("side_by_side") => Self::SideBySide,
            _ => Self::Inline,
        }
    }

    pub(super) fn saved(self) -> &'static str {
        match self {
            Self::Inline => "inline",
            Self::SideBySide => "side_by_side",
        }
    }

    pub fn toggled(self) -> Self {
        match self {
            Self::Inline => Self::SideBySide,
            Self::SideBySide => Self::Inline,
        }
    }
}

impl AppState {
    /// Whether a thread's sidebar (Changes, Output) is shown. The settings key predates
    /// the Output tab and is kept so existing choices survive.
    pub fn thread_sidebar_open(&self, id: ThreadId) -> bool {
        let id = id.to_string();
        self.snapshot
            .settings
            .get("conversation_diff_open")
            .and_then(Value::as_object)
            .and_then(|threads| threads.get(&id))
            .and_then(Value::as_bool)
            .unwrap_or(true)
    }

    /// The sidebar tab a thread shows. Factory threads have no Changes tab.
    pub fn thread_sidebar_tab(&self, id: ThreadId) -> SidebarTab {
        SidebarTab::from_saved(
            self.is_conversation(id),
            self.snapshot
                .settings
                .get("thread_sidebar_tab")
                .and_then(Value::as_object)
                .and_then(|threads| threads.get(&id.to_string()))
                .and_then(Value::as_str),
        )
    }

    pub fn changes_layout(&self) -> ChangesLayout {
        ChangesLayout::from_saved(
            self.snapshot
                .settings
                .get(ChangesLayout::KEY)
                .and_then(Value::as_str),
        )
    }

    pub fn toggle_changes_layout(&mut self) {
        let layout = self.changes_layout().toggled();
        self.set_setting(ChangesLayout::KEY, Value::String(layout.saved().into()));
    }

    /// Show `tab` in the thread's sidebar, or hide the sidebar when `tab` is already shown.
    pub fn toggle_thread_sidebar(&mut self, id: ThreadId, tab: SidebarTab) {
        let (open, tab) = SidebarTab::toggle(
            self.thread_sidebar_open(id),
            self.thread_sidebar_tab(id),
            tab,
        );
        self.set_thread_sidebar(id, open, tab);
    }

    /// Open the thread's sidebar on `tab`.
    pub fn show_thread_sidebar(&mut self, id: ThreadId, tab: SidebarTab) {
        self.set_thread_sidebar(id, true, tab);
    }

    pub(super) fn set_thread_sidebar(&mut self, id: ThreadId, open: bool, tab: SidebarTab) {
        let key = id.to_string();
        let mut threads = self
            .snapshot
            .settings
            .get("conversation_diff_open")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        if open {
            threads.remove(&key);
        } else {
            threads.insert(key.clone(), Value::Bool(false));
        }
        if self.snapshot.settings.get("conversation_diff_open")
            != Some(&Value::Object(threads.clone()))
        {
            self.set_setting("conversation_diff_open", Value::Object(threads));
        }
        if self.thread_sidebar_tab(id) != tab {
            let mut tabs = self
                .snapshot
                .settings
                .get("thread_sidebar_tab")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            match tab {
                SidebarTab::Changes => tabs.remove(&key),
                SidebarTab::Output => tabs.insert(key, Value::String("output".into())),
                SidebarTab::Terminal => tabs.insert(key, Value::String("terminal".into())),
                SidebarTab::Agents => tabs.insert(key, Value::String("agents".into())),
                SidebarTab::Browser => tabs.insert(key, Value::String("browser".into())),
                SidebarTab::Review => tabs.insert(key, Value::String("review".into())),
                SidebarTab::Details => tabs.insert(key, Value::String("details".into())),
            };
            self.set_setting("thread_sidebar_tab", Value::Object(tabs));
        }
    }
    pub fn thread_sidebar_width(&self, id: ThreadId) -> f32 {
        self.snapshot
            .settings
            .get("thread_sidebar_width")
            .and_then(|widths| widths.get(id.to_string()))
            .and_then(Value::as_f64)
            .filter(|width| width.is_finite())
            .unwrap_or(352.) as f32
    }

    pub fn set_thread_sidebar_width(&mut self, id: ThreadId, width: f32) {
        if !width.is_finite() {
            return;
        }
        let mut widths = self
            .snapshot
            .settings
            .get("thread_sidebar_width")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        widths.insert(id.to_string(), json!(width.clamp(180., 1600.)));
        self.set_setting("thread_sidebar_width", Value::Object(widths));
    }
    /// User-resized composer height in logical pixels; absent keeps content-aware sizing.
    pub fn thread_composer_height(&self, id: ThreadId) -> Option<f32> {
        self.snapshot
            .settings
            .get("thread_composer_height")
            .and_then(|heights| heights.get(id.to_string()))
            .and_then(Value::as_f64)
            .filter(|height| height.is_finite())
            .map(|height| height.clamp(128., 1600.) as f32)
    }

    pub fn set_thread_composer_height(&mut self, id: ThreadId, height: f32) {
        if !height.is_finite() {
            return;
        }
        let height = height.clamp(128., 1600.);
        if self.thread_composer_height(id) == Some(height) {
            return;
        }
        let mut heights = self
            .snapshot
            .settings
            .get("thread_composer_height")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        heights.insert(id.to_string(), json!(height));
        self.set_setting("thread_composer_height", Value::Object(heights));
    }
}

#[cfg(test)]
mod tests {
    use super::{ChangesLayout, SidebarTab};

    #[test]
    fn sidebar_toggle_switches_tabs_before_hiding() {
        use SidebarTab::{Changes, Output};
        assert_eq!(SidebarTab::toggle(true, Changes, Output), (true, Output));
        assert_eq!(SidebarTab::toggle(true, Output, Output), (false, Output));
        assert_eq!(SidebarTab::toggle(false, Output, Output), (true, Output));
        assert_eq!(SidebarTab::toggle(false, Output, Changes), (true, Changes));
    }

    #[test]
    fn changes_layout_round_trips_and_defaults_to_inline() {
        for layout in [ChangesLayout::Inline, ChangesLayout::SideBySide] {
            assert_eq!(ChangesLayout::from_saved(Some(layout.saved())), layout);
            assert_eq!(layout.toggled().toggled(), layout);
        }
        assert_eq!(ChangesLayout::from_saved(None), ChangesLayout::Inline);
        assert_eq!(
            ChangesLayout::from_saved(Some("bogus")),
            ChangesLayout::Inline
        );
    }
}
