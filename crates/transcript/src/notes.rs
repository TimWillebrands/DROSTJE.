const HIDDEN: [(&str, &str); 3] = [
    ("<system-reminder>", "</system-reminder>"),
    ("<irc>", "</irc>"),
    ("<critical>", "</critical>"),
];

pub fn is_harness_note(text: &str) -> bool {
    let text = text.trim();
    HIDDEN
        .iter()
        .any(|(open, close)| text.starts_with(open) && text.ends_with(close))
}

#[cfg(test)]
mod tests {
    use super::is_harness_note;

    #[test]
    fn only_whole_injected_notes_are_harness_notes() {
        assert!(is_harness_note(
            "<system-reminder>\n2 todo items still open.\n</system-reminder>"
        ));
        assert!(is_harness_note(
            "<irc>\nIncoming IRC message from agent `Review`:\n</irc>\n"
        ));
        assert!(is_harness_note(
            "<critical>\nPlan mode active.\n</critical>"
        ));
        assert!(!is_harness_note(
            "<system-notice>\nBackground job bg_1 has completed.\n</system-notice>"
        ));
        assert!(!is_harness_note("<irc>partial</irc> and a trailing remark"));
        assert!(!is_harness_note(""));
    }
}
