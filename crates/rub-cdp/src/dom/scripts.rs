use rub_core::error::RubError;
use rub_core::model::Snapshot;

/// JavaScript template to extract interactive elements from the page.
///
/// `__INCLUDE_LISTENERS__` is replaced at runtime so the listener-augmented
/// projection and the plain interactive projection share one classifier authority.
const EXTRACT_ELEMENTS_JS_TEMPLATE: &str = r##"
(() => {
    const includeListeners = __INCLUDE_LISTENERS__;
    const interactiveTags = new Set(['a', 'button', 'input', 'textarea', 'select', 'option']);
    const interactiveRoles = new Set([
        'button', 'link', 'menuitem', 'tab', 'checkbox', 'radio',
        'switch', 'textbox', 'combobox', 'listbox', 'option'
    ]);

    const elements = [];
    const root = document.body || document.documentElement;
    let index = 0;
    let domIndex = 0;

    function getTag(el) {
        const tag = el.tagName.toLowerCase();
        if (tag === 'a') return 'link';
        if (tag === 'textarea') return 'textarea';
        if (tag === 'select') return 'select';
        if (tag === 'option') return 'option';
        if (tag === 'input') {
            const type = el.type || 'text';
            if (type === 'checkbox') return 'checkbox';
            if (type === 'radio') return 'radio';
            return 'input';
        }
        if (tag === 'button') return 'button';
        return 'other';
    }

    function isInteractive(el) {
        const tag = el.tagName.toLowerCase();
        if (interactiveTags.has(tag)) return true;
        if (el.isContentEditable) return true;
        const role = el.getAttribute('role');
        if (role && interactiveRoles.has(role)) return true;
        if (el.onclick || el.hasAttribute('onclick')) return true;
        if (el.getAttribute('tabindex') !== null) return true;
        return false;
    }

    function getListeners(el) {
        if (!includeListeners || typeof getEventListeners !== 'function') return [];
        try {
            const listeners = getEventListeners(el);
            return Object.keys(listeners)
                .filter((name) => Array.isArray(listeners[name]) && listeners[name].length > 0)
                .sort();
        } catch (_) {
            return [];
        }
    }

    function getText(el) {
        return (el.textContent || '').trim().substring(0, 200);
    }

    function getAttrs(el) {
        const attrs = {};
        for (const name of ['href', 'placeholder', 'aria-label', 'aria-readonly', 'type', 'name', 'value', 'role', 'title', 'alt', 'id', 'data-testid', 'data-test-id', 'data-test', 'contenteditable']) {
            const val = el.getAttribute(name);
            if (val != null && val !== '') attrs[name] = val;
        }
        if (el.isContentEditable && !('contenteditable' in attrs)) {
            attrs.contenteditable = 'true';
        }
        if (el.hasAttribute && el.hasAttribute('disabled')) {
            attrs.disabled = '';
        }
        if (el.hasAttribute && el.hasAttribute('readonly')) {
            attrs.readonly = '';
        }
        return attrs;
    }

    function getRect(el) {
        const r = el.getBoundingClientRect();
        let x = r.x;
        let y = r.y;
        let current = window;
        while (current !== current.top) {
            try {
                const frameEl = current.frameElement;
                if (!frameEl) return null;
                const fr = frameEl.getBoundingClientRect();
                x += fr.x;
                y += fr.y;
                current = current.parent;
            } catch (_) {
                return null;
            }
        }
        return { x, y, width: r.width, height: r.height };
    }

    function walk(node, depth) {
        if (!node || node.nodeType !== 1) return;
        const listeners = getListeners(node);
        if (isInteractive(node) || listeners.length > 0) {
            // L0 stealth: do NOT set any DOM attributes here.
            // Index determined by traversal order.
            const extracted = {
                index,
                dom_index: domIndex,
                depth,
                tag: getTag(node),
                text: getText(node),
                attributes: getAttrs(node),
                bounding_box: getRect(node),
            };
            if (includeListeners) {
                extracted.listeners = listeners;
            }
            elements.push(extracted);
            index++;
        }
        domIndex++;
        for (const child of Array.from(node.children || [])) {
            walk(child, depth + 1);
        }
    }

    if (root) {
        walk(root, 0);
    }

    return JSON.stringify({
        elements,
        traversal_count: domIndex,
        title: document.title,
        scroll: (() => {
            let vp = window;
            try { while (vp !== vp.top) vp = vp.parent; } catch (_) { vp = window; }
            return {
                x: vp.pageXOffset || vp.document.documentElement.scrollLeft,
                y: vp.pageYOffset || vp.document.documentElement.scrollTop,
                at_bottom: (vp.innerHeight + vp.pageYOffset) >= (vp.document.documentElement.scrollHeight - 2)
            };
        })()
    });
})()
"##;

pub(super) fn extract_elements_script(include_listeners: bool) -> String {
    EXTRACT_ELEMENTS_JS_TEMPLATE.replace(
        "__INCLUDE_LISTENERS__",
        if include_listeners { "true" } else { "false" },
    )
}

pub(crate) fn live_element_projection_fingerprint_function() -> &'static str {
    r#"function() {
        function getTag(el) {
            const tag = (el.tagName || '').toLowerCase();
            if (tag === 'a') return 'link';
            if (tag === 'textarea') return 'textarea';
            if (tag === 'select') return 'select';
            if (tag === 'option') return 'option';
            if (tag === 'input') {
                const type = el.type || 'text';
                if (type === 'checkbox') return 'checkbox';
                if (type === 'radio') return 'radio';
                return 'input';
            }
            if (tag === 'button') return 'button';
            return 'other';
        }

        function getText(el) {
            return (el.textContent || '').trim().substring(0, 200);
        }

        function getAttrs(el) {
            const attrs = {};
            for (const name of ['href', 'placeholder', 'aria-label', 'aria-readonly', 'type', 'name', 'value', 'role', 'title', 'alt', 'id', 'data-testid', 'data-test-id', 'data-test', 'contenteditable']) {
                const val = el.getAttribute && el.getAttribute(name);
                if (val != null && val !== '') attrs[name] = val;
            }
            if (el.isContentEditable && !('contenteditable' in attrs)) {
                attrs.contenteditable = 'true';
            }
            if (el.hasAttribute && el.hasAttribute('disabled')) {
                attrs.disabled = '';
            }
            if (el.hasAttribute && el.hasAttribute('readonly')) {
                attrs.readonly = '';
            }
            return attrs;
        }

        function getListeners(el) {
            if (typeof getEventListeners !== 'function') return null;
            try {
                const listeners = getEventListeners(el);
                return Object.keys(listeners)
                    .filter((name) => Array.isArray(listeners[name]) && listeners[name].length > 0)
                    .sort();
            } catch (_) {
                return null;
            }
        }

        function getRect(el) {
            const r = el.getBoundingClientRect();
            let x = r.x;
            let y = r.y;
            let current = window;
            while (current !== current.top) {
                try {
                    const frameEl = current.frameElement;
                    if (!frameEl) return null;
                    const fr = frameEl.getBoundingClientRect();
                    x += fr.x;
                    y += fr.y;
                    current = current.parent;
                } catch (_) {
                    return null;
                }
            }
            return { x, y, width: r.width, height: r.height };
        }

        function getDepth(el) {
            const root = document.body || document.documentElement;
            let depth = 0;
            let current = el;
            while (current && current !== root) {
                current = current.parentElement;
                depth += 1;
            }
            return current === root ? depth : 0;
        }

        return JSON.stringify({
            tag: getTag(this),
            text: getText(this),
            attributes: getAttrs(this),
            listeners: getListeners(this),
            bounding_box: getRect(this),
            depth: getDepth(this),
        });
    }"#
}

/// JavaScript to remove all injected highlight overlays (shadow host cleanup).
pub const CLEANUP_HIGHLIGHT_JS: &str = r##"
(() => {
    // Clean shadow DOM host
    const host = document.getElementById('__rub_overlay_host__');
    if (host) host.remove();
    // Legacy cleanup (pre-v1.4)
    const labels = document.querySelectorAll('[data-rub-highlight]');
    for (const l of labels) l.remove();
})()
"##;

/// Build the overlay script for `screenshot --highlight` from a published snapshot.
pub fn highlight_overlay_js(snapshot: &Snapshot) -> Result<String, RubError> {
    let overlays = snapshot
        .elements
        .iter()
        .filter_map(|element| {
            let bbox = element.bounding_box?;
            if bbox.width == 0.0 && bbox.height == 0.0 {
                return None;
            }
            Some(serde_json::json!({
                "index": element.index,
                "left": snapshot.scroll.x + bbox.x,
                "top": snapshot.scroll.y + bbox.y,
            }))
        })
        .collect::<Vec<_>>();
    let overlays_json = serde_json::to_string(&overlays)
        .map_err(|e| RubError::Internal(format!("Highlight overlay JSON failed: {e}")))?;

    Ok(format!(
        r#"
        (() => {{
            const overlays = {overlays_json};
            // Use an open shadow DOM host to isolate overlays from main DOM.
            // mode:'open' is required so host.shadowRoot can be read back on
            // subsequent calls; mode:'closed' causes shadowRoot to always return
            // null, making the second invocation call null.attachShadow() and
            // throw a TypeError.
            let host = document.getElementById('__rub_overlay_host__');
            let shadow;
            if (!host) {{
                host = document.createElement('div');
                host.id = '__rub_overlay_host__';
                host.style.cssText = 'position:absolute;top:0;left:0;width:0;height:0;overflow:visible;z-index:2147483647;pointer-events:none';
                // document.body may be null in SPAs during early render or on about:blank.
                const mountPoint = document.body || document.documentElement;
                if (!mountPoint) return 0;
                mountPoint.appendChild(host);
                shadow = host.attachShadow({{ mode: 'open' }});
            }} else {{
                shadow = host.shadowRoot;
                if (!shadow) {{
                    // Defensive: host exists but shadow root is inaccessible — rebuild.
                    host.remove();
                    host = document.createElement('div');
                    host.id = '__rub_overlay_host__';
                    host.style.cssText = 'position:absolute;top:0;left:0;width:0;height:0;overflow:visible;z-index:2147483647;pointer-events:none';
                    const mountPoint = document.body || document.documentElement;
                    if (!mountPoint) return 0;
                    mountPoint.appendChild(host);
                    shadow = host.attachShadow({{ mode: 'open' }});
                }}
            }}
            while (shadow.firstChild) {{
                shadow.removeChild(shadow.firstChild);
            }}
            let count = 0;
            for (const item of overlays) {{
                const label = document.createElement('div');
                label.textContent = String(item.index);
                label.style.cssText = [
                    'position:absolute',
                    `top:${{item.top}}px`,
                    `left:${{item.left}}px`,
                    'background:rgba(255,59,48,0.85)',
                    'color:#fff',
                    'font:bold 11px/14px system-ui,sans-serif',
                    'padding:1px 4px',
                    'border-radius:3px',
                    'pointer-events:none',
                    'white-space:nowrap',
                ].join(';');
                shadow.appendChild(label);
                count++;
            }}
            return count;
        }})()
        "#
    ))
}
