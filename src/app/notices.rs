use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeLevel {
    Info,
    Diagnostic,
    Warning,
    Error,
}

#[derive(Clone, Debug)]
pub struct WorkspaceNotice {
    pub level: NoticeLevel,
    pub text: String,
}

impl AppState {
    pub fn notice(&mut self, level: NoticeLevel, text: impl Into<String>) {
        self.notice_sequence = self.notice_sequence.wrapping_add(1);
        if self.notices.len() == 200 {
            self.notices.pop_front();
        }
        self.notices.push_back(WorkspaceNotice {
            level,
            text: text.into(),
        });
    }
}
