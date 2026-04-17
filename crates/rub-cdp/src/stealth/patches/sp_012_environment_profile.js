(() => {
    const expectedScreenWidth = __RUB_SCREEN_WIDTH__;
    const expectedScreenHeight = __RUB_SCREEN_HEIGHT__;
    const expectedOuterWidth = __RUB_OUTER_WIDTH__;
    const expectedOuterHeight = __RUB_OUTER_HEIGHT__;
    const expectedTouchPoints = __RUB_MAX_TOUCH_POINTS__;
    const touchEnabled = __RUB_TOUCH_ENABLED__;
    const markNative = globalThis[Symbol.for('rub.stealth.mark_native')];

    const defineGetter = (target, prop, getter) => {
        if (!target) return;
        try {
            const wrappedGetter = function() {
                return getter();
            };
            Object.defineProperty(target, prop, {
                get: typeof markNative === 'function'
                    ? markNative(
                        wrappedGetter,
                        undefined,
                        `function get ${String(prop)}() { [native code] }`
                    )
                    : wrappedGetter,
                configurable: true,
            });
        } catch (_) {}
    };

    const stripProperty = (root, prop) => {
        const visited = new Set();
        let current = root;
        while (current && !visited.has(current)) {
            visited.add(current);
            try {
                const desc = Object.getOwnPropertyDescriptor(current, prop);
                if (desc && desc.configurable) {
                    delete current[prop];
                }
            } catch (_) {}
            current = Object.getPrototypeOf(current);
        }
    };

    defineGetter(screen, 'width', () => expectedScreenWidth);
    defineGetter(screen, 'availWidth', () => expectedScreenWidth);
    defineGetter(screen, 'height', () => expectedScreenHeight);
    defineGetter(screen, 'availHeight', () => expectedScreenHeight);
    defineGetter(window, 'outerWidth', () =>
        Math.max(expectedOuterWidth, Number(window.innerWidth) || 0)
    );
    defineGetter(window, 'outerHeight', () =>
        Math.max(expectedOuterHeight, Number(window.innerHeight) || 0)
    );
    stripProperty(navigator, 'maxTouchPoints');
    defineGetter(
        (typeof Navigator !== 'undefined' && Navigator.prototype) || Object.getPrototypeOf(navigator),
        'maxTouchPoints',
        () => expectedTouchPoints
    );

    if (!touchEnabled) {
        stripProperty(window, 'ontouchstart');
        stripProperty(Window.prototype, 'ontouchstart');
        stripProperty(Document.prototype, 'ontouchstart');
        stripProperty(HTMLElement.prototype, 'ontouchstart');
    }
})();
