use serde_json::{Value, json};
use std::collections::VecDeque;

const MAX_EVENTS: usize = 256;
const MAX_STRING_BYTES: usize = 4096;

#[derive(Default)]
pub(super) struct Diagnostics {
    cursor: u64,
    events: VecDeque<(u64, Value)>,
}

fn bounded(mut value: String) -> (String, bool) {
    if value.len() <= MAX_STRING_BYTES {
        return (value, false);
    }
    let mut end = MAX_STRING_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    (value, true)
}

impl Diagnostics {
    pub fn push(&mut self, kind: &str, level: String, message: String) {
        self.cursor += 1;
        let (level, level_truncated) = bounded(level);
        let (message, message_truncated) = bounded(message);
        if self.events.len() == MAX_EVENTS {
            self.events.pop_front();
        }
        self.events.push_back((self.cursor, json!({
            "sequence": self.cursor, "kind": kind,
            "source": if kind == "console" { "native_console" } else if kind == "request" { "native_request" } else if kind == "evaluation" { "runtime" } else { "document_start_observer" },
            "level": level, "message": message,
            "truncated": level_truncated || message_truncated,
        })));
    }

    pub fn read(&self, after: u64) -> Value {
        let first = self
            .events
            .front()
            .map_or(self.cursor + 1, |(sequence, _)| *sequence);
        json!({
            "cursor": self.cursor,
            "truncated": after.saturating_add(1) < first,
            "dropped": first.saturating_sub(after.saturating_add(1)),
            "events": self.events.iter().filter(|(sequence, _)| *sequence > after).map(|(_, event)| event).collect::<Vec<_>>(),
            "coverage": {
                "console": "native Servo console callback",
                "network": "native HTTP request starts; document-start observer captures fetch/XHR HTTP errors and failures plus resource error events",
                "limitations": "No top-level HTTP response status, inaccessible transport details, worker failures, or full network tracing; page-observed events are not authenticated",
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retention_bounds_count_and_utf8_strings_and_reports_cursor_gaps() {
        let mut diagnostics = Diagnostics::default();
        for _ in 0..MAX_EVENTS + 3 {
            diagnostics.push("console", "warning".into(), "é".repeat(MAX_STRING_BYTES));
        }
        let result = diagnostics.read(0);
        assert_eq!(result["cursor"], MAX_EVENTS + 3);
        assert_eq!(result["dropped"], 3);
        assert_eq!(result["truncated"], true);
        let events = result["events"].as_array().unwrap();
        assert_eq!(events.len(), MAX_EVENTS);
        assert_eq!(
            events[0]["message"].as_str().unwrap().len(),
            MAX_STRING_BYTES
        );
        assert_eq!(events[0]["truncated"], true);
        assert!(
            diagnostics.read((MAX_EVENTS + 3) as u64)["events"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(diagnostics.read(3)["truncated"], false);
    }
}
