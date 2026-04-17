(() => {
    if (navigator.plugins.length > 0) return; // Already has plugins (e.g., external browser)

    function FakePlugin(name, description, filename, mimeType) {
        return Object.create(Plugin.prototype, {
            name: { value: name, enumerable: true },
            description: { value: description, enumerable: true },
            filename: { value: filename, enumerable: true },
            length: { value: 1, enumerable: true },
            0: { value: { type: mimeType, suffixes: '', description: '', enabledPlugin: null } },
        });
    }

    const plugins = [
        FakePlugin('PDF Viewer', 'Portable Document Format', 'internal-pdf-viewer', 'application/pdf'),
        FakePlugin('Chrome PDF Plugin', 'Portable Document Format', 'internal-pdf-viewer', 'application/x-google-chrome-pdf'),
        FakePlugin('Chrome PDF Viewer', '', 'mhjfbmdgcfjbbpaeojofohoefgiehjai', 'application/pdf'),
        FakePlugin('Native Client', '', 'internal-nacl-plugin', 'application/x-nacl'),
    ];

    Object.defineProperty(navigator, 'plugins', {
        get: () => {
            const arr = Object.create(PluginArray.prototype);
            for (let i = 0; i < plugins.length; i++) {
                arr[i] = plugins[i];
            }
            Object.defineProperty(arr, 'length', { value: plugins.length });
            arr.item = (i) => plugins[i] || null;
            arr.namedItem = (name) => plugins.find(p => p.name === name) || null;
            arr.refresh = () => {};
            return arr;
        },
        configurable: true,
    });
})();
