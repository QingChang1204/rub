pub const LOCATOR_JS_HELPERS: &str = r#"
const normalize = (value) => String(value ?? '')
    .replace(/\s+/g, ' ')
    .trim()
    .toLocaleLowerCase();
const textish = (...values) => values
    .map((value) => normalize(value))
    .find((value) => value.length > 0) || '';
const fallbackRole = (el) => {
    const tag = String(el.tagName || '').toLowerCase();
    const landmarkContext = (candidate) => {
        if (!candidate || typeof candidate.closest !== 'function') return false;
        return !!candidate.closest('article, aside, main, nav, section');
    };
    if (tag === 'button') return 'button';
    if (tag === 'a' && el.hasAttribute('href')) return 'link';
    if (tag === 'textarea') return 'textbox';
    if (tag === 'select') return 'combobox';
    if (tag === 'option') return 'option';
    if (/^h[1-6]$/.test(tag)) return 'heading';
    if (tag === 'main') return 'main';
    if (tag === 'nav') return 'navigation';
    if (tag === 'aside') return 'complementary';
    if (tag === 'article') return 'article';
    if (tag === 'form') return 'form';
    if (tag === 'header') return landmarkContext(el) ? '' : 'banner';
    if (tag === 'footer') return landmarkContext(el) ? '' : 'contentinfo';
    if (tag === 'input') {
        const type = String(el.getAttribute('type') || '').toLowerCase();
        if (type === 'checkbox') return 'checkbox';
        if (type === 'radio') return 'radio';
        if (type === 'button' || type === 'submit' || type === 'reset') return 'button';
        return 'textbox';
    }
    return '';
};
const semanticRole = (el) => textish(
    el.getAttribute && el.getAttribute('role'),
    fallbackRole(el)
);
const accessibleLabel = (el) => {
    const labels = [];
    if (el.getAttribute) {
        labels.push(el.getAttribute('aria-label'));
        labels.push(el.getAttribute('placeholder'));
        labels.push(el.getAttribute('title'));
        labels.push(el.getAttribute('alt'));
        labels.push(el.getAttribute('value'));
        labels.push(el.getAttribute('name'));
    }
    const id = el.getAttribute && el.getAttribute('id');
    if (id) {
        const viaFor = document.querySelector(`label[for="${CSS.escape(id)}"]`);
        if (viaFor) labels.push(viaFor.innerText || viaFor.textContent);
    }
    if (typeof el.closest === 'function') {
        const viaClosest = el.closest('label');
        if (viaClosest) labels.push(viaClosest.innerText || viaClosest.textContent);
    }
    labels.push(el.innerText || el.textContent);
    return textish(...labels);
};
const accessibleDescription = (el) => {
    if (!el || !el.getAttribute) return '';
    const descriptions = [];
    descriptions.push(el.getAttribute('aria-description'));
    const describedBy = String(el.getAttribute('aria-describedby') || '')
        .split(/\s+/)
        .map((part) => part.trim())
        .filter((part) => part.length > 0);
    for (const id of describedBy) {
        const node = document.getElementById(id);
        if (node) descriptions.push(node.innerText || node.textContent);
    }
    return textish(...descriptions);
};
const testingId = (el) => {
    if (!el.getAttribute) return '';
    return textish(
        el.getAttribute('data-testid'),
        el.getAttribute('data-test-id'),
        el.getAttribute('data-test')
    );
};
const allElements = () => Array.from(document.querySelectorAll('*'));
const selectMatches = (elements, selection) => {
    if (!selection) return elements;
    switch (selection) {
        case 'first':
            return elements.slice(0, 1);
        case 'last':
            return elements.slice(-1);
        default:
            if (
                typeof selection === 'object' &&
                selection !== null &&
                Number.isInteger(selection.nth)
            ) {
                const selected = elements[selection.nth];
                return selected ? [selected] : [];
            }
            return elements;
    }
};
const resolveLocatorMatches = (locator) => {
    switch (locator.kind) {
        case 'selector':
            try {
                return Array.from(document.querySelectorAll(locator.css));
            } catch (error) {
                throw new Error(String(error && error.message ? error.message : error));
            }
        case 'target_text': {
            const needle = normalize(locator.text);
            if (!needle) return [];
            const candidates = allElements();
            const exact = candidates.filter((el) => {
                const candidate = textish(
                    el.innerText,
                    el.textContent,
                    el.getAttribute && el.getAttribute('aria-label'),
                    el.getAttribute && el.getAttribute('title'),
                    el.getAttribute && el.getAttribute('placeholder')
                );
                return candidate === needle;
            });
            if (exact.length) return exact;
            return candidates.filter((el) => {
                const candidate = textish(
                    el.innerText,
                    el.textContent,
                    el.getAttribute && el.getAttribute('aria-label'),
                    el.getAttribute && el.getAttribute('title'),
                    el.getAttribute && el.getAttribute('placeholder')
                );
                return candidate.includes(needle);
            });
        }
        case 'role': {
            const needle = normalize(locator.role);
            if (!needle) return [];
            return allElements().filter((el) => semanticRole(el) === needle);
        }
        case 'label': {
            const needle = normalize(locator.label);
            if (!needle) return [];
            const candidates = allElements();
            const exact = candidates.filter((el) => accessibleLabel(el) === needle);
            if (exact.length) return exact;
            return candidates.filter((el) => {
                const candidate = accessibleLabel(el);
                return candidate.length > 0 && candidate.includes(needle);
            });
        }
        case 'test_id': {
            const needle = normalize(locator.testid);
            if (!needle) return [];
            return allElements().filter((el) => testingId(el) === needle);
        }
        default:
            throw new Error(`unsupported locator kind: ${locator.kind}`);
    }
};
"#;
