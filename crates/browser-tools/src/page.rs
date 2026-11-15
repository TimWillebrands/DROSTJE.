use std::fmt::Write as _;

pub(super) fn observe_script(limit: usize) -> String {
    let mut script =
        String::with_capacity(COMMON.len() + OBSERVE_HELPERS.len() + OBSERVE.len() + 160);
    let _ = write!(
        script,
        "(() => {{\nconst seed = \"{}\";\nconst limit = {};\n",
        uuid::Uuid::new_v4(),
        limit.clamp(1, 200),
    );
    script.push_str(COMMON);
    script.push_str(OBSERVE_HELPERS);
    script.push_str(OBSERVE);
    script.push_str("\n})()");
    script
}

pub(super) fn resolve_script(target: &str) -> String {
    let target = serde_json::Value::String(target.to_owned()).to_string();
    let mut script = String::with_capacity(COMMON.len() + RESOLVE.len() + target.len() + 48);
    script.push_str("(() => {\nconst target = ");
    script.push_str(&target);
    script.push_str(";\n");
    script.push_str(COMMON);
    script.push_str(RESOLVE);
    script.push_str("\n})()");
    script
}

const COMMON: &str = r#"
const doc = document;
const registryKey = Symbol.for("drostje.browser.observation");
const width = window.innerWidth;
const height = window.innerHeight;
const clips = value => /^(hidden|clip|scroll|auto|overlay)$/.test(value);
const semanticElement = element => element.localName === "label" && element.control ? element.control : element;
const styles = new WeakMap();
function styleOf(element) {
    let style = styles.get(element);
    if (!style) {
        style = getComputedStyle(element);
        styles.set(element, style);
    }
    return style;
}
function clipBox(box, element) {
    if (![box.left, box.top, box.right, box.bottom].every(Number.isFinite)) return null;
    let left = Math.max(0, box.left), top = Math.max(0, box.top);
    let right = Math.min(width, box.right), bottom = Math.min(height, box.bottom);
    if (right <= left || bottom <= top) return null;
    const ownStyle = styleOf(element);
    if (ownStyle.visibility === "hidden" || ownStyle.visibility === "collapse") return null;
    let ancestor = element;
    for (let depth = 0; ancestor && depth < 128; depth++, ancestor = ancestor.parentElement) {
        const style = styleOf(ancestor);
        if (style.display === "none" || Number(style.opacity) === 0 || style.contentVisibility === "hidden") return null;
        if (ancestor !== element && (clips(style.overflowX) || clips(style.overflowY))) {
            const outer = ancestor.getBoundingClientRect();
            // Scale client metrics with transformed ancestors; rotated clipping is
            // conservatively checked again by elementFromPoint at resolution time.
            const sx = ancestor.offsetWidth ? outer.width / ancestor.offsetWidth : 1;
            const sy = ancestor.offsetHeight ? outer.height / ancestor.offsetHeight : 1;
            const innerLeft = outer.left + ancestor.clientLeft * sx;
            const innerTop = outer.top + ancestor.clientTop * sy;
            if (clips(style.overflowX)) {
                left = Math.max(left, innerLeft);
                right = Math.min(right, innerLeft + ancestor.clientWidth * sx);
            }
            if (clips(style.overflowY)) {
                top = Math.max(top, innerTop);
                bottom = Math.min(bottom, innerTop + ancestor.clientHeight * sy);
            }
            if (right <= left || bottom <= top) return null;
        }
    }
    if (ancestor) return null;
    return {left, top, right, bottom};
}
function visibleBox(element) {
    let best = null, area = 0;
    const boxes = element.getClientRects();
    for (let i = 0; i < Math.min(boxes.length, 64); i++) {
        const clipped = clipBox(boxes[i], element);
        if (!clipped) continue;
        const size = (clipped.right - clipped.left) * (clipped.bottom - clipped.top);
        if (size > area) { best = clipped; area = size; }
    }
    return best;
}
function disabled(element) {
    const control = semanticElement(element);
    if (control.matches(":disabled")) return true;
    for (const start of control === element ? [element] : [element, control]) {
        let ancestor = start;
        for (let depth = 0; ancestor && depth < 128; depth++, ancestor = ancestor.parentElement) {
            if (ancestor.hasAttribute("inert") || ancestor.getAttribute("aria-disabled") === "true") return true;
            if (ancestor.localName === "select" && ancestor.disabled) return true;
        }
        if (ancestor) return true;
    }
    return false;
}
function hitAt(element, box) {
    const x = (box.left + box.right) / 2, y = (box.top + box.bottom) / 2;
    const hit = doc.elementFromPoint(x, y);
    if (hit && (hit === element || element.contains(hit))) return {x, y};
    // A label covering its associated input still delivers native activation to
    // that input. Do not accept a different control merely sharing an ancestor.
    const label = hit && hit.closest("label");
    if (label && label.control === element) return {x, y};
    return null;
}
"#;

const OBSERVE_HELPERS: &str = r#"
const interactiveRoles = new Set([
    "button", "checkbox", "combobox", "gridcell", "link", "listbox", "menuitem",
    "menuitemcheckbox", "menuitemradio", "option", "radio", "scrollbar", "searchbox",
    "slider", "spinbutton", "switch", "tab", "textbox", "treeitem"
]);
const compact = (value, max) => String(value || "").slice(0, max * 4).replace(/\s+/g, " ").trim().slice(0, max);
const rect = box => ({x: box.left, y: box.top, width: box.right - box.left, height: box.bottom - box.top});
const skipText = element => /^(SCRIPT|STYLE|TEMPLATE|NOSCRIPT)$/.test(element.tagName);
function explicitRole(element) {
    const roles = compact(element.getAttribute("role"), 128).toLowerCase().split(" ");
    return roles.find(role => interactiveRoles.has(role)) || "";
}
function roleOf(element) {
    const control = semanticElement(element);
    const explicit = explicitRole(control);
    if (explicit) return explicit;
    const tag = control.localName;
    if (tag === "a" || tag === "area") return "link";
    if (tag === "button" || tag === "summary") return "button";
    if (tag === "select") return control.multiple || control.size > 1 ? "listbox" : "combobox";
    if (tag === "option") return "option";
    if (tag === "textarea") return "textbox";
    if (tag === "input") {
        switch (control.type) {
            case "button": case "submit": case "reset": case "image": case "file": case "color": return "button";
            case "checkbox": return "checkbox";
            case "radio": return "radio";
            case "range": return "slider";
            case "number": return "spinbutton";
            case "search": return "searchbox";
            default: return "textbox";
        }
    }
    return control.isContentEditable ? "textbox" : "generic";
}
function interactive(element) {
    const tag = element.localName;
    if (tag === "label") return !!element.control && !visibleBox(element.control);
    if (tag === "input") return element.type !== "hidden";
    if (tag === "button" || tag === "select" || tag === "option" || tag === "textarea" || tag === "summary") return true;
    if ((tag === "a" || tag === "area") && element.hasAttribute("href")) return true;
    return !!explicitRole(element) || element.tabIndex >= 0 ||
        (element.isContentEditable && !(element.parentElement && element.parentElement.isContentEditable)) ||
        typeof element.onclick === "function" || element.getAttribute("draggable") === "true";
}
// Accessible-name text can include explicitly referenced hidden labels. Limit
// both work and output rather than constructing an unbounded textContent string.
function nameText(root, max) {
    const walker = doc.createTreeWalker(root, NodeFilter.SHOW_ELEMENT | NodeFilter.SHOW_TEXT);
    let node = root, text = "", visited = 0;
    while (node && visited++ < 512 && text.length < max * 2) {
        if (node.nodeType === Node.ELEMENT_NODE && skipText(node)) {
            node = nextAfterSubtree(walker, root);
            continue;
        }
        if (node.nodeType === Node.TEXT_NODE) text += " " + node.substringData(0, max * 2 - text.length);
        else if (node.localName === "img") text += " " + compact(node.getAttribute("alt"), max);
        node = walker.nextNode();
    }
    return compact(text, max);
}
function nextAfterSubtree(walker, root) {
    let next = walker.nextSibling();
    while (!next && walker.currentNode !== root) {
        if (!walker.parentNode() || walker.currentNode === root) return null;
        next = walker.nextSibling();
    }
    return next;
}
function nameOf(element) {
    const control = semanticElement(element);
    for (const source of control === element ? [element] : [element, control]) {
        const ids = compact(source.getAttribute("aria-labelledby"), 1024).split(" ").slice(0, 16);
        let label = "";
        for (const id of ids) {
            const reference = id && doc.getElementById(id);
            if (reference) label += " " + nameText(reference, 256);
            if (label.length >= 256) break;
        }
        if (label.trim()) return compact(label, 256);
        const aria = compact(source.getAttribute("aria-label"), 256);
        if (aria) return aria;
    }
    if (control.labels) {
        let label = "";
        for (let i = 0; i < Math.min(control.labels.length, 16) && label.length < 256; i++) {
            label += " " + nameText(control.labels[i], 256);
        }
        if (label.trim()) return compact(label, 256);
    }
    if (control.localName === "input") {
        if (control.type === "image") return compact(control.getAttribute("alt") || control.title || "Submit", 256);
        if (/^(button|submit|reset)$/.test(control.type)) {
            return compact(control.value || (control.type === "reset" ? "Reset" : control.type === "submit" ? "Submit" : ""), 256);
        }
    }
    const text = control.localName === "select" || control.localName === "textarea" ? "" : nameText(element, 256);
    return text || compact(control.getAttribute("title") || control.getAttribute("placeholder"), 256);
}
function stateOf(element, box) {
    const control = semanticElement(element);
    const state = {
        disabled: disabled(element),
        focused: doc.activeElement === control || doc.activeElement === element,
        occluded: !hitAt(element, box)
    };
    if ("readOnly" in control || control.hasAttribute("aria-readonly")) {
        state.readonly = !!control.readOnly || control.getAttribute("aria-readonly") === "true";
    }
    if (control.localName === "input" && /^(checkbox|radio)$/.test(control.type)) {
        state.checked = control.indeterminate ? "mixed" : control.checked;
    } else if (control.hasAttribute("aria-checked")) {
        const checked = control.getAttribute("aria-checked");
        state.checked = checked === "mixed" ? "mixed" : checked === "true";
    }
    if (control.localName === "option") state.selected = control.selected;
    else if (control.hasAttribute("aria-selected")) state.selected = control.getAttribute("aria-selected") === "true";
    if (control.localName === "summary" && control.parentElement && control.parentElement.localName === "details") {
        state.expanded = control.parentElement.open;
    } else if (control.hasAttribute("aria-expanded")) state.expanded = control.getAttribute("aria-expanded") === "true";
    if (control.localName === "input" && control.type === "password") {
        state.value_redacted = true;
    } else if (/^(input|select|textarea)$/.test(control.localName)) {
        const value = control.value;
        state.value = value.slice(0, 512);
        state.value_truncated = value.length > 512;
    } else if (control.isContentEditable) {
        state.value = nameText(control, 512);
    } else if (control.hasAttribute("aria-valuetext") || control.hasAttribute("aria-valuenow")) {
        state.value = compact(control.getAttribute("aria-valuetext") || control.getAttribute("aria-valuenow"), 512);
    }
    return state;
}
"#;

const OBSERVE: &str = r#"
let registry = doc[registryKey];
if (!registry || registry.document !== doc || registry.root !== doc.documentElement || !registry.active) {
    if (registry) {
        registry.targets.clear();
        window.removeEventListener("pagehide", registry.onPageHide);
    }
    registry = {document: doc, root: doc.documentElement, token: seed, active: true, targets: new Map()};
    registry.onPageHide = () => { registry.active = false; registry.targets.clear(); };
    Object.defineProperty(doc, registryKey, {value: registry, configurable: true});
    window.addEventListener("pagehide", registry.onPageHide, {once: true});
}
registry.targets.clear();
registry.observation = seed;
const controls = [];
let truncated = false, text = "", textTruncated = false, scanned = 0;
const walker = doc.createTreeWalker(doc.documentElement || doc, NodeFilter.SHOW_ELEMENT | NodeFilter.SHOW_TEXT);
const root = walker.root;
const range = doc.createRange();
let node = root;
while (node && scanned++ < 12000) {
    if (node.nodeType === Node.ELEMENT_NODE) {
        if (skipText(node) || styleOf(node).display === "none" || styleOf(node).contentVisibility === "hidden") {
            node = nextAfterSubtree(walker, root);
            continue;
        }
        if (interactive(node)) {
            const box = visibleBox(node);
            if (box) {
                if (controls.length >= limit) truncated = true;
                else {
                    const target = registry.token + "/" + seed + "/" + controls.length;
                    registry.targets.set(target, node);
                    controls.push({target, role: roleOf(node), label: nameOf(node),
                        bounds: rect(node.getBoundingClientRect()), visible_bounds: rect(box), state: stateOf(node, box)});
                }
            }
        }
    } else if (node.nodeType === Node.TEXT_NODE && node.parentElement &&
        (text.length < 8000 || !textTruncated) && /\S/.test(node.substringData(0, 8192))) {
        range.selectNodeContents(node);
        const boxes = range.getClientRects();
        for (let i = 0; i < Math.min(boxes.length, 64); i++) {
            if (!clipBox(boxes[i], node.parentElement)) continue;
            const remaining = Math.max(0, 8000 - text.length - (text ? 1 : 0));
            const rawLimit = (remaining + 1) * 4;
            const snippet = compact(node.substringData(0, rawLimit), remaining + 1);
            if (snippet.length > remaining || node.length > rawLimit) textTruncated = true;
            if (snippet && remaining) text += (text ? " " : "") + snippet.slice(0, remaining);
            break;
        }
    }
    node = walker.nextNode();
}
if (node) { truncated = true; textTruncated = true; }
return {
    document: registry.token,
    observation: seed,
    url: String(doc.URL).slice(0, 4096),
    title: compact(doc.title, 256),
    ready_state: doc.readyState,
    viewport: {width, height, scale: window.devicePixelRatio},
    text: text.slice(0, 8000),
    text_truncated: textTruncated,
    controls,
    truncated,
    limits: {controls: limit, scanned_nodes: 12000, label_characters: 256, value_characters: 512, text_characters: 8000},
    limitations: [
        "DOM-derived controls, not a full accessibility tree; event-listener-only controls may be absent.",
        "Top document only; iframe documents and shadow-root contents are not traversed.",
        "Viewport-visible bounds are CSS pixels; clipping and center-point occlusion checks are conservative.",
        "Targets expire on the next observation, pagehide, document replacement, or node detachment/replacement."
    ]
};
"#;

const RESOLVE: &str = r#"
const registry = doc[registryKey];
if (!registry || !registry.active || registry.document !== doc || registry.root !== doc.documentElement) {
    throw new Error("Stale target: observe the current document again");
}
const element = registry.targets.get(target);
if (!element || !element.isConnected || element.ownerDocument !== doc || element.getRootNode() !== doc) {
    throw new Error("Stale target: the observation expired or the node was detached/replaced");
}
if (disabled(element)) throw new Error("Disabled target: native, aria-disabled, or inert control");
const box = visibleBox(element);
if (!box) throw new Error("Hidden or offscreen target: no visible viewport bounds; scroll explicitly and observe again");
const point = hitAt(element, box);
if (!point) throw new Error("Occluded target: another element covers its visible center");
return point;
"#;
