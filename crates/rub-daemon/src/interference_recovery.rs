use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use tokio::time::{Instant, sleep};

use rub_core::model::{
    HumanVerificationHandoffStatus, InterferenceKind, InterferenceRecoveryAction,
    InterferenceRecoveryReport, InterferenceRecoveryResult, InterferenceRuntimeInfo,
    InterferenceRuntimeStatus, KeyCombo, OverlayState, ReadinessInfo, ReadinessStatus,
    RouteStability, TabInfo,
};
use rub_core::port::BrowserPort;

use crate::interference::InterferenceRecoveryContext;
use crate::runtime_refresh::refresh_live_runtime_and_interference;
use crate::session::SessionState;

const RECOVERY_POLL_INTERVAL: Duration = Duration::from_millis(200);
const RECOVERY_TIMEOUT: Duration = Duration::from_secs(4);
const DISMISS_OVERLAY_PROBE_JS: &str = r#"
(() => {
    const nodes = Array.from(document.querySelectorAll('*'));
    const viewportArea = Math.max(window.innerWidth * window.innerHeight, 1);
    const textish = (value) => typeof value === 'string' ? value : '';
    const matchesPattern = (el, pattern) => {
        const parts = [
            textish(el.id),
            textish(el.className),
            textish(el.textContent),
            textish(el.getAttribute && el.getAttribute('aria-label')),
            textish(el.getAttribute && el.getAttribute('title')),
            textish(el.getAttribute && el.getAttribute('data-testid')),
            textish(el.getAttribute && el.getAttribute('role')),
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
    const overflowHidden = [document.body, document.documentElement]
        .filter(Boolean)
        .some((node) => {
            const overflow = getComputedStyle(node).overflow;
            return overflow === 'hidden' || overflow === 'clip';
        });
    let overlayActive = false;
    for (const el of nodes) {
        if (!isVisible(el)) continue;
        const rect = el.getBoundingClientRect();
        const area = rect.width * rect.height;
        const style = getComputedStyle(el);
        const fixedLike = style.position === 'fixed' || style.position === 'sticky';
        const modalSemantics = el.matches && el.matches('dialog[open], [aria-modal="true"], [role="dialog"], [role="alertdialog"]');
        const modalPattern = matchesPattern(el, /\b(modal|dialog|overlay|popup|drawer|sheet|mask)\b/i);
        const largeCenteredPanel = fixedLike
            && area >= viewportArea * 0.18
            && rect.top < window.innerHeight * 0.75
            && rect.bottom > window.innerHeight * 0.2;
        const backdropLike = fixedLike
            && area >= viewportArea * 0.45
            && (matchesPattern(el, /\b(backdrop|scrim|overlay|mask)\b/i) || Number(style.zIndex || '0') >= 20);
        if (modalSemantics || (modalPattern && largeCenteredPanel && (backdropLike || overflowHidden))) {
            overlayActive = true;
            break;
        }
    }
    if (!overlayActive) {
        return JSON.stringify({ status: 'cleared' });
    }

    const allowPattern = /\b(close|dismiss|cancel|skip|later|not now|关闭|取消|跳过|稍后|以后再说|暂不|我知道了|知道了)\b/i;
    const denyPattern = /\b(log ?in|sign ?in|register|continue|submit|allow|accept|登录|注册|继续|确认|授权|同意)\b/i;
    const candidates = [];
    for (const el of nodes) {
        if (!isVisible(el)) continue;
        if (!(el.matches && el.matches('button, [role="button"], [aria-label], [title], [data-testid], [data-test]'))) {
            continue;
        }
        const label = [
            textish(el.textContent),
            textish(el.getAttribute && el.getAttribute('aria-label')),
            textish(el.getAttribute && el.getAttribute('title')),
            textish(el.getAttribute && el.getAttribute('data-testid')),
            textish(el.id),
            textish(el.className),
        ].join(' ').replace(/\s+/g, ' ').trim();
        if (!allowPattern.test(label) || denyPattern.test(label)) continue;
        const rect = el.getBoundingClientRect();
        const style = getComputedStyle(el);
        const score = (style.position === 'fixed' ? 20 : 0)
            + (matchesPattern(el, /\b(close|dismiss|cancel|skip|later|not-now|关闭|取消|跳过)\b/i) ? 10 : 0)
            + Math.min(rect.width * rect.height / 500, 20);
        candidates.push({
            x: rect.left + rect.width / 2,
            y: rect.top + rect.height / 2,
            label,
            score,
        });
    }
    candidates.sort((a, b) => b.score - a.score);
    const best = candidates[0];
    if (!best) {
        return JSON.stringify({ status: 'blocked' });
    }
    return JSON.stringify({
        status: 'candidate',
        x: best.x,
        y: best.y,
        label: best.label,
    });
})()
"#;

#[derive(Debug, Deserialize)]
struct OverlayDismissProbe {
    status: String,
    #[serde(default)]
    x: Option<f64>,
    #[serde(default)]
    y: Option<f64>,
    #[serde(default)]
    label: Option<String>,
}

pub(crate) async fn recover(
    browser: &Arc<dyn BrowserPort>,
    state: &Arc<SessionState>,
) -> InterferenceRecoveryReport {
    let tabs = match refresh_live_runtime_and_interference(browser, state).await {
        Ok(tabs) => tabs,
        Err(error) => {
            return InterferenceRecoveryReport {
                reason: Some(format!("live_refresh_failed:{error}")),
                ..InterferenceRecoveryReport::default()
            };
        }
    };

    let context = state.interference_recovery_context().await;
    let Some(current) = context.projection.current_interference.as_ref() else {
        return InterferenceRecoveryReport {
            reason: Some("no_active_interference".to_string()),
            ..InterferenceRecoveryReport::default()
        };
    };

    let Some(action) = select_recovery_action(&context, &tabs) else {
        state
            .record_interference_recovery_outcome(None, InterferenceRecoveryResult::Abandoned)
            .await;
        return InterferenceRecoveryReport {
            attempted: false,
            result: Some(InterferenceRecoveryResult::Abandoned),
            reason: Some(format!(
                "interference_not_safely_recoverable:{}",
                interference_kind_label(current.kind)
            )),
            ..InterferenceRecoveryReport::default()
        };
    };

    state.begin_interference_recovery(action).await;

    if matches!(action, InterferenceRecoveryAction::EscalateToHandoff) {
        let handoff = state.human_verification_handoff().await;
        if matches!(handoff.status, HumanVerificationHandoffStatus::Unavailable) {
            state
                .finish_interference_recovery(InterferenceRecoveryResult::Failed)
                .await;
            return InterferenceRecoveryReport {
                attempted: true,
                action: Some(action),
                result: Some(InterferenceRecoveryResult::Failed),
                fence_satisfied: false,
                reason: Some("handoff_unavailable".to_string()),
            };
        }

        state.activate_handoff().await;
        let _ = refresh_live_runtime_and_interference(browser, state).await;
        state
            .finish_interference_recovery(InterferenceRecoveryResult::Escalated)
            .await;
        let handoff = state.human_verification_handoff().await;
        return InterferenceRecoveryReport {
            attempted: true,
            action: Some(action),
            result: Some(InterferenceRecoveryResult::Escalated),
            fence_satisfied: matches!(handoff.status, HumanVerificationHandoffStatus::Active),
            reason: Some("handoff_activated".to_string()),
        };
    }

    if let Err(reason) = apply_recovery_action(browser, action, &context, &tabs).await {
        let _ = refresh_live_runtime_and_interference(browser, state).await;
        state
            .finish_interference_recovery(InterferenceRecoveryResult::Failed)
            .await;
        return InterferenceRecoveryReport {
            attempted: true,
            action: Some(action),
            result: Some(InterferenceRecoveryResult::Failed),
            fence_satisfied: false,
            reason: Some(reason),
        };
    }

    let (fence_satisfied, reason) = wait_for_recovery_fence(browser, state, action, &context).await;
    let result = if fence_satisfied {
        InterferenceRecoveryResult::Succeeded
    } else {
        InterferenceRecoveryResult::Failed
    };
    state.finish_interference_recovery(result).await;

    InterferenceRecoveryReport {
        attempted: true,
        action: Some(action),
        result: Some(result),
        fence_satisfied,
        reason,
    }
}

fn select_recovery_action(
    context: &InterferenceRecoveryContext,
    tabs: &[TabInfo],
) -> Option<InterferenceRecoveryAction> {
    match context.projection.current_interference.as_ref()?.kind {
        InterferenceKind::InterstitialNavigation => Some(InterferenceRecoveryAction::BackNavigate),
        InterferenceKind::PopupHijack => find_active_tab(tabs)
            .map(|_| InterferenceRecoveryAction::CloseUnexpectedTab)
            .or(Some(InterferenceRecoveryAction::RestorePrimaryContext)),
        InterferenceKind::UnknownNavigationDrift => {
            if find_primary_tab(tabs, context).is_some() {
                Some(InterferenceRecoveryAction::RestorePrimaryContext)
            } else {
                Some(InterferenceRecoveryAction::BackNavigate)
            }
        }
        InterferenceKind::HumanVerificationRequired => {
            Some(InterferenceRecoveryAction::EscalateToHandoff)
        }
        InterferenceKind::OverlayInterference => Some(InterferenceRecoveryAction::DismissOverlay),
        InterferenceKind::ThirdPartyNoise => None,
    }
}

async fn apply_recovery_action(
    browser: &Arc<dyn BrowserPort>,
    action: InterferenceRecoveryAction,
    context: &InterferenceRecoveryContext,
    tabs: &[TabInfo],
) -> Result<(), String> {
    match action {
        InterferenceRecoveryAction::BackNavigate => browser
            .back(rub_core::DEFAULT_WAIT_TIMEOUT_MS)
            .await
            .map(|_| ())
            .map_err(|error| format!("back_navigation_failed:{error}")),
        InterferenceRecoveryAction::CloseUnexpectedTab => {
            let Some(active) = find_active_tab(tabs) else {
                return Err("active_tab_not_found".to_string());
            };
            browser
                .close_tab(Some(active.index))
                .await
                .map(|_| ())
                .map_err(|error| format!("close_unexpected_tab_failed:{error}"))
        }
        InterferenceRecoveryAction::RestorePrimaryContext => {
            let Some(primary) = find_primary_tab(tabs, context) else {
                return Err("primary_context_not_found".to_string());
            };
            browser
                .switch_tab(primary.index)
                .await
                .map(|_| ())
                .map_err(|error| format!("restore_primary_context_failed:{error}"))
        }
        InterferenceRecoveryAction::DismissOverlay => dismiss_overlay(browser).await,
        InterferenceRecoveryAction::EscalateToHandoff => Ok(()),
    }
}

async fn wait_for_recovery_fence(
    browser: &Arc<dyn BrowserPort>,
    state: &Arc<SessionState>,
    action: InterferenceRecoveryAction,
    context: &InterferenceRecoveryContext,
) -> (bool, Option<String>) {
    let deadline = Instant::now() + RECOVERY_TIMEOUT;

    loop {
        let tabs = match refresh_live_runtime_and_interference(browser, state).await {
            Ok(tabs) => tabs,
            Err(error) => return (false, Some(format!("live_refresh_failed:{error}"))),
        };
        let runtime = state.interference_runtime().await;
        let readiness = state.readiness_state().await;

        if recovery_fence_satisfied(action, context, &tabs, &runtime, &readiness) {
            return (true, Some("primary_context_restored".to_string()));
        }

        if Instant::now() >= deadline {
            return (false, Some("recovery_fence_timed_out".to_string()));
        }

        sleep(RECOVERY_POLL_INTERVAL).await;
    }
}

fn recovery_fence_satisfied(
    action: InterferenceRecoveryAction,
    context: &InterferenceRecoveryContext,
    tabs: &[TabInfo],
    runtime: &InterferenceRuntimeInfo,
    readiness: &ReadinessInfo,
) -> bool {
    if matches!(action, InterferenceRecoveryAction::EscalateToHandoff) {
        return false;
    }

    if !matches!(runtime.status, InterferenceRuntimeStatus::Inactive) {
        return false;
    }
    if runtime.current_interference.is_some() {
        return false;
    }
    let Some(active_tab) = find_active_tab(tabs) else {
        return false;
    };

    match action {
        InterferenceRecoveryAction::BackNavigate => context
            .baseline
            .primary_url
            .as_deref()
            .is_none_or(|primary_url| active_tab.url == primary_url),
        InterferenceRecoveryAction::CloseUnexpectedTab => {
            let primary_ok = primary_context_restored(active_tab, context);
            let tab_count_ok = context.baseline.last_tab_count == 0
                || tabs.len() <= context.baseline.last_tab_count;
            primary_ok && tab_count_ok
        }
        InterferenceRecoveryAction::RestorePrimaryContext => {
            if !readiness_allows_resume(readiness) {
                return false;
            }
            primary_context_restored(active_tab, context)
        }
        InterferenceRecoveryAction::DismissOverlay => {
            overlay_dismiss_fence_satisfied(readiness)
                && primary_context_restored(active_tab, context)
        }
        InterferenceRecoveryAction::EscalateToHandoff => false,
    }
}

fn readiness_allows_resume(readiness: &ReadinessInfo) -> bool {
    matches!(readiness.status, ReadinessStatus::Active)
        && !matches!(readiness.route_stability, RouteStability::Transitioning)
        && !readiness.loading_present
        && !readiness.skeleton_present
        && matches!(readiness.overlay_state, OverlayState::None)
}

fn overlay_dismiss_fence_satisfied(readiness: &ReadinessInfo) -> bool {
    matches!(readiness.status, ReadinessStatus::Active)
        && matches!(readiness.overlay_state, OverlayState::None)
}

fn primary_context_restored(active_tab: &TabInfo, context: &InterferenceRecoveryContext) -> bool {
    context
        .baseline
        .primary_target_id
        .as_deref()
        .is_some_and(|target_id| active_tab.target_id == target_id)
        || context
            .baseline
            .primary_url
            .as_deref()
            .is_some_and(|url| active_tab.url == url)
}

fn find_active_tab(tabs: &[TabInfo]) -> Option<&TabInfo> {
    tabs.iter().find(|tab| tab.active)
}

fn find_primary_tab<'a>(
    tabs: &'a [TabInfo],
    context: &InterferenceRecoveryContext,
) -> Option<&'a TabInfo> {
    context
        .baseline
        .primary_target_id
        .as_deref()
        .and_then(|target_id| tabs.iter().find(|tab| tab.target_id == target_id))
        .or_else(|| {
            context
                .baseline
                .primary_url
                .as_deref()
                .and_then(|url| tabs.iter().find(|tab| tab.url == url))
        })
}

fn interference_kind_label(kind: InterferenceKind) -> &'static str {
    match kind {
        InterferenceKind::InterstitialNavigation => "interstitial_navigation",
        InterferenceKind::PopupHijack => "popup_hijack",
        InterferenceKind::OverlayInterference => "overlay_interference",
        InterferenceKind::ThirdPartyNoise => "third_party_noise",
        InterferenceKind::HumanVerificationRequired => "human_verification_required",
        InterferenceKind::UnknownNavigationDrift => "unknown_navigation_drift",
    }
}

async fn dismiss_overlay(browser: &Arc<dyn BrowserPort>) -> Result<(), String> {
    let escape =
        KeyCombo::parse("Escape").map_err(|error| format!("escape_key_parse_failed:{error}"))?;
    browser
        .send_keys(&escape)
        .await
        .map_err(|error| format!("dismiss_overlay_escape_failed:{error}"))?;

    let probe = browser
        .execute_js(DISMISS_OVERLAY_PROBE_JS)
        .await
        .map_err(|error| format!("dismiss_overlay_probe_failed:{error}"))?;
    let json = probe
        .as_str()
        .ok_or_else(|| "dismiss_overlay_probe_invalid_payload".to_string())?;
    let probe: OverlayDismissProbe = serde_json::from_str(json)
        .map_err(|error| format!("dismiss_overlay_probe_parse_failed:{error}"))?;

    match probe.status.as_str() {
        "cleared" => Ok(()),
        "candidate" => {
            let x = probe
                .x
                .ok_or_else(|| "dismiss_overlay_candidate_missing_x".to_string())?;
            let y = probe
                .y
                .ok_or_else(|| "dismiss_overlay_candidate_missing_y".to_string())?;
            browser.click_xy(x, y).await.map(|_| ()).map_err(|error| {
                let label = probe.label.unwrap_or_else(|| "candidate".to_string());
                format!("dismiss_overlay_click_failed:{label}:{error}")
            })
        }
        "blocked" => Err("dismiss_overlay_candidate_not_found".to_string()),
        other => Err(format!("dismiss_overlay_probe_unknown_status:{other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        find_primary_tab, overlay_dismiss_fence_satisfied, primary_context_restored,
        readiness_allows_resume, select_recovery_action,
    };
    use crate::interference::InterferenceRecoveryContext;
    use crate::interference_classifier::InterferenceBaseline;
    use rub_core::model::{
        InterferenceKind, InterferenceObservation, InterferenceRecoveryAction,
        InterferenceRuntimeInfo, OverlayState, ReadinessInfo, ReadinessStatus, RouteStability,
        TabInfo,
    };

    fn active_tab(index: u32, target_id: &str, url: &str) -> TabInfo {
        TabInfo {
            index,
            target_id: target_id.to_string(),
            url: url.to_string(),
            title: "Title".to_string(),
            active: true,
        }
    }

    fn popup_context(kind: InterferenceKind) -> InterferenceRecoveryContext {
        InterferenceRecoveryContext {
            baseline: InterferenceBaseline {
                primary_target_id: Some("target-1".to_string()),
                primary_url: Some("https://app.example.test/home".to_string()),
                last_tab_count: 1,
            },
            projection: InterferenceRuntimeInfo {
                current_interference: Some(InterferenceObservation {
                    kind,
                    summary: "interference".to_string(),
                    current_url: Some("https://ads.example.test/popup".to_string()),
                    primary_url: Some("https://app.example.test/home".to_string()),
                }),
                ..InterferenceRuntimeInfo::default()
            },
        }
    }

    #[test]
    fn readiness_fence_requires_stable_visible_non_overlay_page() {
        assert!(readiness_allows_resume(&ReadinessInfo {
            status: ReadinessStatus::Active,
            route_stability: RouteStability::Stable,
            loading_present: false,
            skeleton_present: false,
            overlay_state: OverlayState::None,
            ..ReadinessInfo::default()
        }));
        assert!(!readiness_allows_resume(&ReadinessInfo {
            status: ReadinessStatus::Active,
            route_stability: RouteStability::Transitioning,
            ..ReadinessInfo::default()
        }));
    }

    #[test]
    fn popup_hijack_prefers_closing_the_unexpected_active_tab() {
        let action = select_recovery_action(
            &popup_context(InterferenceKind::PopupHijack),
            &[active_tab(1, "target-2", "https://ads.example.test/popup")],
        );
        assert_eq!(action, Some(InterferenceRecoveryAction::CloseUnexpectedTab));
    }

    #[test]
    fn overlay_interference_prefers_dismiss_overlay() {
        let action = select_recovery_action(
            &popup_context(InterferenceKind::OverlayInterference),
            &[active_tab(0, "target-1", "https://app.example.test/home")],
        );
        assert_eq!(action, Some(InterferenceRecoveryAction::DismissOverlay));
    }

    #[test]
    fn primary_context_helpers_use_target_then_url() {
        let context = popup_context(InterferenceKind::UnknownNavigationDrift);
        let primary = TabInfo {
            index: 0,
            target_id: "target-1".to_string(),
            url: "https://app.example.test/home".to_string(),
            title: "Home".to_string(),
            active: true,
        };
        assert!(primary_context_restored(&primary, &context));
        assert!(find_primary_tab(&[primary], &context).is_some());
    }

    #[test]
    fn overlay_dismiss_fence_only_requires_active_non_overlay_page() {
        assert!(overlay_dismiss_fence_satisfied(&ReadinessInfo {
            status: ReadinessStatus::Active,
            route_stability: RouteStability::Transitioning,
            loading_present: true,
            skeleton_present: true,
            overlay_state: OverlayState::None,
            ..ReadinessInfo::default()
        }));
        assert!(!overlay_dismiss_fence_satisfied(&ReadinessInfo {
            status: ReadinessStatus::Active,
            overlay_state: OverlayState::UserBlocking,
            ..ReadinessInfo::default()
        }));
    }
}
