// DROSTJE. artifact kit v1: the page side of the artifact channel.
//
// DROSTJE runs this at document start in every frame of an artifact page,
// before page scripts and right after inserting the kit styles (a low-priority
// cascade layer, #drostje-kit) and the theme tokens (#drostje-theme). No
// dependencies, no network, no storage. Everything is guarded so a failure here
// never breaks the page.
//
// Wire contract (the host parses exactly this):
//   page -> host  console.warn("__DROSTJE_CHANNEL__" + JSON {v:1, id, type, data, final?}), at most 32 KiB
//   host -> page  drostje.__receipt(id, "delivered" | "queued" | "failed" | "unknown", reason?)
//   host -> page  drostje.__theme(cssText) replaces the unlayered #drostje-theme tokens
// "queued" is not final: the event waits (for example behind a permission
// prompt) and the host later sends delivered, failed or unknown for the same id.
// send() resolves with that final status. No receipt at all within 15 s
// resolves as "unknown": the kit never claims delivery.
(function () {
  "use strict";
  try {
    if (window.drostje) return;
    // Captured first: page scripts may replace console later.
    const emit = console.warn.bind(console);

    const PREFIX = "__DROSTJE_CHANNEL__";
    const TYPE = /^[a-z][a-z0-9_.-]{0,63}$/;
    const MAX_BYTES = 32768;
    const RECEIPT_TIMEOUT_MS = 15000;
    const DOUBLE_G_MS = 600;
    const STATUSES = ["delivered", "queued", "failed", "unknown"];
    const LISTS = ".choices, [role=listbox], [data-drostje-list]";
    const FOCUSABLE = "input:not([type=hidden]), button, select, textarea, a[href], summary, [tabindex], [contenteditable]";
    const pending = new Map();

    // ------------------------------------------------------------ channel

    function uuid() {
      try {
        if (typeof crypto.randomUUID === "function") return crypto.randomUUID();
      } catch (_) {}
      const bytes = new Uint8Array(16);
      try {
        crypto.getRandomValues(bytes);
      } catch (_) {
        for (let i = 0; i < bytes.length; i++) bytes[i] = Math.floor(Math.random() * 256);
      }
      bytes[6] = (bytes[6] & 0x0f) | 0x40;
      bytes[8] = (bytes[8] & 0x3f) | 0x80;
      const hex = Array.from(bytes, b => b.toString(16).padStart(2, "0")).join("");
      return hex.slice(0, 8) + "-" + hex.slice(8, 12) + "-" + hex.slice(12, 16) + "-" + hex.slice(16, 20) + "-" + hex.slice(20);
    }

    // The limit is checked in UTF-8 bytes. That is never less than the
    // string's length, so it also holds for hosts that count characters.
    function byteLength(text) {
      try {
        return new TextEncoder().encode(text).length;
      } catch (_) {}
      let bytes = 0;
      for (const ch of text) {
        const code = ch.codePointAt(0);
        bytes += code < 0x80 ? 1 : code < 0x800 ? 2 : code < 0x10000 ? 3 : 4;
      }
      return bytes;
    }

    function announce(id, receipt) {
      try {
        window.dispatchEvent(new CustomEvent("drostje:receipt", { detail: { id, status: receipt.status, reason: receipt.reason } }));
      } catch (_) {}
    }

    function settle(id, status, reason) {
      const entry = pending.get(id);
      if (!entry) return;
      clearTimeout(entry.timer);
      const receipt = Object.freeze({ status, reason: reason == null ? null : String(reason) });
      if (status === "queued") {
        // Acknowledged but waiting: no timeout any more, the final status follows.
        entry.timer = 0;
        announce(id, receipt);
        if (entry.progress) {
          try {
            entry.progress(receipt);
          } catch (_) {}
        }
        return;
      }
      pending.delete(id);
      entry.resolve(receipt);
      announce(id, receipt);
    }

    // progress(receipt) hears intermediate "queued" receipts; the promise only
    // resolves with a final status.
    // final: the page's purpose is fulfilled once this event is delivered, so DROSTJE
    // collapses the artifact (a docked one moves into the transcript).
    function transmit(type, data, progress, final) {
      try {
        if (typeof type !== "string" || !TYPE.test(type)) {
          return Promise.reject(new TypeError("Invalid event type: " + String(type).slice(0, 80)));
        }
        const body = JSON.stringify(data === undefined ? null : data);
        if (body === undefined) return Promise.reject(new TypeError("Event data is not JSON-serialisable"));
        const id = uuid();
        // Same bytes as JSON.stringify({v: 1, id, type, data, final}); final only when true.
        const encoded = '{"v":1,"id":' + JSON.stringify(id) + ',"type":' + JSON.stringify(type) + ',"data":' + body + (final ? ',"final":true' : "") + "}";
        if (byteLength(encoded) > MAX_BYTES) return Promise.reject(new RangeError("Event exceeds 32 KiB"));
        return new Promise(resolve => {
          const timer = setTimeout(() => settle(id, "unknown", "No receipt from DROSTJE"), RECEIPT_TIMEOUT_MS);
          pending.set(id, { resolve, timer, progress });
          try {
            emit(PREFIX + encoded);
          } catch (_) {
            settle(id, "failed", "Could not reach DROSTJE");
          }
        });
      } catch (error) {
        return Promise.reject(error);
      }
    }

    // Script sends keep the page open unless they say the answer is final.
    function send(type, data, options) {
      return transmit(type, data, null, !!(options && options.final === true));
    }

    function __receipt(id, status, reason) {
      try {
        settle(String(id), STATUSES.includes(status) ? status : "unknown", reason);
      } catch (_) {}
    }

    function __theme(cssText) {
      try {
        const doc = document;
        const parent = doc.head || doc.documentElement;
        if (!parent) {
          doc.addEventListener("DOMContentLoaded", () => __theme(cssText), { once: true });
          return;
        }
        let style = doc.getElementById("drostje-theme");
        if (!style || style.localName !== "style") {
          // Unlayered, after the kit layer and before page styles, so the page still wins.
          style = doc.createElement("style");
          style.id = "drostje-theme";
          const kit = doc.getElementById("drostje-kit");
          if (kit && kit.parentNode) kit.parentNode.insertBefore(style, kit.nextSibling);
          else parent.insertBefore(style, parent.firstChild);
        }
        style.textContent = cssText == null ? "" : String(cssText);
      } catch (_) {}
    }

    Object.defineProperty(window, "drostje", {
      value: Object.freeze({ version: 1, send, __receipt, __theme }),
      writable: false,
      configurable: false,
      enumerable: false,
    });

    // ------------------------------------------------------------ helpers

    const guard = fn => function (event) {
      try {
        fn(event);
      } catch (_) {}
    };

    function elementOf(node) {
      return node && node.nodeType === 1 ? node : node && node.parentElement;
    }

    function isCheckable(el) {
      return !!el && el.localName === "input" && (el.type === "radio" || el.type === "checkbox");
    }

    function isSubmitButton(el) {
      if (!el) return false;
      if (el.localName === "button") return el.type === "submit";
      return el.localName === "input" && (el.type === "submit" || el.type === "image");
    }

    function isNativeButton(el) {
      return el.localName === "button" || (el.localName === "input" && /^(button|submit|reset|image)$/.test(el.type));
    }

    function isTextEntry(el) {
      if (el.isContentEditable) return true;
      const tag = el.localName;
      if (tag === "textarea" || tag === "select") return true;
      return tag === "input" && !/^(radio|checkbox|button|submit|reset|image)$/.test(el.type);
    }

    function inFlight(el) {
      const state = el.getAttribute("data-drostje-state");
      return state === "sending" || state === "queued";
    }

    function isDisabled(el) {
      return el.disabled === true || el.getAttribute("aria-disabled") === "true";
    }

    function setDisabled(el, on) {
      if (typeof el.disabled === "boolean") el.disabled = on;
      else if (on) el.setAttribute("aria-disabled", "true");
      else el.removeAttribute("aria-disabled");
    }

    function fire(el, type) {
      el.dispatchEvent(new Event(type, { bubbles: true }));
    }

    // queued is an in-flight state; unknown counts as failed for styling.
    function outcome(receipt) {
      switch (receipt.status) {
        case "delivered":
          return { state: "sent", text: "Sent to the agent" };
        case "queued":
          return { state: "queued", text: "Queued \u2014 " + (receipt.reason || "the agent receives it when it is free") };
        case "failed":
          return { state: "failed", text: receipt.reason || "Not delivered" };
        default:
          return { state: "failed", text: "Delivery unknown \u2014 check the conversation" };
      }
    }

    // Resolves with the final receipt; onQueued hears intermediate queued receipts.
    function deliver(type, data, onQueued, final) {
      return transmit(type, data, onQueued, final).catch(error => ({ status: "failed", reason: (error && error.message) || null }));
    }

    function showStatus(el, state, text) {
      if (!el) return;
      el.setAttribute("data-state", state);
      el.textContent = text;
      if (!el.hasAttribute("role")) el.setAttribute("role", "status");
    }

    // Disabling a focused button drops focus to <body>; give it back.
    function restoreFocus(el) {
      const active = document.activeElement;
      if (el && el.isConnected && !el.disabled && (!active || active === document.body || active === document.documentElement)) {
        el.focus({ preventScroll: true });
      }
    }

    // ------------------------------------------------------------ form route

    // Named, enabled controls: text-like -> string (a repeated name -> array),
    // radio -> checked value or null, checkbox group -> checked values, a lone
    // checkbox without a value -> boolean, select[multiple] -> array.
    function serialize(form, submitter) {
      const fields = new Map();
      for (const el of Array.from(form.elements)) {
        const name = el.name;
        const tag = el.localName;
        if (!name || el.disabled || tag === "button" || tag === "fieldset" || tag === "output" || tag === "object") continue;
        if (tag === "input" && /^(submit|reset|button|image|file)$/.test(el.type)) continue;
        try {
          if (el.matches(":disabled")) continue;
        } catch (_) {}
        const kind = tag === "input" && (el.type === "checkbox" || el.type === "radio") ? el.type : tag === "select" && el.multiple ? "multiple" : "text";
        let field = fields.get(name);
        if (!field) fields.set(name, (field = { kind, count: 0, plain: false, values: [] }));
        field.count += 1;
        if (kind === "checkbox") {
          field.plain = !el.hasAttribute("value") || el.value === "on";
          if (el.checked) field.values.push(el.value);
        } else if (kind === "radio") {
          if (el.checked) field.values.push(el.value);
        } else if (kind === "multiple") {
          for (const option of Array.from(el.options)) if (option.selected) field.values.push(option.value);
        } else {
          field.values.push(String(el.value));
        }
      }
      const data = {};
      for (const [name, field] of fields) {
        if (field.kind === "checkbox") {
          data[name] = field.count === 1 && field.plain ? field.values.length === 1 : field.values;
        } else if (field.kind === "radio") {
          data[name] = field.values.length ? field.values[0] : null;
        } else if (field.kind === "multiple") {
          data[name] = field.values;
        } else {
          data[name] = field.count === 1 ? field.values[0] : field.values;
        }
      }
      if (submitter && submitter.name) data[submitter.name] = submitter.value;
      return data;
    }

    function onSubmit(event) {
      const form = event.target;
      if (!form || form.localName !== "form" || !form.hasAttribute("data-drostje")) return;
      event.preventDefault();
      if (inFlight(form)) return;
      const type = form.getAttribute("data-drostje") || "submit";
      const data = serialize(form, event.submitter || null);
      const final = !keepsOpen(form) && !keepsOpen(event.submitter);
      const submits = Array.from(form.elements).filter(isSubmitButton);
      const disabled = submits.filter(button => !button.disabled);
      let status = form.querySelector("[data-drostje-status]");
      if (!status) {
        status = document.createElement("span");
        status.className = "status";
        status.setAttribute("data-drostje-status", "");
        const last = submits[submits.length - 1];
        if (last && last.parentNode) last.parentNode.insertBefore(status, last.nextSibling);
        else form.appendChild(status);
      }
      const focused = document.activeElement;
      form.setAttribute("data-drostje-state", "sending");
      form.setAttribute("aria-busy", "true");
      for (const button of disabled) button.disabled = true;
      showStatus(status, "sending", "Sending\u2026");
      // Submit buttons stay disabled while queued; the form is never reset, so
      // the user's input stays for a retry or an edit.
      const progress = guard(receipt => {
        const result = outcome(receipt);
        form.setAttribute("data-drostje-state", result.state);
        showStatus(status, result.state, result.text);
      });
      deliver(type, data, progress, final).then(guard(receipt => {
        const result = outcome(receipt);
        form.setAttribute("data-drostje-state", result.state);
        form.removeAttribute("aria-busy");
        for (const button of disabled) button.disabled = false;
        showStatus(status, result.state, result.text);
        restoreFocus(focused);
      }));
    }

    function requestSubmit(form) {
      if (inFlight(form)) return;
      if (typeof form.requestSubmit === "function") {
        form.requestSubmit();
        return;
      }
      const button = Array.from(form.elements).find(el => isSubmitButton(el) && !el.disabled);
      if (button) button.click();
      else form.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
    }

    // ------------------------------------------------------------ choose route

    // The nearest ancestor holding a status, skipping statuses owned by another
    // form route or another once-container.
    function findStatus(el) {
      for (let node = el.parentElement; node; node = node.parentElement) {
        for (const status of Array.from(node.querySelectorAll("[data-drostje-status]"))) {
          const owner = status.closest("form[data-drostje], [data-drostje-once]");
          if (!owner || owner.contains(el)) return status;
        }
      }
      return null;
    }

    // Multi-step pages mark answers that should leave the page open.
    function keepsOpen(el) {
      return !!(el && el.closest && el.closest("[data-drostje-keep-open]"));
    }

    function choose(el) {
      const type = el.getAttribute("data-drostje-send") || "choose";
      const value = (typeof el.value === "string" && el.value) || el.dataset.value || null;
      const label = (el.textContent || "").replace(/\s+/g, " ").trim().slice(0, 200);
      const once = el.closest("[data-drostje-once]");
      const group = once || el.parentElement;
      const peers = group ? Array.from(group.querySelectorAll("[data-drostje-send]")) : [el];
      // One current choice per group: the previous one loses its marks.
      for (const peer of peers) {
        if (peer === el) continue;
        if (peer.getAttribute("aria-pressed") === "true") peer.setAttribute("aria-pressed", "false");
        peer.removeAttribute("data-drostje-state");
      }
      el.setAttribute("aria-pressed", "true");
      el.setAttribute("data-drostje-state", "sending");
      // Inert until a final receipt, without native disabling, so focus stays put.
      const marked = el.getAttribute("aria-disabled") !== "true";
      if (marked) el.setAttribute("aria-disabled", "true");
      // A once-container takes one answer: lock the others while this one is in
      // flight (queued included); keep them locked once it is delivered.
      const locked = once ? peers.filter(peer => peer !== el && !isDisabled(peer)) : [];
      for (const peer of locked) setDisabled(peer, true);
      const status = findStatus(el);
      showStatus(status, "sending", "Sending\u2026");
      const progress = guard(receipt => {
        const result = outcome(receipt);
        el.setAttribute("data-drostje-state", result.state);
        showStatus(status, result.state, result.text);
      });
      deliver(type, { value, label }, progress, !keepsOpen(el)).then(guard(receipt => {
        const result = outcome(receipt);
        el.setAttribute("data-drostje-state", result.state);
        showStatus(status, result.state, result.text);
        if (marked) el.removeAttribute("aria-disabled");
        if (once && receipt.status === "delivered") {
          for (const peer of Array.from(once.querySelectorAll("[data-drostje-send]"))) setDisabled(peer, true);
        } else {
          for (const peer of locked) setDisabled(peer, false);
        }
      }));
    }

    function onClick(event) {
      const target = elementOf(event.target);
      const el = target && target.closest("[data-drostje-send]");
      if (!el) return;
      // A submit button of a form route belongs to that route (as its submitter).
      if (isSubmitButton(el) && el.form && el.form.hasAttribute("data-drostje")) return;
      // Labels and their inputs keep their default so the choice input still toggles.
      const passive = el.localName === "label" || isCheckable(target);
      if (inFlight(el)) {
        // A repeat, or the label's own follow-up click on its control.
        if (!passive) event.preventDefault();
        return;
      }
      if (isDisabled(el)) {
        event.preventDefault();
        return;
      }
      if (!passive) event.preventDefault();
      choose(el);
    }

    // ------------------------------------------------------------ choice lists

    let gAt = 0;
    let gList = null;

    function listItems(list) {
      let root = list;
      if (list.getAttribute("role") === "listbox") {
        return Array.from(list.querySelectorAll("[role=option]")).filter(rendered);
      }
      const kids = Array.from(root.children);
      if (kids.length === 1 && /^(ul|ol)$/.test(kids[0].localName) && !/^(ul|ol)$/.test(root.localName)) root = kids[0];
      return Array.from(root.children).filter(item => focusTarget(item) && rendered(item));
    }

    function rendered(el) {
      try {
        return el.getClientRects().length > 0;
      } catch (_) {
        return true;
      }
    }

    // The control that takes focus for an item: the item itself, or the first
    // enabled focusable inside it (a radio group's inputs are all reachable).
    function focusTarget(item) {
      if (item.matches(FOCUSABLE + ", [role=option], [data-drostje-send]")) return item.disabled ? null : item;
      for (const el of Array.from(item.querySelectorAll(FOCUSABLE))) if (!el.disabled) return el;
      return null;
    }

    function focusItem(item) {
      const el = focusTarget(item);
      if (!el) return;
      if (!el.matches(FOCUSABLE)) el.setAttribute("tabindex", "-1");
      el.focus();
    }

    function selectOption(event, list, target) {
      if (isCheckable(target)) {
        // Native Space/Return handling differs per engine; do it once, here.
        event.preventDefault();
        if (!target.disabled) target.click();
        return;
      }
      if (target.getAttribute("role") === "option") {
        event.preventDefault();
        if (list.getAttribute("aria-multiselectable") === "true") {
          target.setAttribute("aria-selected", target.getAttribute("aria-selected") === "true" ? "false" : "true");
        } else {
          for (const option of Array.from(list.querySelectorAll("[role=option]"))) {
            option.setAttribute("aria-selected", option === target ? "true" : "false");
          }
        }
        target.click();
      }
      // Buttons, links and summaries keep their own activation.
    }

    function onKeyDown(event) {
      if (event.defaultPrevented || event.isComposing) return;
      const target = elementOf(event.target);
      if (!target) return;
      const key = event.key;

      if (key === "Enter" && (event.metaKey || event.ctrlKey) && !event.altKey) {
        const form = target.form && target.form.hasAttribute("data-drostje") ? target.form : target.closest("form[data-drostje]");
        if (form) {
          event.preventDefault();
          requestSubmit(form);
        }
        return;
      }
      if (event.metaKey || event.ctrlKey || event.altKey || isTextEntry(target)) return;

      if ((key === "Enter" || key === " ") && target.hasAttribute("data-drostje-send") && !isNativeButton(target)) {
        event.preventDefault();
        if (!inFlight(target) && !isDisabled(target)) choose(target);
        return;
      }

      const list = target.closest(LISTS);
      if (!list) return;
      if (key !== "g") gList = null;
      if (key === "Enter" || key === " ") {
        selectOption(event, list, target);
        return;
      }
      const items = listItems(list);
      if (!items.length) return;
      const index = items.findIndex(item => item === target || item.contains(target));
      let next;
      if (key === "j" || key === "ArrowDown") next = index < 0 ? 0 : Math.min(index + 1, items.length - 1);
      else if (key === "k" || key === "ArrowUp") next = index < 0 ? 0 : Math.max(index - 1, 0);
      else if (key === "G") next = items.length - 1;
      else if (key === "g") {
        const now = Date.now();
        if (gList === list && now - gAt <= DOUBLE_G_MS) {
          gList = null;
          next = 0;
        } else {
          gList = list;
          gAt = now;
          event.preventDefault();
          return;
        }
      } else return;
      // Also stops native radio arrows from moving the selection with focus.
      event.preventDefault();
      if (next !== index) focusItem(items[next]);
    }

    // ------------------------------------------------------------ label mirror

    // Servo 0.7 does not re-evaluate :has() on state changes, so choice labels
    // carry their input's state as attributes for kit.css.
    function choiceLabels(input) {
      const labels = [];
      try {
        if (input.labels) for (const label of Array.from(input.labels)) labels.push(label);
      } catch (_) {}
      const wrapper = input.closest("label");
      if (wrapper && !labels.includes(wrapper)) labels.push(wrapper);
      return labels.filter(label => label.closest(".choices"));
    }

    function mirror(input) {
      for (const label of choiceLabels(input)) {
        label.setAttribute("data-drostje-choice", input.type);
        label.setAttribute("data-drostje-checked", input.checked ? "true" : "false");
      }
    }

    function onChange(event) {
      const input = event.target;
      if (!isCheckable(input)) return;
      if (input.type !== "radio" || !input.name) {
        mirror(input);
        return;
      }
      const pool = input.form ? Array.from(input.form.elements) : Array.from(document.querySelectorAll("input[type=radio]"));
      for (const radio of pool) {
        if (radio.type === "radio" && radio.name === input.name && radio.form === input.form) mirror(radio);
      }
    }

    function onFocus(event) {
      if (!isCheckable(event.target)) return;
      for (const label of choiceLabels(event.target)) label.setAttribute("data-drostje-focus", "");
    }

    function onBlur(event) {
      if (!isCheckable(event.target)) return;
      for (const label of choiceLabels(event.target)) label.removeAttribute("data-drostje-focus");
    }

    function mirrorAll() {
      for (const label of Array.from(document.querySelectorAll(".choices label"))) {
        const input = label.control || label.querySelector("input");
        if (isCheckable(input)) mirror(input);
      }
    }

    // ------------------------------------------------------------ wiring

    const doc = document;
    doc.addEventListener("submit", guard(onSubmit), true);
    doc.addEventListener("click", guard(onClick), true);
    doc.addEventListener("keydown", guard(onKeyDown), true);
    doc.addEventListener("change", guard(onChange), true);
    doc.addEventListener("focus", guard(onFocus), true);
    doc.addEventListener("blur", guard(onBlur), true);
    if (doc.readyState === "loading") doc.addEventListener("DOMContentLoaded", guard(mirrorAll), { once: true });
    else guard(mirrorAll)();
  } catch (_) {
    // The kit must never break the page.
  }
})();
