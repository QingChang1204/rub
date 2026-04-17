(() => {
    if (navigator.languages && navigator.languages.length > 0) return;
    Object.defineProperty(navigator, 'languages', {
        get: () => Object.freeze(['en-US', 'en']),
        configurable: true,
    });
    if (!navigator.language) {
        Object.defineProperty(navigator, 'language', {
            get: () => 'en-US',
            configurable: true,
        });
    }
})();
