(() => {
    const root = globalThis;
    const registryKey = Symbol.for('rub.stealth.native_string_registry');
    const installKey = Symbol.for('rub.stealth.native_string_installed');
    const markerKey = Symbol.for('rub.stealth.mark_native');

    if (!root[registryKey]) {
        Object.defineProperty(root, registryKey, {
            value: new WeakMap(),
            configurable: true,
        });
    }

    const registry = root[registryKey];

    if (!root[installKey]) {
        const nativeToString = Function.prototype.toString;
        const proxyToString = new Proxy(nativeToString, {
            apply(target, thisArg, args) {
                try {
                    if (registry.has(thisArg)) {
                        return registry.get(thisArg);
                    }
                } catch (_) {}
                return Reflect.apply(target, thisArg, args);
            },
        });

        Object.defineProperty(Function.prototype, 'toString', {
            value: proxyToString,
            configurable: true,
            writable: true,
        });

        Object.defineProperty(root, markerKey, {
            value: (wrapper, original, nativeSource) => {
                if (typeof wrapper !== 'function') return wrapper;
                let source = typeof nativeSource === 'string' ? nativeSource : '';
                if (!source && typeof original === 'function') {
                    try {
                        source = Reflect.apply(nativeToString, original, []);
                    } catch (_) {}
                }
                if (!source) {
                    const name = String(
                        wrapper.name || (typeof original === 'function' ? original.name : '') || ''
                    );
                    source = `function ${name}() { [native code] }`;
                }
                try {
                    registry.set(wrapper, source);
                } catch (_) {}
                return wrapper;
            },
            configurable: true,
        });

        Object.defineProperty(root, installKey, {
            value: true,
            configurable: true,
        });
    }
})();
