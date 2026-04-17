use chromiumoxide::Page;
use chromiumoxide::cdp::js_protocol::runtime::ExecutionContextId;
use rub_core::error::RubError;
use rub_core::model::{
    AuthState, OverlayState, ReadinessInfo, ReadinessStatus, RouteStability, RuntimeStateSnapshot,
    StateInspectorInfo, StateInspectorStatus,
};
use serde::Deserialize;
use std::sync::Arc;
use tokio::time::Duration;
use tracing::warn;

type RuntimeStateSequenceAllocator = Arc<dyn Fn() -> u64 + Send + Sync>;
type RuntimeStateSnapshotCallback = Arc<dyn Fn(u64, RuntimeStateSnapshot) + Send + Sync>;
const RUNTIME_PROBE_TIMEOUT: Duration = Duration::from_millis(250);
const FRAME_SCOPED_PAGE_GLOBAL_COOKIE_REASON: &str =
    "page_global_cookie_authority_in_frame_snapshot";
const FRAME_SCOPED_PAGE_GLOBAL_COOKIE_SIGNAL: &str = "page_global_cookie_authority_mixed";

#[derive(Clone, Default)]
pub struct RuntimeStateCallbacks {
    pub allocate_sequence: Option<RuntimeStateSequenceAllocator>,
    pub on_snapshot: Option<RuntimeStateSnapshotCallback>,
}

impl RuntimeStateCallbacks {
    pub fn is_empty(&self) -> bool {
        self.allocate_sequence.is_none() || self.on_snapshot.is_none()
    }
}

#[derive(Debug, Default, Deserialize)]
struct StorageProbe {
    #[serde(default)]
    local_storage_keys: Vec<String>,
    #[serde(default)]
    session_storage_keys: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ReadinessProbe {
    #[serde(default)]
    document_ready_state: String,
    #[serde(default)]
    loading_present: bool,
    #[serde(default)]
    skeleton_present: bool,
    #[serde(default)]
    overlay_state: String,
    #[serde(default)]
    route_stability: String,
}

#[derive(Debug, Default, Clone, PartialEq, Deserialize)]
struct DocumentFenceProbe {
    #[serde(default)]
    href: String,
    #[serde(default)]
    time_origin: Option<f64>,
}

const STORAGE_PROBE_JS: &str = r#"
(() => JSON.stringify({
    local_storage_keys: Object.keys(window.localStorage || {}),
    session_storage_keys: Object.keys(window.sessionStorage || {})
}))()
"#;

const DOCUMENT_FENCE_PROBE_JS: &str = r#"
(() => JSON.stringify({
    href: String(window.location.href || ''),
    time_origin: Number.isFinite(window.performance?.timeOrigin) ? window.performance.timeOrigin : null,
}))()
"#;

const READINESS_PROBE_JS: &str = r#"
(() => {
    const collectCandidates = (selectors, limit = 64) => {
        const seen = new Set();
        const results = [];
        for (const selector of selectors) {
            let matches = [];
            try {
                matches = Array.from(document.querySelectorAll(selector));
            } catch (_error) {
                continue;
            }
            for (const el of matches) {
                if (!(el instanceof Element)) continue;
                if (seen.has(el)) continue;
                seen.add(el);
                results.push(el);
                if (results.length >= limit) {
                    return results;
                }
            }
        }
        return results;
    };
    const textish = (value) => typeof value === 'string' ? value : '';
    const viewportArea = Math.max(window.innerWidth * window.innerHeight, 1);
    const matchesPattern = (el, pattern) => {
        const parts = [
            textish(el.id),
            textish(el.className),
            textish(el.getAttribute && el.getAttribute('data-testid')),
            textish(el.getAttribute && el.getAttribute('data-test')),
            textish(el.getAttribute && el.getAttribute('data-qa')),
            textish(el.getAttribute && el.getAttribute('role')),
            textish(el.getAttribute && el.getAttribute('aria-label')),
        ];
        return pattern.test(parts.join(' '));
    };
    const isVisible = (el) => {
        if (!(el instanceof Element)) return false;
        const rect = el.getBoundingClientRect();
        if (rect.width < 8 || rect.height < 8) return false;
        const style = getComputedStyle(el);
        return style.display !== 'none'
            && style.visibility !== 'hidden'
            && Number(style.opacity || '1') > 0.01
            && style.pointerEvents !== 'none';
    };
    const overlayCandidates = collectCandidates([
        'dialog[open]',
        '[aria-modal="true"]',
        '[role="dialog"]',
        '[role="alertdialog"]',
        '[id*="modal" i]',
        '[class*="modal" i]',
        '[id*="dialog" i]',
        '[class*="dialog" i]',
        '[id*="overlay" i]',
        '[class*="overlay" i]',
        '[id*="popup" i]',
        '[class*="popup" i]',
        '[id*="drawer" i]',
        '[class*="drawer" i]',
        '[id*="sheet" i]',
        '[class*="sheet" i]',
        '[id*="mask" i]',
        '[class*="mask" i]',
        '[id*="backdrop" i]',
        '[class*="backdrop" i]',
        '[id*="scrim" i]',
        '[class*="scrim" i]',
    ]);
    const hasUserBlockingOverlay = (() => {
        const overflowHidden = [document.body, document.documentElement]
            .filter(Boolean)
            .some((node) => {
                const overflow = getComputedStyle(node).overflow;
                return overflow === 'hidden' || overflow === 'clip';
            });
        let dialogLike = false;
        let backdropLike = false;
        for (const el of overlayCandidates) {
            if (!isVisible(el)) continue;
            const rect = el.getBoundingClientRect();
            const style = getComputedStyle(el);
            const area = rect.width * rect.height;
            const fixedLike = style.position === 'fixed' || style.position === 'sticky';
            const modalSemantics = el.matches && el.matches('dialog[open], [aria-modal="true"], [role="dialog"], [role="alertdialog"]');
            const modalPattern = matchesPattern(el, /\b(modal|dialog|overlay|popup|drawer|sheet|mask)\b/i);
            const largeCenteredPanel = fixedLike
                && area >= viewportArea * 0.18
                && rect.top < window.innerHeight * 0.75
                && rect.bottom > window.innerHeight * 0.2;
            if (modalSemantics || (modalPattern && largeCenteredPanel)) {
                dialogLike = true;
            }
            if (
                fixedLike
                && area >= viewportArea * 0.45
                && (matchesPattern(el, /\b(backdrop|scrim|overlay|mask)\b/i)
                    || Number(style.zIndex || '0') >= 20)
            ) {
                backdropLike = true;
            }
            if (dialogLike && (backdropLike || overflowHidden)) {
                return true;
            }
        }
        return dialogLike && overflowHidden;
    })();

    const loadingCandidates = collectCandidates([
        '[aria-busy="true"]',
        '[role="progressbar"]',
        '[id*="loading" i]',
        '[class*="loading" i]',
        '[data-testid*="loading" i]',
        '[data-test*="loading" i]',
        '[data-qa*="loading" i]',
        '[id*="spinner" i]',
        '[class*="spinner" i]',
        '[id*="progress" i]',
        '[class*="progress" i]',
    ]);
    const loadingPresent = loadingCandidates.some((el) => {
        if (el.getAttribute && el.getAttribute('aria-busy') === 'true') return true;
        if (el.getAttribute && el.getAttribute('role') === 'progressbar') return true;
        return matchesPattern(el, /\b(loading|spinner|progress)\b/i);
    });

    const skeletonCandidates = collectCandidates([
        '[id*="skeleton" i]',
        '[class*="skeleton" i]',
        '[id*="shimmer" i]',
        '[class*="shimmer" i]',
        '[id*="placeholder" i]',
        '[class*="placeholder" i]',
        '[data-testid*="skeleton" i]',
        '[data-test*="skeleton" i]',
        '[data-qa*="skeleton" i]',
    ]);
    const skeletonPresent = skeletonCandidates.some((el) => matchesPattern(el, /\b(skeleton|shimmer|placeholder)\b/i));

    let overlayState = 'none';
    if (document.querySelector('vite-error-overlay, nextjs-portal, [data-nextjs-dialog-overlay], #webpack-dev-server-client-overlay, iframe#webpack-dev-server-client-overlay')) {
        overlayState = 'development';
    } else if (overlayCandidates.some((el) => matchesPattern(el, /\b(error-overlay|runtime-error|stack-trace)\b/i))) {
        overlayState = 'error';
    } else if (hasUserBlockingOverlay) {
        overlayState = 'user_blocking';
    }

    const readyState = document.readyState;
    const routeStability = readyState === 'complete' && !loadingPresent && !skeletonPresent && overlayState === 'none'
        ? 'stable'
        : 'transitioning';

    return JSON.stringify({
        document_ready_state: readyState,
        loading_present: loadingPresent,
        skeleton_present: skeletonPresent,
        overlay_state: overlayState,
        route_stability: routeStability,
    });
})()
"#;

pub async fn probe_page_runtime_state(page: Arc<Page>, callbacks: RuntimeStateCallbacks) {
    let Some(allocate_sequence) = callbacks.allocate_sequence else {
        return;
    };
    let Some(on_snapshot) = callbacks.on_snapshot else {
        return;
    };

    let sequence = allocate_sequence();
    let snapshot = capture_runtime_state(&page).await;
    on_snapshot(sequence, snapshot);
}

pub async fn capture_runtime_state(page: &Arc<Page>) -> RuntimeStateSnapshot {
    capture_runtime_state_for_current_frame(page)
        .await
        .unwrap_or_else(frame_context_unavailable_snapshot)
}

pub async fn capture_runtime_state_for_current_frame(
    page: &Arc<Page>,
) -> Result<RuntimeStateSnapshot, RubError> {
    capture_runtime_state_for_frame_context(page, None).await
}

pub async fn capture_runtime_state_for_explicit_frame(
    page: &Arc<Page>,
    frame_id: &str,
) -> Result<RuntimeStateSnapshot, RubError> {
    capture_runtime_state_for_frame_context(page, Some(frame_id)).await
}

async fn capture_runtime_state_for_frame_context(
    page: &Arc<Page>,
    frame_id: Option<&str>,
) -> Result<RuntimeStateSnapshot, RubError> {
    let context_id = crate::frame_runtime::resolve_frame_context(page, frame_id)
        .await?
        .execution_context_id;

    Ok(capture_runtime_state_in_context(page, context_id, frame_id.is_some()).await)
}

fn frame_context_unavailable_snapshot(error: RubError) -> RuntimeStateSnapshot {
    RuntimeStateSnapshot {
        state_inspector: StateInspectorInfo {
            status: StateInspectorStatus::Degraded,
            degraded_reason: Some(format!("frame_context_unavailable:{error}")),
            ..StateInspectorInfo::default()
        },
        readiness_state: ReadinessInfo {
            status: ReadinessStatus::Degraded,
            degraded_reason: Some(format!("frame_context_unavailable:{error}")),
            ..ReadinessInfo::default()
        },
    }
}

async fn capture_runtime_state_in_context(
    page: &Arc<Page>,
    context_id: Option<ExecutionContextId>,
    frame_scoped: bool,
) -> RuntimeStateSnapshot {
    let document_before = probe_document_fence(page, context_id).await;
    let mut state_inspector = probe_state_inspector(page, context_id, frame_scoped).await;
    let mut readiness_state = probe_readiness(page, context_id).await;
    let document_after = probe_document_fence(page, context_id).await;

    if let Some(reason) =
        runtime_document_fence_failure_reason(document_before.as_ref(), document_after.as_ref())
    {
        degrade_runtime_snapshot_for_document_fence(
            &mut state_inspector,
            &mut readiness_state,
            reason,
        );
    }

    RuntimeStateSnapshot {
        state_inspector,
        readiness_state,
    }
}

async fn probe_document_fence(
    page: &Arc<Page>,
    context_id: Option<ExecutionContextId>,
) -> Option<DocumentFenceProbe> {
    probe_json_in_context(
        page,
        context_id,
        DOCUMENT_FENCE_PROBE_JS,
        parse_document_fence_probe_json,
    )
    .await
    .map_err(|reason| {
        warn!(reason = %reason, "Runtime document fence probe failed");
        reason
    })
    .ok()
}

async fn probe_state_inspector(
    page: &Arc<Page>,
    context_id: Option<ExecutionContextId>,
    frame_scoped: bool,
) -> StateInspectorInfo {
    let mut degraded_reasons = Vec::new();

    let cookie_count = match page.get_cookies().await {
        Ok(cookies) => cookies.len() as u32,
        Err(error) => {
            degraded_reasons.push("cookie_query_failed".to_string());
            warn!(error = %error, "State inspector failed to query cookies");
            0
        }
    };

    let storage =
        match probe_json_in_context(page, context_id, STORAGE_PROBE_JS, parse_storage_probe_json)
            .await
        {
            Ok(storage) => storage,
            Err(reason) => {
                degraded_reasons.push(reason);
                StorageProbe::default()
            }
        };

    build_state_inspector_info(cookie_count, storage, degraded_reasons, frame_scoped)
}

async fn probe_readiness(
    page: &Arc<Page>,
    context_id: Option<ExecutionContextId>,
) -> ReadinessInfo {
    match probe_json_in_context(
        page,
        context_id,
        READINESS_PROBE_JS,
        parse_readiness_probe_json,
    )
    .await
    {
        Ok(probe) => {
            let route_stability = parse_route_stability(&probe.route_stability);
            let overlay_state = parse_overlay_state(&probe.overlay_state);
            ReadinessInfo {
                status: ReadinessStatus::Active,
                route_stability,
                loading_present: probe.loading_present,
                skeleton_present: probe.skeleton_present,
                overlay_state,
                document_ready_state: normalize_document_ready_state(&probe.document_ready_state),
                blocking_signals: infer_blocking_signals(
                    &probe.document_ready_state,
                    probe.loading_present,
                    probe.skeleton_present,
                    overlay_state,
                    route_stability,
                ),
                degraded_reason: None,
            }
        }
        Err(reason) => ReadinessInfo {
            status: ReadinessStatus::Degraded,
            degraded_reason: Some(reason),
            ..ReadinessInfo::default()
        },
    }
}

async fn probe_json_in_context<T>(
    page: &Arc<Page>,
    context_id: Option<ExecutionContextId>,
    expression: &str,
    parser: fn(&str) -> Result<T, String>,
) -> Result<T, String> {
    let evaluated = tokio::time::timeout(
        RUNTIME_PROBE_TIMEOUT,
        crate::js::evaluate_returning_string_in_context(page, context_id, expression),
    )
    .await;

    let payload = match evaluated {
        Ok(Ok(payload)) => payload,
        Ok(Err(error)) => {
            warn!(error = %error, "Runtime probe failed");
            return Err("probe_failed".to_string());
        }
        Err(_) => {
            warn!("Runtime probe timed out");
            return Err("probe_timeout".to_string());
        }
    };

    parser(&payload)
}

fn parse_storage_probe_json(json: &str) -> Result<StorageProbe, String> {
    serde_json::from_str::<StorageProbe>(json).map_err(|_| "storage_probe_malformed".to_string())
}

fn parse_readiness_probe_json(json: &str) -> Result<ReadinessProbe, String> {
    serde_json::from_str::<ReadinessProbe>(json).map_err(|_| "probe_malformed".to_string())
}

fn parse_document_fence_probe_json(json: &str) -> Result<DocumentFenceProbe, String> {
    serde_json::from_str::<DocumentFenceProbe>(json)
        .map_err(|_| "document_fence_probe_malformed".to_string())
}

fn runtime_document_fence_failure_reason(
    before: Option<&DocumentFenceProbe>,
    after: Option<&DocumentFenceProbe>,
) -> Option<&'static str> {
    let (Some(before), Some(after)) = (before, after) else {
        return Some("document_fence_unavailable");
    };
    if !document_fence_is_authoritative(before) || !document_fence_is_authoritative(after) {
        return Some("document_fence_unavailable");
    }
    if before != after {
        return Some("document_changed_during_runtime_probe");
    }
    None
}

fn document_fence_is_authoritative(probe: &DocumentFenceProbe) -> bool {
    !probe.href.is_empty() && probe.time_origin.is_some()
}

fn degrade_runtime_snapshot_for_document_fence(
    state_inspector: &mut StateInspectorInfo,
    readiness_state: &mut ReadinessInfo,
    reason: &'static str,
) {
    state_inspector.status = StateInspectorStatus::Degraded;
    state_inspector.degraded_reason =
        append_degraded_reason(state_inspector.degraded_reason.take(), reason);
    readiness_state.status = ReadinessStatus::Degraded;
    readiness_state.degraded_reason =
        append_degraded_reason(readiness_state.degraded_reason.take(), reason);
}

fn append_degraded_reason(existing: Option<String>, reason: &str) -> Option<String> {
    match existing {
        Some(existing) if existing.split(',').any(|entry| entry == reason) => Some(existing),
        Some(existing) if existing.is_empty() => Some(reason.to_string()),
        Some(existing) => Some(format!("{existing},{reason}")),
        None => Some(reason.to_string()),
    }
}

fn build_state_inspector_info(
    cookie_count: u32,
    storage: StorageProbe,
    mut degraded_reasons: Vec<String>,
    frame_scoped: bool,
) -> StateInspectorInfo {
    let auth_cookie_count = if frame_scoped { 0 } else { cookie_count };
    let mut auth_signals = infer_auth_signals(
        auth_cookie_count,
        &storage.local_storage_keys,
        &storage.session_storage_keys,
    );
    if frame_scoped {
        degraded_reasons.push(FRAME_SCOPED_PAGE_GLOBAL_COOKIE_REASON.to_string());
        auth_signals.push(FRAME_SCOPED_PAGE_GLOBAL_COOKIE_SIGNAL.to_string());
    }

    StateInspectorInfo {
        status: if degraded_reasons.is_empty() {
            StateInspectorStatus::Active
        } else {
            StateInspectorStatus::Degraded
        },
        auth_state: infer_auth_state(
            auth_cookie_count,
            &storage.local_storage_keys,
            &storage.session_storage_keys,
        ),
        cookie_count,
        auth_signals,
        local_storage_keys: storage.local_storage_keys,
        session_storage_keys: storage.session_storage_keys,
        degraded_reason: (!degraded_reasons.is_empty()).then(|| degraded_reasons.join(",")),
    }
}

fn infer_auth_state(
    cookie_count: u32,
    local_storage_keys: &[String],
    session_storage_keys: &[String],
) -> AuthState {
    if cookie_count == 0 && local_storage_keys.is_empty() && session_storage_keys.is_empty() {
        AuthState::Anonymous
    } else {
        AuthState::Unknown
    }
}

fn infer_auth_signals(
    cookie_count: u32,
    local_storage_keys: &[String],
    session_storage_keys: &[String],
) -> Vec<String> {
    let mut signals = Vec::new();
    if cookie_count > 0 {
        signals.push("cookies_present".to_string());
    }
    if !local_storage_keys.is_empty() {
        signals.push("local_storage_present".to_string());
    }
    if !session_storage_keys.is_empty() {
        signals.push("session_storage_present".to_string());
    }
    if local_storage_keys
        .iter()
        .chain(session_storage_keys.iter())
        .any(|key| is_auth_like_key(key))
    {
        signals.push("auth_like_storage_key_present".to_string());
    }
    signals
}

fn is_auth_like_key(key: &str) -> bool {
    let lower = key.to_lowercase();
    ["token", "auth", "session", "csrf", "jwt", "bearer"]
        .iter()
        .any(|needle| lower.contains(needle))
}

fn parse_route_stability(value: &str) -> RouteStability {
    match value {
        "stable" => RouteStability::Stable,
        "transitioning" => RouteStability::Transitioning,
        _ => RouteStability::Unknown,
    }
}

fn parse_overlay_state(value: &str) -> OverlayState {
    match value {
        "development" => OverlayState::Development,
        "error" => OverlayState::Error,
        "user_blocking" => OverlayState::UserBlocking,
        _ => OverlayState::None,
    }
}

fn normalize_document_ready_state(value: &str) -> Option<String> {
    match value {
        "loading" | "interactive" | "complete" => Some(value.to_string()),
        _ => None,
    }
}

fn infer_blocking_signals(
    document_ready_state: &str,
    loading_present: bool,
    skeleton_present: bool,
    overlay_state: OverlayState,
    route_stability: RouteStability,
) -> Vec<String> {
    let mut signals = Vec::new();
    if document_ready_state != "complete" {
        signals.push(format!("document_ready_state:{document_ready_state}"));
    }
    if loading_present {
        signals.push("loading_present".to_string());
    }
    if skeleton_present {
        signals.push("skeleton_present".to_string());
    }
    match overlay_state {
        OverlayState::Development => signals.push("overlay:development".to_string()),
        OverlayState::Error => signals.push("overlay:error".to_string()),
        OverlayState::UserBlocking => signals.push("overlay:user_blocking".to_string()),
        OverlayState::None => {}
    }
    if matches!(route_stability, RouteStability::Transitioning) {
        signals.push("route_transitioning".to_string());
    }
    signals
}

#[cfg(test)]
mod tests;
