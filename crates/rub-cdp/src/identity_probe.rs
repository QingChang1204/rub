use std::sync::Arc;

use chromiumoxide::Page;
use rub_core::error::RubError;
use rub_core::model::{IdentityProbeStatus, IdentitySelfProbeInfo};
use serde::Deserialize;

use crate::identity_policy::IdentityPolicy;

#[derive(Debug, Default, Deserialize)]
struct MainWorldProbe {
    #[serde(default)]
    webdriver_missing: bool,
    #[serde(default)]
    chrome_runtime_present: bool,
    #[serde(default)]
    plugins_present: bool,
    #[serde(default)]
    languages_present: bool,
    #[serde(default)]
    headless_user_agent: bool,
}

#[derive(Debug, Default, Deserialize)]
struct FingerprintProbe {
    #[serde(default)]
    webgl_profile_match: bool,
    #[serde(default)]
    canvas_stable_noise: bool,
    #[serde(default)]
    audio_stable_noise: bool,
}

#[derive(Debug, Default, Deserialize)]
struct PermissionsProbe {
    #[serde(default)]
    supported: bool,
    #[serde(default)]
    native_like: bool,
    #[serde(default)]
    leaked_patch_source: bool,
    #[serde(default)]
    query_name_ok: bool,
    #[serde(default)]
    query_length_ok: bool,
}

#[derive(Debug, Default, Deserialize)]
struct EnvironmentProbe {
    #[serde(default)]
    viewport_consistent: bool,
    #[serde(default)]
    touch_consistent: bool,
    #[serde(default)]
    window_metrics_consistent: bool,
}

#[derive(Debug, Default, Deserialize)]
struct AsyncProbe {
    #[serde(default)]
    status: String,
    #[serde(default)]
    webdriver_missing: bool,
    #[serde(default)]
    chrome_runtime_present: bool,
    #[serde(default)]
    headless_user_agent: bool,
}

const MAIN_WORLD_PROBE_JS: &str = r#"
(() => JSON.stringify({
    webdriver_missing: !('webdriver' in navigator),
    chrome_runtime_present: typeof (window.chrome && window.chrome.runtime) === 'object',
    plugins_present: !!(navigator.plugins && navigator.plugins.length > 0),
    languages_present: !!(navigator.languages && navigator.languages.length > 0),
    headless_user_agent: /HeadlessChrome/i.test(String(navigator.userAgent || ''))
}))()
"#;

const PERMISSIONS_PROBE_JS: &str = r#"
(() => JSON.stringify((() => {
    try {
        if (typeof Permissions === 'undefined' ||
            !Permissions.prototype ||
            typeof Permissions.prototype.query !== 'function') {
            return { supported: false };
        }
        const query = Permissions.prototype.query;
        const source = String(Function.prototype.toString.call(query));
        return {
            supported: true,
            native_like: /\[native code\]/.test(source),
            leaked_patch_source: /notifications|Promise\.resolve|originalQuery|wrappedQuery/.test(source),
            query_name_ok: String(query.name || '') === 'query',
            query_length_ok: Number(query.length || 0) === 1
        };
    } catch (_) {
        return { supported: false };
    }
})()))()
"#;

const IFRAME_PROBE_JS: &str = r#"
(() => new Promise((resolve) => {
    try {
        const iframe = document.createElement('iframe');
        iframe.setAttribute('aria-hidden', 'true');
        iframe.style.cssText = 'position:absolute;left:-9999px;top:-9999px;width:1px;height:1px;border:0;visibility:hidden;';
        iframe.srcdoc = '<!DOCTYPE html><html><body></body></html>';
        const cleanup = (result) => {
            try { iframe.remove(); } catch (_) {}
            resolve(JSON.stringify(result));
        };
        iframe.onload = () => {
            try {
                const win = iframe.contentWindow;
                const nav = win && win.navigator;
                cleanup({
                    status: 'ok',
                    webdriver_missing: !!nav && !('webdriver' in nav),
                    chrome_runtime_present: !!(win && win.chrome && win.chrome.runtime),
                    headless_user_agent: /HeadlessChrome/i.test(String((nav && nav.userAgent) || ''))
                });
            } catch (error) {
                cleanup({ status: 'error' });
            }
        };
        document.documentElement.appendChild(iframe);
        setTimeout(() => cleanup({ status: 'timeout' }), 750);
    } catch (error) {
        resolve(JSON.stringify({ status: 'error' }));
    }
}))()
"#;

const WORKER_PROBE_JS: &str = r#"
(() => new Promise((resolve) => {
    try {
        if (typeof Worker !== 'function') {
            resolve(JSON.stringify({ status: 'unsupported' }));
            return;
        }
        const source = `
            self.postMessage({
                status: 'ok',
                webdriver_missing: !('webdriver' in navigator),
                chrome_runtime_present: typeof (self.chrome && self.chrome.runtime) === 'object',
                headless_user_agent: /HeadlessChrome/i.test(String(navigator.userAgent || ''))
            });
        `;
        const url = URL.createObjectURL(new Blob([source], { type: 'text/javascript' }));
        const worker = new Worker(url);
        const cleanup = (result) => {
            try { worker.terminate(); } catch (_) {}
            try { URL.revokeObjectURL(url); } catch (_) {}
            resolve(JSON.stringify(result));
        };
        const timer = setTimeout(() => cleanup({ status: 'timeout' }), 750);
        worker.onmessage = (event) => {
            clearTimeout(timer);
            cleanup(event.data || { status: 'error' });
        };
        worker.onerror = () => {
            clearTimeout(timer);
            cleanup({ status: 'error' });
        };
    } catch (error) {
        resolve(JSON.stringify({ status: 'error' }));
    }
}))()
"#;

pub async fn run_identity_self_probe(
    page: &Arc<Page>,
    policy: &IdentityPolicy,
) -> IdentitySelfProbeInfo {
    let main_world = probe_main_world(page).await;
    let iframe_context = probe_async_surface(page, IFRAME_PROBE_JS).await;
    let worker_context = if policy.worker_coverage_supported() {
        probe_async_surface(page, WORKER_PROBE_JS).await
    } else {
        IdentityProbeStatus::Unknown
    };
    let ua_consistency = probe_ua_consistency(page, policy).await;
    let fingerprint_surfaces = probe_fingerprint_surfaces(page, policy).await;
    let environment_surfaces = probe_environment_surfaces(page, policy).await;

    IdentitySelfProbeInfo {
        page_main_world: Some(main_world),
        iframe_context: Some(iframe_context),
        worker_context: Some(worker_context),
        ua_consistency: Some(ua_consistency),
        webgl_surface: Some(fingerprint_surfaces.webgl_surface),
        canvas_surface: Some(fingerprint_surfaces.canvas_surface),
        audio_surface: Some(fingerprint_surfaces.audio_surface),
        permissions_surface: Some(fingerprint_surfaces.permissions_surface),
        viewport_surface: Some(environment_surfaces.viewport_surface),
        touch_surface: Some(environment_surfaces.touch_surface),
        window_metrics_surface: Some(environment_surfaces.window_metrics_surface),
        unsupported_surfaces: vec!["service_worker".to_string()],
    }
}

async fn probe_main_world(page: &Arc<Page>) -> IdentityProbeStatus {
    match evaluate_json::<MainWorldProbe>(page, MAIN_WORLD_PROBE_JS).await {
        Ok(probe) => classify_surface(
            probe.webdriver_missing
                && probe.chrome_runtime_present
                && probe.plugins_present
                && probe.languages_present
                && !probe.headless_user_agent,
        ),
        Err(_) => IdentityProbeStatus::Unknown,
    }
}

async fn probe_async_surface(page: &Arc<Page>, script: &str) -> IdentityProbeStatus {
    match evaluate_json::<AsyncProbe>(page, script).await {
        Ok(probe) => match probe.status.as_str() {
            "ok" => classify_surface(
                probe.webdriver_missing
                    && probe.chrome_runtime_present
                    && !probe.headless_user_agent,
            ),
            "unsupported" | "error" | "timeout" => IdentityProbeStatus::Unknown,
            _ => IdentityProbeStatus::Unknown,
        },
        Err(_) => IdentityProbeStatus::Unknown,
    }
}

async fn probe_ua_consistency(page: &Arc<Page>, policy: &IdentityPolicy) -> IdentityProbeStatus {
    if !policy.user_agent_override_expected() {
        return IdentityProbeStatus::Unknown;
    }

    let script = r#"
(() => JSON.stringify({
    webdriver_missing: !('webdriver' in navigator),
    chrome_runtime_present: !!(window.chrome && window.chrome.runtime),
    headless_user_agent: /HeadlessChrome/i.test(String(navigator.userAgent || '')),
    user_agent_own_descriptor_absent: Object.getOwnPropertyDescriptor(navigator, 'userAgent') === undefined,
    user_agent_getter_native_like: (() => {
        const descriptor = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(navigator), 'userAgent');
        if (!descriptor || typeof descriptor.get !== 'function') return true;
        return /\[native code\]/.test(String(Function.prototype.toString.call(descriptor.get)));
    })(),
    brands_headless: !!(navigator.userAgentData && Array.isArray(navigator.userAgentData.brands) &&
        navigator.userAgentData.brands.some((brand) => /HeadlessChrome/i.test(String(brand.brand || ''))))
}))()
"#;

    #[derive(Debug, Default, Deserialize)]
    struct UaProbe {
        #[serde(default)]
        webdriver_missing: bool,
        #[serde(default)]
        chrome_runtime_present: bool,
        #[serde(default)]
        headless_user_agent: bool,
        #[serde(default)]
        user_agent_own_descriptor_absent: bool,
        #[serde(default)]
        user_agent_getter_native_like: bool,
        #[serde(default)]
        brands_headless: bool,
    }

    match evaluate_json::<UaProbe>(page, script).await {
        Ok(probe) => classify_surface(
            probe.webdriver_missing
                && probe.chrome_runtime_present
                && !probe.headless_user_agent
                && probe.user_agent_own_descriptor_absent
                && probe.user_agent_getter_native_like
                && !probe.brands_headless,
        ),
        Err(_) => IdentityProbeStatus::Unknown,
    }
}

async fn evaluate_json<T: serde::de::DeserializeOwned>(
    page: &Arc<Page>,
    script: &str,
) -> Result<T, RubError> {
    let json = page
        .evaluate(script)
        .await
        .map_err(|error| RubError::Internal(format!("Evaluate failed: {error}")))?
        .into_value::<String>()
        .map_err(|error| {
            RubError::Internal(format!("Evaluate did not return JSON string: {error}"))
        })?;

    serde_json::from_str(&json)
        .map_err(|error| RubError::Internal(format!("Identity probe parse failed: {error}")))
}

fn classify_surface(passed: bool) -> IdentityProbeStatus {
    if passed {
        IdentityProbeStatus::Passed
    } else {
        IdentityProbeStatus::Failed
    }
}

#[derive(Debug, Clone, Copy)]
struct FingerprintSurfaceStatuses {
    webgl_surface: IdentityProbeStatus,
    canvas_surface: IdentityProbeStatus,
    audio_surface: IdentityProbeStatus,
    permissions_surface: IdentityProbeStatus,
}

#[derive(Debug, Clone, Copy)]
struct EnvironmentSurfaceStatuses {
    viewport_surface: IdentityProbeStatus,
    touch_surface: IdentityProbeStatus,
    window_metrics_surface: IdentityProbeStatus,
}

async fn probe_fingerprint_surfaces(
    page: &Arc<Page>,
    policy: &IdentityPolicy,
) -> FingerprintSurfaceStatuses {
    let profile = policy.fingerprint_profile();
    let vendor =
        serde_json::to_string(&profile.webgl_vendor).unwrap_or_else(|_| "\"\"".to_string());
    let renderer =
        serde_json::to_string(&profile.webgl_renderer).unwrap_or_else(|_| "\"\"".to_string());
    let audio_delta = format!("{:.8}", profile.audio_noise.delta);
    let probe_script = format!(
        r#"
(() => {{
    const expectedVendor = {vendor};
    const expectedRenderer = {renderer};
    const expectedCanvas = [{canvas_r}, {canvas_g}, {canvas_b}, 255];
    const audioIndices = [{audio_first}, {audio_second}];
    const audioDelta = {audio_delta};
    const approxEqual = (left, right) => Math.abs(left - right) < 0.0000005;
    const probeWebGl = () => {{
        try {{
            const canvas = document.createElement('canvas');
            const gl = canvas.getContext('webgl') || canvas.getContext('experimental-webgl');
            if (!gl) return false;
            const ext = gl.getExtension('WEBGL_debug_renderer_info');
            if (!ext) return false;
            return gl.getParameter(ext.UNMASKED_VENDOR_WEBGL) === expectedVendor &&
                gl.getParameter(ext.UNMASKED_RENDERER_WEBGL) === expectedRenderer;
        }} catch (_) {{
            return false;
        }}
    }};
    const probeCanvas = () => {{
        try {{
            const canvas = document.createElement('canvas');
            canvas.width = 2;
            canvas.height = 1;
            const ctx = canvas.getContext('2d');
            if (!ctx) return false;
            ctx.fillStyle = 'rgba(10,20,30,1)';
            ctx.fillRect(0, 0, 2, 1);
            const firstPixels = Array.from(ctx.getImageData(0, 0, 1, 1).data);
            const secondPixels = Array.from(ctx.getImageData(0, 0, 1, 1).data);
            const firstDataUrl = canvas.toDataURL();
            const secondDataUrl = canvas.toDataURL();
            return firstPixels.every((value, index) => value === expectedCanvas[index]) &&
                secondPixels.every((value, index) => value === expectedCanvas[index]) &&
                firstDataUrl === secondDataUrl;
        }} catch (_) {{
            return false;
        }}
    }};
    const probeAudio = () => {{
        try {{
            if (typeof OfflineAudioContext !== 'function') return false;
            const context = new OfflineAudioContext(1, 32, 44100);
            const buffer = context.createBuffer(1, 32, 44100);
            const first = Array.from(buffer.getChannelData(0));
            const second = Array.from(buffer.getChannelData(0));
            return approxEqual(first[audioIndices[0]], audioDelta) &&
                approxEqual(first[audioIndices[1]], audioDelta) &&
                approxEqual(second[audioIndices[0]], audioDelta) &&
                approxEqual(second[audioIndices[1]], audioDelta);
        }} catch (_) {{
            return false;
        }}
    }};
    return JSON.stringify({{
        webgl_profile_match: probeWebGl(),
        canvas_stable_noise: probeCanvas(),
        audio_stable_noise: probeAudio(),
    }});
}})()
"#,
        vendor = vendor,
        renderer = renderer,
        canvas_r = 10 + u16::from(profile.canvas_noise.red_offset),
        canvas_g = 20 + u16::from(profile.canvas_noise.green_offset),
        canvas_b = 30 + u16::from(profile.canvas_noise.blue_offset),
        audio_first = profile.audio_noise.first_index,
        audio_second = profile.audio_noise.second_index,
        audio_delta = audio_delta,
    );

    let probe = match evaluate_json::<FingerprintProbe>(page, probe_script.as_str()).await {
        Ok(probe) => probe,
        Err(_) => {
            return FingerprintSurfaceStatuses {
                webgl_surface: IdentityProbeStatus::Unknown,
                canvas_surface: IdentityProbeStatus::Unknown,
                audio_surface: IdentityProbeStatus::Unknown,
                permissions_surface: IdentityProbeStatus::Unknown,
            };
        }
    };

    let permissions_probe = probe_permissions_surface(page).await;

    FingerprintSurfaceStatuses {
        webgl_surface: classify_surface(probe.webgl_profile_match),
        canvas_surface: classify_surface(probe.canvas_stable_noise),
        audio_surface: classify_surface(probe.audio_stable_noise),
        permissions_surface: permissions_probe,
    }
}

async fn probe_permissions_surface(page: &Arc<Page>) -> IdentityProbeStatus {
    match evaluate_json::<PermissionsProbe>(page, PERMISSIONS_PROBE_JS).await {
        Ok(probe) => {
            if !probe.supported {
                IdentityProbeStatus::Unknown
            } else {
                classify_surface(
                    probe.native_like
                        && !probe.leaked_patch_source
                        && probe.query_name_ok
                        && probe.query_length_ok,
                )
            }
        }
        Err(_) => IdentityProbeStatus::Unknown,
    }
}

async fn probe_environment_surfaces(
    page: &Arc<Page>,
    policy: &IdentityPolicy,
) -> EnvironmentSurfaceStatuses {
    let Some(profile) = policy.environment_profile() else {
        return EnvironmentSurfaceStatuses {
            viewport_surface: IdentityProbeStatus::Unknown,
            touch_surface: IdentityProbeStatus::Unknown,
            window_metrics_surface: IdentityProbeStatus::Unknown,
        };
    };

    let expected_screen_width = profile.screen_width;
    let expected_screen_height = profile.screen_height;
    let expected_dpr = format!("{:.2}", profile.device_scale_factor);
    let expected_touch_points = profile.max_touch_points;
    let touch_enabled = if profile.touch_enabled {
        "true"
    } else {
        "false"
    };
    let probe_script = format!(
        r#"
(() => JSON.stringify({{
    viewport_consistent: (
        Number(screen.width) === {expected_screen_width} &&
        Number(screen.height) === {expected_screen_height} &&
        Math.abs(Number(window.devicePixelRatio || 0) - {expected_dpr}) < 0.01
    ),
    touch_consistent: (
        Number(navigator.maxTouchPoints || 0) === {expected_touch_points} &&
        (('ontouchstart' in window) === {touch_enabled}) &&
        Object.getOwnPropertyDescriptor(navigator, 'maxTouchPoints') === undefined &&
        (() => {{
            const descriptor = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(navigator), 'maxTouchPoints');
            if (!descriptor || typeof descriptor.get !== 'function') return true;
            return /\[native code\]/.test(String(Function.prototype.toString.call(descriptor.get)));
        }})()
    ),
    window_metrics_consistent: (
        Number(window.outerWidth || 0) >= Number(window.innerWidth || 0) &&
        Number(window.outerHeight || 0) >= Number(window.innerHeight || 0)
    ),
}}))()
"#,
        expected_screen_width = expected_screen_width,
        expected_screen_height = expected_screen_height,
        expected_dpr = expected_dpr,
        expected_touch_points = expected_touch_points,
        touch_enabled = touch_enabled,
    );

    let probe = match evaluate_json::<EnvironmentProbe>(page, probe_script.as_str()).await {
        Ok(probe) => probe,
        Err(_) => {
            return EnvironmentSurfaceStatuses {
                viewport_surface: IdentityProbeStatus::Unknown,
                touch_surface: IdentityProbeStatus::Unknown,
                window_metrics_surface: IdentityProbeStatus::Unknown,
            };
        }
    };

    EnvironmentSurfaceStatuses {
        viewport_surface: classify_surface(probe.viewport_consistent),
        touch_surface: classify_surface(probe.touch_consistent),
        window_metrics_surface: classify_surface(probe.window_metrics_consistent),
    }
}

#[cfg(test)]
mod tests {
    use super::classify_surface;
    use rub_core::model::IdentityProbeStatus;

    #[test]
    fn classify_surface_maps_true_and_false() {
        assert_eq!(classify_surface(true), IdentityProbeStatus::Passed);
        assert_eq!(classify_surface(false), IdentityProbeStatus::Failed);
    }
}
