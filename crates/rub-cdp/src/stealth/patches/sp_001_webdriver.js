(() => {
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

    stripProperty(navigator, 'webdriver');
})();
