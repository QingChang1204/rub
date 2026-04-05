//! L1 stealth patches — JS scripts injected via `evaluate_on_new_document`
//! to normalize the browser environment and reduce automation fingerprints.
//!
//! Each patch is a self-contained IIFE that targets one detection vector.
//! Patches are concatenated by [`combined_stealth_script`] and injected once
//! per new page/frame.

use crate::environment_profile::EnvironmentProfile;
use crate::fingerprint_profile::FingerprintProfile;

/// All available stealth patches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StealthPatch {
    /// SP-001: `navigator.webdriver` → `undefined`
    WebdriverUndefined,
    /// SP-002: `chrome.runtime` mock object
    ChromeRuntimeMock,
    /// SP-003: `navigator.plugins` with realistic entries
    NavigatorPlugins,
    /// SP-004: `navigator.languages` with system locale
    NavigatorLanguages,
    /// SP-005: `Permissions.prototype.query` normalization
    PermissionsQuery,
    /// SP-006: `window.chrome` object scaffold
    WindowChrome,
    /// SP-007: `navigator.connection.rtt` non-zero
    NavigatorConnection,
    /// SP-008: WebGL debug renderer info
    WebGLDebugInfo,
    /// SP-009: dedicated/shared worker identity bridge
    WorkerContextBridge,
    /// SP-010: deterministic canvas fingerprint perturbation
    CanvasFingerprint,
    /// SP-011: deterministic audio fingerprint perturbation
    AudioFingerprint,
    /// SP-012: desktop environment consistency profile
    EnvironmentProfile,
}

impl StealthPatch {
    /// Human-readable name for `doctor` output.
    pub fn name(self) -> &'static str {
        match self {
            Self::WebdriverUndefined => "webdriver_undefined",
            Self::ChromeRuntimeMock => "chrome_runtime_mock",
            Self::NavigatorPlugins => "navigator_plugins",
            Self::NavigatorLanguages => "navigator_languages",
            Self::PermissionsQuery => "permissions_query",
            Self::WindowChrome => "window_chrome",
            Self::NavigatorConnection => "navigator_connection",
            Self::WebGLDebugInfo => "webgl_debug_info",
            Self::WorkerContextBridge => "worker_context_bridge",
            Self::CanvasFingerprint => "canvas_fingerprint",
            Self::AudioFingerprint => "audio_fingerprint",
            Self::EnvironmentProfile => "environment_profile",
        }
    }

    /// The JS source for this patch (self-contained IIFE).
    pub fn script(self) -> &'static str {
        match self {
            Self::WebdriverUndefined => SP_001_WEBDRIVER,
            Self::ChromeRuntimeMock => SP_002_CHROME_RUNTIME,
            Self::NavigatorPlugins => SP_003_NAVIGATOR_PLUGINS,
            Self::NavigatorLanguages => SP_004_NAVIGATOR_LANGUAGES,
            Self::PermissionsQuery => SP_005_PERMISSIONS_QUERY,
            Self::WindowChrome => SP_006_WINDOW_CHROME,
            Self::NavigatorConnection => SP_007_NAVIGATOR_CONNECTION,
            Self::WebGLDebugInfo => SP_008_WEBGL_DEBUG,
            Self::WorkerContextBridge => SP_009_WORKER_CONTEXT_BRIDGE,
            Self::CanvasFingerprint => SP_010_CANVAS_FINGERPRINT,
            Self::AudioFingerprint => SP_011_AUDIO_FINGERPRINT,
            Self::EnvironmentProfile => SP_012_ENVIRONMENT_PROFILE,
        }
    }

    /// All patches in recommended application order.
    pub fn all() -> &'static [StealthPatch] {
        &[
            Self::WebdriverUndefined,
            Self::ChromeRuntimeMock,
            Self::NavigatorPlugins,
            Self::NavigatorLanguages,
            Self::PermissionsQuery,
            Self::WindowChrome,
            Self::NavigatorConnection,
            Self::WebGLDebugInfo,
            Self::WorkerContextBridge,
            Self::CanvasFingerprint,
            Self::AudioFingerprint,
            Self::EnvironmentProfile,
        ]
    }
}

/// Runtime stealth configuration threaded from CLI to daemon.
#[derive(Debug, Clone)]
pub struct StealthConfig {
    pub enabled: bool,
    pub fingerprint_profile: FingerprintProfile,
    pub environment_profile: Option<EnvironmentProfile>,
}

impl Default for StealthConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            fingerprint_profile: FingerprintProfile::for_seed(0),
            environment_profile: None,
        }
    }
}

/// Build the combined stealth script from all enabled patches.
/// Returns `None` if stealth is disabled.
pub fn combined_stealth_script(config: &StealthConfig) -> Option<String> {
    if !config.enabled {
        return None;
    }

    let patches = StealthPatch::all();
    let mut script = String::with_capacity(8192);
    script.push_str("// rub stealth patches\n");
    script.push_str(STEALTH_SHARED_HELPERS);
    script.push('\n');
    for patch in patches {
        script.push_str(render_patch_script(*patch, config).as_str());
        script.push('\n');
    }
    Some(script)
}

/// Returns the list of applied patch names (for `doctor` output).
pub fn applied_patch_names(config: &StealthConfig) -> Vec<String> {
    if !config.enabled {
        return vec![];
    }
    let mut names = Vec::new();
    for patch in StealthPatch::all() {
        if matches!(patch, StealthPatch::EnvironmentProfile) && config.environment_profile.is_none()
        {
            continue;
        }
        names.push(patch.name().to_string());
    }
    names
}

fn render_patch_script(patch: StealthPatch, config: &StealthConfig) -> String {
    match patch {
        StealthPatch::WebGLDebugInfo => render_dynamic_patch(
            patch.script(),
            &[
                (
                    "__RUB_WEBGL_VENDOR__",
                    js_string_literal(&config.fingerprint_profile.webgl_vendor),
                ),
                (
                    "__RUB_WEBGL_RENDERER__",
                    js_string_literal(&config.fingerprint_profile.webgl_renderer),
                ),
            ],
        ),
        StealthPatch::CanvasFingerprint => render_dynamic_patch(
            patch.script(),
            &[
                (
                    "__RUB_CANVAS_RED_OFFSET__",
                    config
                        .fingerprint_profile
                        .canvas_noise
                        .red_offset
                        .to_string(),
                ),
                (
                    "__RUB_CANVAS_GREEN_OFFSET__",
                    config
                        .fingerprint_profile
                        .canvas_noise
                        .green_offset
                        .to_string(),
                ),
                (
                    "__RUB_CANVAS_BLUE_OFFSET__",
                    config
                        .fingerprint_profile
                        .canvas_noise
                        .blue_offset
                        .to_string(),
                ),
            ],
        ),
        StealthPatch::AudioFingerprint => render_dynamic_patch(
            patch.script(),
            &[
                (
                    "__RUB_AUDIO_FIRST_INDEX__",
                    config
                        .fingerprint_profile
                        .audio_noise
                        .first_index
                        .to_string(),
                ),
                (
                    "__RUB_AUDIO_SECOND_INDEX__",
                    config
                        .fingerprint_profile
                        .audio_noise
                        .second_index
                        .to_string(),
                ),
                (
                    "__RUB_AUDIO_DELTA__",
                    format!("{:.8}", config.fingerprint_profile.audio_noise.delta),
                ),
            ],
        ),
        StealthPatch::EnvironmentProfile => {
            let Some(environment_profile) = config.environment_profile else {
                return String::new();
            };
            render_dynamic_patch(
                patch.script(),
                &[
                    (
                        "__RUB_SCREEN_WIDTH__",
                        environment_profile.screen_width.to_string(),
                    ),
                    (
                        "__RUB_SCREEN_HEIGHT__",
                        environment_profile.screen_height.to_string(),
                    ),
                    (
                        "__RUB_OUTER_WIDTH__",
                        environment_profile.outer_width.to_string(),
                    ),
                    (
                        "__RUB_OUTER_HEIGHT__",
                        environment_profile.outer_height.to_string(),
                    ),
                    (
                        "__RUB_MAX_TOUCH_POINTS__",
                        environment_profile.max_touch_points.to_string(),
                    ),
                    (
                        "__RUB_TOUCH_ENABLED__",
                        if environment_profile.touch_enabled {
                            "true".to_string()
                        } else {
                            "false".to_string()
                        },
                    ),
                ],
            )
        }
        _ => patch.script().to_string(),
    }
}

fn render_dynamic_patch(template: &str, replacements: &[(&str, String)]) -> String {
    let mut rendered = template.to_string();
    for (needle, value) in replacements {
        rendered = rendered.replace(needle, value);
    }
    rendered
}

fn js_string_literal(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

const STEALTH_SHARED_HELPERS: &str = r#"
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
"#;

// ── SP-001: navigator.webdriver → undefined ─────────────────────────

const SP_001_WEBDRIVER: &str = r#"
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
"#;

// ── SP-002: chrome.runtime mock ─────────────────────────────────────

const SP_002_CHROME_RUNTIME: &str = r#"
(() => {
    if (!window.chrome) window.chrome = {};
    if (!window.chrome.runtime) {
        window.chrome.runtime = {
            id: undefined,
            connect: function() { return {}; },
            sendMessage: function() {},
            onMessage: { addListener: function() {}, removeListener: function() {} },
            onConnect: { addListener: function() {}, removeListener: function() {} },
        };
    }
})();
"#;

// ── SP-003: navigator.plugins ───────────────────────────────────────

const SP_003_NAVIGATOR_PLUGINS: &str = r#"
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
"#;

// ── SP-004: navigator.languages ─────────────────────────────────────

const SP_004_NAVIGATOR_LANGUAGES: &str = r#"
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
"#;

// ── SP-005: Permissions.prototype.query ──────────────────────────────

const SP_005_PERMISSIONS_QUERY: &str = r#"
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
"#;

// ── SP-006: window.chrome object ────────────────────────────────────

const SP_006_WINDOW_CHROME: &str = r#"
(() => {
    if (!window.chrome) window.chrome = {};
    if (!window.chrome.app) {
        window.chrome.app = {
            isInstalled: false,
            InstallState: { DISABLED: 'disabled', INSTALLED: 'installed', NOT_INSTALLED: 'not_installed' },
            RunningState: { CANNOT_RUN: 'cannot_run', READY_TO_RUN: 'ready_to_run', RUNNING: 'running' },
            getDetails: function() { return null; },
            getIsInstalled: function() { return false; },
        };
    }
    if (!window.chrome.csi) {
        window.chrome.csi = function() { return {}; };
    }
    if (!window.chrome.loadTimes) {
        window.chrome.loadTimes = function() { return {}; };
    }
})();
"#;

// ── SP-007: navigator.connection ────────────────────────────────────

const SP_007_NAVIGATOR_CONNECTION: &str = r#"
(() => {
    if (navigator.connection && navigator.connection.rtt !== 0) return;
    try {
        Object.defineProperty(navigator, 'connection', {
            get: () => ({
                effectiveType: '4g',
                rtt: 50,
                downlink: 10,
                saveData: false,
                onchange: null,
            }),
            configurable: true,
        });
    } catch (_) {}
})();
"#;

// ── SP-008: WebGL debug renderer ────────────────────────────────────

const SP_008_WEBGL_DEBUG: &str = r#"
(() => {
    const webglVendor = __RUB_WEBGL_VENDOR__;
    const webglRenderer = __RUB_WEBGL_RENDERER__;
    const markNative = globalThis[Symbol.for('rub.stealth.mark_native')];
    const install = (Ctor) => {
        if (typeof Ctor === 'undefined' || !Ctor || Ctor.prototype.__rubWebglDebugInfo === true) return;
        const nativeGetParameter = Ctor.prototype.getParameter;
        const wrappedGetParameter = function getParameter(param) {
            if (param === 0x9245) return webglVendor;
            if (param === 0x9246) return webglRenderer;
            return nativeGetParameter.call(this, param);
        };
        Ctor.prototype.getParameter =
            typeof markNative === 'function'
                ? markNative(wrappedGetParameter, nativeGetParameter)
                : wrappedGetParameter;
        Object.defineProperty(Ctor.prototype, '__rubWebglDebugInfo', {
            value: true,
            configurable: true,
        });
    };
    install(globalThis.WebGLRenderingContext);
    install(globalThis.WebGL2RenderingContext);
})();
"#;

// ── SP-009: dedicated/shared worker bridge ─────────────────────────

const SP_009_WORKER_CONTEXT_BRIDGE: &str = r#"
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
"#;

// ── SP-010: canvas fingerprint perturbation ─────────────────────────

const SP_010_CANVAS_FINGERPRINT: &str = r#"
(() => {
    const redOffset = __RUB_CANVAS_RED_OFFSET__;
    const greenOffset = __RUB_CANVAS_GREEN_OFFSET__;
    const blueOffset = __RUB_CANVAS_BLUE_OFFSET__;
    const markNative = globalThis[Symbol.for('rub.stealth.mark_native')];
    const clamp = (value) => Math.max(0, Math.min(255, value));
    const applyNoise = (data) => {
        if (!data || typeof data.length !== 'number' || data.length < 4) return data;
        data[0] = clamp(data[0] + redOffset);
        data[1] = clamp(data[1] + greenOffset);
        data[2] = clamp(data[2] + blueOffset);
        return data;
    };
    const makeCanvasClone = (source) => {
        const width = Number(source && source.width) || 0;
        const height = Number(source && source.height) || 0;
        if (typeof document !== 'undefined' && typeof document.createElement === 'function') {
            const clone = document.createElement('canvas');
            clone.width = width;
            clone.height = height;
            return clone;
        }
        if (typeof OffscreenCanvas !== 'undefined') {
            return new OffscreenCanvas(width, height);
        }
        return null;
    };
    const installContextPatch = (Ctor) => {
        if (typeof Ctor === 'undefined' || !Ctor || Ctor.prototype.__rubCanvasFingerprint === true) return;
        const nativeGetImageData = Ctor.prototype.getImageData;
        const wrappedGetImageData = function getImageData(...args) {
            const result = nativeGetImageData.apply(this, args);
            try {
                applyNoise(result && result.data);
            } catch (_) {}
            return result;
        };
        Ctor.prototype.getImageData =
            typeof markNative === 'function'
                ? markNative(wrappedGetImageData, nativeGetImageData)
                : wrappedGetImageData;
        Object.defineProperty(Ctor.prototype.getImageData, '__rubNativeGetImageData', {
            value: nativeGetImageData,
            configurable: true,
        });
        Object.defineProperty(Ctor.prototype, '__rubCanvasFingerprint', {
            value: true,
            configurable: true,
        });
    };
    const readNativeImageData = (ctx, width, height) => {
        if (!ctx || typeof ctx.getImageData !== 'function') return null;
        const nativeGetImageData = ctx.getImageData.__rubNativeGetImageData;
        if (typeof nativeGetImageData === 'function') {
            return nativeGetImageData.call(ctx, 0, 0, width, height);
        }
        return ctx.getImageData(0, 0, width, height);
    };
    const cloneWithNoise = (source) => {
        const clone = makeCanvasClone(source);
        if (!clone || typeof clone.getContext !== 'function') return null;
        const ctx = clone.getContext('2d');
        if (!ctx) return null;
        ctx.drawImage(source, 0, 0);
        const width = Number(clone.width) || 0;
        const height = Number(clone.height) || 0;
        if (width > 0 && height > 0) {
            const imageData = readNativeImageData(ctx, width, height);
            if (imageData) {
                applyNoise(imageData.data);
                ctx.putImageData(imageData, 0, 0);
            }
        }
        return clone;
    };
    const installCanvasMethodPatch = (Ctor, method, marker) => {
        if (typeof Ctor === 'undefined' || !Ctor || typeof Ctor.prototype[method] !== 'function') return;
        if (Ctor.prototype[marker] === true) return;
        const nativeMethod = Ctor.prototype[method];
        const wrappedMethod = function(...args) {
            try {
                const clone = cloneWithNoise(this);
                if (clone && typeof nativeMethod === 'function') {
                    return nativeMethod.apply(clone, args);
                }
            } catch (_) {}
            return nativeMethod.apply(this, args);
        };
        Ctor.prototype[method] =
            typeof markNative === 'function'
                ? markNative(wrappedMethod, nativeMethod)
                : wrappedMethod;
        Object.defineProperty(Ctor.prototype, marker, {
            value: true,
            configurable: true,
        });
    };

    installContextPatch(globalThis.CanvasRenderingContext2D);
    installContextPatch(globalThis.OffscreenCanvasRenderingContext2D);
    installCanvasMethodPatch(globalThis.HTMLCanvasElement, 'toDataURL', '__rubCanvasToDataURL');
    installCanvasMethodPatch(globalThis.HTMLCanvasElement, 'toBlob', '__rubCanvasToBlob');
    installCanvasMethodPatch(globalThis.OffscreenCanvas, 'convertToBlob', '__rubCanvasConvertToBlob');
})();
"#;

// ── SP-011: audio fingerprint perturbation ──────────────────────────

const SP_011_AUDIO_FINGERPRINT: &str = r#"
(() => {
    const firstIndex = __RUB_AUDIO_FIRST_INDEX__;
    const secondIndex = __RUB_AUDIO_SECOND_INDEX__;
    const delta = __RUB_AUDIO_DELTA__;
    const markNative = globalThis[Symbol.for('rub.stealth.mark_native')];
    const touchedChannels = new WeakMap();
    const install = () => {
        if (typeof AudioBuffer === 'undefined' || !AudioBuffer.prototype || AudioBuffer.prototype.__rubAudioFingerprint === true) return;
        const nativeGetChannelData = AudioBuffer.prototype.getChannelData;
        const wrappedGetChannelData = function getChannelData(channel) {
            const data = nativeGetChannelData.call(this, channel);
            try {
                let seen = touchedChannels.get(this);
                if (!seen) {
                    seen = new Set();
                    touchedChannels.set(this, seen);
                }
                const key = `${channel}:${data.length}`;
                if (!seen.has(key)) {
                    if (firstIndex < data.length) data[firstIndex] += delta;
                    if (secondIndex < data.length) data[secondIndex] += delta;
                    seen.add(key);
                }
            } catch (_) {}
            return data;
        };
        AudioBuffer.prototype.getChannelData =
            typeof markNative === 'function'
                ? markNative(wrappedGetChannelData, nativeGetChannelData)
                : wrappedGetChannelData;
        if (typeof AudioBuffer.prototype.copyFromChannel === 'function') {
            const nativeCopyFromChannel = AudioBuffer.prototype.copyFromChannel;
            const wrappedCopyFromChannel = function copyFromChannel(destination, channel, startInChannel) {
                try {
                    this.getChannelData(channel);
                } catch (_) {}
                return nativeCopyFromChannel.call(this, destination, channel, startInChannel);
            };
            AudioBuffer.prototype.copyFromChannel =
                typeof markNative === 'function'
                    ? markNative(wrappedCopyFromChannel, nativeCopyFromChannel)
                    : wrappedCopyFromChannel;
        }
        Object.defineProperty(AudioBuffer.prototype, '__rubAudioFingerprint', {
            value: true,
            configurable: true,
        });
    };
    install();
})();
"#;

// ── SP-012: desktop environment consistency ─────────────────────────

const SP_012_ENVIRONMENT_PROFILE: &str = r#"
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
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment_profile::EnvironmentProfile;

    #[test]
    fn combined_script_contains_all_patches_when_enabled() {
        let config = StealthConfig {
            environment_profile: Some(EnvironmentProfile::for_seed(0)),
            ..StealthConfig::default()
        };
        let script = combined_stealth_script(&config).unwrap();
        assert!(script.contains("rub.stealth.mark_native"));
        assert!(script.contains("stripProperty"));
        assert!(script.contains("'webdriver'"));
        assert!(script.contains("chrome.runtime"));
        assert!(script.contains("navigator.plugins"));
        assert!(script.contains("navigator.languages"));
        assert!(script.contains("Permissions.prototype.query"));
        assert!(script.contains("window.chrome"));
        assert!(script.contains("navigator.connection"));
        assert!(script.contains("WebGLRenderingContext"));
        assert!(script.contains("installWorkerBridge"));
        assert!(script.contains("CanvasRenderingContext2D"));
        assert!(script.contains("AudioBuffer"));
        assert!(script.contains("maxTouchPoints"));
    }

    #[test]
    fn combined_script_none_when_disabled() {
        let config = StealthConfig {
            enabled: false,
            ..StealthConfig::default()
        };
        assert!(combined_stealth_script(&config).is_none());
    }

    #[test]
    fn applied_patch_names_returns_all_when_enabled() {
        let config = StealthConfig {
            environment_profile: Some(EnvironmentProfile::for_seed(0)),
            ..StealthConfig::default()
        };
        let names = applied_patch_names(&config);
        assert_eq!(names.len(), 12);
        assert!(names.contains(&"webdriver_undefined".to_string()));
        assert!(names.contains(&"worker_context_bridge".to_string()));
        assert!(names.contains(&"canvas_fingerprint".to_string()));
        assert!(names.contains(&"audio_fingerprint".to_string()));
    }

    #[test]
    fn applied_patch_names_empty_when_disabled() {
        let config = StealthConfig {
            enabled: false,
            ..StealthConfig::default()
        };
        assert!(applied_patch_names(&config).is_empty());
    }

    #[test]
    fn webdriver_patch_strips_descriptor_instead_of_redefining_getter() {
        let script = StealthPatch::WebdriverUndefined.script();
        assert!(script.contains("stripProperty"));
        assert!(script.contains("delete current[prop]"));
        assert!(!script.contains("get: () => undefined"));
    }

    #[test]
    fn worker_bridge_patch_wraps_dedicated_and_shared_workers() {
        let script = StealthPatch::WorkerContextBridge.script();
        assert!(script.contains("installWorkerBridge('Worker')"));
        assert!(script.contains("installWorkerBridge('SharedWorker')"));
        assert!(script.contains("replace(/HeadlessChrome/g, 'Chrome')"));
        assert!(script.contains("importScripts"));
    }

    #[test]
    fn permissions_patch_marks_wrapped_query_as_native_like() {
        let script = StealthPatch::PermissionsQuery.script();
        assert!(script.contains("rub.stealth.mark_native"));
        assert!(script.contains("markNative(wrappedQuery, originalQuery)"));
    }

    #[test]
    fn environment_profile_patch_is_omitted_without_profile() {
        let config = StealthConfig::default();
        let script = combined_stealth_script(&config).unwrap();

        assert!(!script.contains("expectedScreenWidth"));
        assert!(!applied_patch_names(&config).contains(&"environment_profile".to_string()));
    }
}
