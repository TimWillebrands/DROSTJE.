// Runs through Servo UserScript when the document's head is attached.
// Observes real outcomes, never reads response bodies or replaces native requests.
(() => {
    const emit = console.warn.bind(console);
    const stringify = JSON.stringify;
    const text = value => { try { return String(value).slice(0, 2048); } catch (_) { return '<unprintable>'; } };
    const report = (kind, details) => {
        try { emit('__DROSTJE_DIAGNOSTIC__' + stringify({kind, ...details})); } catch (_) {}
    };
    addEventListener('error', event => {
        const target = event.target;
        if (target && target !== window) {
            report('network', {type: 'resource_error', url: text(target.currentSrc || target.src || target.href || ''), element: text(target.localName || '')});
        } else {
            report('runtime', {type: 'error', message: text(event.message), url: text(event.filename), line: event.lineno, column: event.colno});
        }
    }, true);
    addEventListener('unhandledrejection', event => {
        report('runtime', {type: 'unhandled_rejection', message: text(event.reason)});
    });
    if (typeof fetch === 'function') {
        const original = fetch;
        window.fetch = function(...args) {
            // Do not coerce caller-supplied objects or inspect Request bodies.
            const url = typeof args[0] === 'string' ? args[0].slice(0, 2048) : '<Request or URL object>';
            return Reflect.apply(original, this, args).then(response => {
                if (response.status >= 400) report('network', {type: 'http_error', api: 'fetch', url: text(response.url || url), status: response.status});
                return response;
            }, error => {
                report('network', {type: 'request_failed', api: 'fetch', url, message: text(error)});
                throw error;
            });
        };
    }
    const open = XMLHttpRequest.prototype.open;
    const send = XMLHttpRequest.prototype.send;
    const requests = new WeakMap();
    XMLHttpRequest.prototype.open = function(...args) {
        const result = Reflect.apply(open, this, args);
        requests.set(this, typeof args[1] === 'string' ? args[1].slice(0, 2048) : '<URL object>');
        return result;
    };
    XMLHttpRequest.prototype.send = function(...args) {
        const xhr = this;
        const url = requests.get(xhr) || '';
        const complete = event => {
            if (event.type === 'load' && xhr.status >= 400) {
                report('network', {type: 'http_error', api: 'xhr', url: text(xhr.responseURL || url), status: xhr.status});
            } else if (event.type !== 'load') {
                report('network', {type: 'request_failed', api: 'xhr', url, reason: event.type});
            }
        };
        const types = ['load', 'error', 'timeout', 'abort'];
        const cleanup = () => {
            for (const type of types) xhr.removeEventListener(type, complete);
            xhr.removeEventListener('loadend', cleanup);
        };
        for (const type of types) xhr.addEventListener(type, complete);
        xhr.addEventListener('loadend', cleanup);
        try { return Reflect.apply(send, xhr, args); }
        catch (error) { cleanup(); throw error; }
    };
})();
