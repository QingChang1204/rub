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
