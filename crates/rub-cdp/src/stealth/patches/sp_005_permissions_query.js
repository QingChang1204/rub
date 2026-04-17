(() => {
    if (typeof Permissions === 'undefined') return;
    const markNative = globalThis[Symbol.for('rub.stealth.mark_native')];
    const originalQuery = Permissions.prototype.query;
    const wrappedQuery = function query(desc) {
        if (desc && desc.name === 'notifications') {
            return Promise.resolve({ state: 'prompt', onchange: null });
        }
        return originalQuery.call(this, desc);
    };
    Permissions.prototype.query =
        typeof markNative === 'function'
            ? markNative(wrappedQuery, originalQuery)
            : wrappedQuery;
})();
