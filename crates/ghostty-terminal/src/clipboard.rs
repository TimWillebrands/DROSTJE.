use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClipboardOperation {
    Paste,
    Read,
    Write,
}

pub struct ClipboardApproval<'a> {
    pub operation: ClipboardOperation,
    pub text: &'a str,
}

pub type ClipboardApprovalCallback = Arc<dyn Fn(ClipboardApproval<'_>) -> bool + Send + Sync>;
