//! The artifact kit: default styles and the `window.drostje` channel bridge, injected
//! into artifact surfaces only and only into their `file:` documents. Page styles always
//! win, because the kit's rules sit in a cascade layer below every unlayered page rule.
use crate::browser::ReceiptStatus;
use servo::UserScript;

const KIT_CSS: &str = include_str!("../../../assets/artifact-kit/kit.css");
const KIT_JS: &str = include_str!("../../../assets/artifact-kit/kit.js");

/// Install the kit's styles ahead of the page's own, then define `window.drostje`.
/// The theme rule is unlayered so it overrides the kit's fallback tokens, and it comes
/// before page styles, so a page can still restyle the tokens themselves.
const PRELUDE: &str = r#"(() => {
    try {
        if (location.protocol !== 'file:') return;
        const root = document.head || document.documentElement;
        if (!root || document.getElementById('drostje-kit')) return;
        const style = (id, text) => {
            const element = document.createElement('style');
            element.id = id;
            element.textContent = text;
            return element;
        };
        root.prepend(style('drostje-theme', __DROSTJE_THEME__));
        root.prepend(style('drostje-kit', __DROSTJE_KIT__));
    } catch (_) {}
})();
"#;

/// The kit script for one artifact surface with the given theme tokens.
pub(super) fn script(theme: &str) -> UserScript {
    let kit = js_string(&format!("@layer drostje-kit {{\n{KIT_CSS}\n}}"));
    let prelude = PRELUDE
        .replace("__DROSTJE_THEME__", &js_string(theme))
        .replace("__DROSTJE_KIT__", &kit);
    // kit.js defines the bridge only for file documents, like the prelude's styles.
    UserScript::new(
        format!("{prelude}if (location.protocol === 'file:') {{\n{KIT_JS}\n}}\n"),
        None,
    )
}

/// Replace the current document's theme tokens; reloads use the replaced user script.
pub(super) fn theme_script(theme: &str) -> String {
    format!(
        "(() => {{ try {{ window.drostje && window.drostje.__theme({}); }} catch (_) {{}} }})()",
        js_string(theme)
    )
}

/// Settle one pending `drostje.send` promise in the page.
pub(super) fn receipt_script(event: &str, status: ReceiptStatus, reason: Option<&str>) -> String {
    format!(
        "(() => {{ try {{ window.drostje && window.drostje.__receipt({}, {}, {}); }} catch (_) {{}} }})()",
        js_string(event),
        js_string(status.as_str()),
        reason.map_or_else(|| "null".to_owned(), js_string)
    )
}

/// A JavaScript string literal that cannot close a surrounding script element.
fn js_string(text: &str) -> String {
    serde_json::to_string(text)
        .unwrap_or_else(|_| "\"\"".to_owned())
        .replace('<', "\\u003c")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_quote_page_supplied_text_as_inert_string_literals() {
        let hostile = "x'); alert(1); ('</script>\u{2028}";
        let script = receipt_script(hostile, ReceiptStatus::Failed, Some(hostile));
        assert!(!script.contains("</script>"));
        assert!(!script.contains('\u{2028}'));
        assert!(script.contains(r#""failed""#));
        let literal = js_string(hostile);
        let decoded: String = serde_json::from_str(&literal.replace("\\u003c", "<")).unwrap();
        assert_eq!(decoded, hostile);
        assert!(
            receipt_script("a", ReceiptStatus::Delivered, None)
                .ends_with("null); } catch (_) {} })()")
        );
    }

    #[test]
    fn the_kit_installs_only_into_file_documents_with_layered_defaults() {
        let source = script(":root{--dk-bg:#000}");
        let text = source.script();
        assert!(text.contains("location.protocol !== 'file:'"));
        assert!(text.contains("@layer drostje-kit"));
        assert!(text.contains("--dk-bg:#000"));
        assert!(text.contains("if (location.protocol === 'file:')"));
    }
}
