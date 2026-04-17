(() => {
    const root = globalThis;
    const markNative = root[Symbol.for('rub.stealth.mark_native')];
    const installWorkerBridge = (globalName) => {
        const NativeCtor = root[globalName];
        if (typeof NativeCtor !== 'function') return;
        if (NativeCtor.__rubWorkerBridge === true) return;

        const workerBootstrap = `(() => {
            const workerRoot = globalThis;
            const nav = workerRoot.navigator;
            const stripProperty = (target, prop) => {
                const visited = new Set();
                let current = target;
                while (current && !visited.has(current)) {
                    visited.add(current);
                    try {
                        const desc = Object.getOwnPropertyDescriptor(current, prop);
                        if (desc && desc.configurable) delete current[prop];
                    } catch (_) {}
                    current = Object.getPrototypeOf(current);
                }
            };

            if (nav) {
                stripProperty(nav, 'webdriver');

                try {
                    const cleanUserAgent = String(nav.userAgent || '').replace(/HeadlessChrome/g, 'Chrome');
                    if (cleanUserAgent) {
                        Object.defineProperty(nav, 'userAgent', {
                            get: () => cleanUserAgent,
                            configurable: true,
                        });
                    }
                } catch (_) {}

                try {
                    if (!nav.languages || nav.languages.length === 0) {
                        Object.defineProperty(nav, 'languages', {
                            get: () => Object.freeze(['en-US', 'en']),
                            configurable: true,
                        });
                    }
                } catch (_) {}

                try {
                    if (!nav.language) {
                        Object.defineProperty(nav, 'language', {
                            get: () => 'en-US',
                            configurable: true,
                        });
                    }
                } catch (_) {}

                try {
                    if (!nav.connection || nav.connection.rtt === 0) {
                        Object.defineProperty(nav, 'connection', {
                            get: () => ({
                                effectiveType: '4g',
                                rtt: 50,
                                downlink: 10,
                                saveData: false,
                                onchange: null,
                            }),
                            configurable: true,
                        });
                    }
                } catch (_) {}
            }

            if (!workerRoot.chrome) workerRoot.chrome = {};
            if (!workerRoot.chrome.runtime) {
                workerRoot.chrome.runtime = {
                    id: undefined,
                    connect: function() { return {}; },
                    sendMessage: function() {},
                    onMessage: { addListener: function() {}, removeListener: function() {} },
                    onConnect: { addListener: function() {}, removeListener: function() {} },
                };
            }
        })();`;

        const buildWrappedUrl = (scriptUrl, workerType) => {
            const source = String(scriptUrl);
            const loader = workerType === 'module'
                ? `\nimport(${JSON.stringify(source)});`
                : `\nimportScripts(${JSON.stringify(source)});`;
            const blob = new Blob([workerBootstrap + loader], {
                type: 'text/javascript',
            });
            return URL.createObjectURL(blob);
        };

        const detectWorkerType = (args) => {
            const second = args[1];
            if (second && typeof second === 'object' && second.type === 'module') {
                return 'module';
            }
            return 'classic';
        };

        const WrappedCtor = function(...args) {
            if (args.length === 0) {
                return Reflect.construct(NativeCtor, args, new.target || WrappedCtor);
            }

            const nextArgs = args.slice();
            const wrappedUrl = buildWrappedUrl(nextArgs[0], detectWorkerType(args));
            nextArgs[0] = wrappedUrl;

            try {
                const instance = Reflect.construct(NativeCtor, nextArgs, new.target || WrappedCtor);
                setTimeout(() => URL.revokeObjectURL(wrappedUrl), 0);
                return instance;
            } catch (_) {
                URL.revokeObjectURL(wrappedUrl);
                return Reflect.construct(NativeCtor, args, new.target || WrappedCtor);
            }
        };

        Object.setPrototypeOf(WrappedCtor, NativeCtor);
        WrappedCtor.prototype = NativeCtor.prototype;
        Object.defineProperty(WrappedCtor, '__rubWorkerBridge', {
            value: true,
            configurable: false,
        });
        Object.defineProperty(WrappedCtor, 'name', {
            value: NativeCtor.name,
            configurable: true,
        });
        if (typeof markNative === 'function') {
            markNative(WrappedCtor, NativeCtor);
        } else {
            Object.defineProperty(WrappedCtor, 'toString', {
                value: () => `function ${NativeCtor.name}() { [native code] }`,
                configurable: true,
            });
        }

        root[globalName] = WrappedCtor;
    };

    installWorkerBridge('Worker');
    installWorkerBridge('SharedWorker');
})();
