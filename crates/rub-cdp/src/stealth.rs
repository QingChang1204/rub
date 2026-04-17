//! L1 stealth patches — JS scripts injected via `evaluate_on_new_document`
//! to normalize the browser environment and reduce automation fingerprints.
//!
//! Each patch is a self-contained IIFE that targets one detection vector.
//! Patches are concatenated by [`combined_stealth_script`] and injected once
//! per new page/frame.

mod patches;
#[cfg(test)]
mod tests;

use crate::environment_profile::EnvironmentProfile;
use crate::fingerprint_profile::FingerprintProfile;
use patches::{
    SP_001_WEBDRIVER, SP_002_CHROME_RUNTIME, SP_003_NAVIGATOR_PLUGINS, SP_004_NAVIGATOR_LANGUAGES,
    SP_005_PERMISSIONS_QUERY, SP_006_WINDOW_CHROME, SP_007_NAVIGATOR_CONNECTION,
    SP_008_WEBGL_DEBUG, SP_009_WORKER_CONTEXT_BRIDGE, SP_010_CANVAS_FINGERPRINT,
    SP_011_AUDIO_FINGERPRINT, SP_012_ENVIRONMENT_PROFILE, STEALTH_SHARED_HELPERS,
};

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
