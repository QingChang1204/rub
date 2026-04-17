//! Unified identity policy derived from browser launch options.

use chromiumoxide::cdp::browser_protocol::{
    emulation::UserAgentMetadata, network::SetUserAgentOverrideParams,
};

use crate::browser::BrowserLaunchOptions;
use crate::environment_profile::EnvironmentProfile;
use crate::fingerprint_profile::FingerprintProfile;
use crate::stealth::StealthConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityCoverageMode {
    PageFrameWorkerBridge,
}

impl IdentityCoverageMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PageFrameWorkerBridge => "page_frame_worker_bridge",
        }
    }
}

#[derive(Debug, Clone)]
pub struct UserAgentOverrideProfile {
    pub user_agent: String,
    pub params: SetUserAgentOverrideParams,
    pub script: String,
    pub has_metadata: bool,
}

#[derive(Debug, Clone)]
pub struct IdentityPolicy {
    stealth_enabled: bool,
    headless: bool,
    coverage_mode: IdentityCoverageMode,
    fingerprint_profile: FingerprintProfile,
    environment_profile: Option<EnvironmentProfile>,
}

impl IdentityPolicy {
    pub fn from_options(options: &BrowserLaunchOptions) -> Self {
        Self::from_options_with_seed(options, 0)
    }

    pub fn from_options_with_seed(options: &BrowserLaunchOptions, seed: u64) -> Self {
        Self {
            stealth_enabled: options.stealth,
            headless: options.headless,
            coverage_mode: IdentityCoverageMode::PageFrameWorkerBridge,
            fingerprint_profile: FingerprintProfile::for_seed(seed),
            environment_profile: environment_profile_for(options, seed),
        }
    }

    pub fn stealth_enabled(&self) -> bool {
        self.stealth_enabled
    }

    pub fn coverage_mode(&self) -> IdentityCoverageMode {
        self.coverage_mode
    }

    pub fn fingerprint_profile(&self) -> &FingerprintProfile {
        &self.fingerprint_profile
    }

    pub fn environment_profile(&self) -> Option<EnvironmentProfile> {
        self.environment_profile
    }

    pub fn stealth_config(&self) -> StealthConfig {
        StealthConfig {
            enabled: self.stealth_enabled,
            fingerprint_profile: self.fingerprint_profile.clone(),
            environment_profile: self.environment_profile,
        }
    }

    pub fn worker_coverage_supported(&self) -> bool {
        self.stealth_enabled
    }

    pub fn user_agent_override_expected(&self) -> bool {
        self.stealth_enabled && self.headless
    }

    pub fn user_agent_override(
        &self,
        original_user_agent: &str,
    ) -> Option<UserAgentOverrideProfile> {
        if !self.stealth_enabled
            || !self.headless
            || !original_user_agent.contains("HeadlessChrome")
        {
            return None;
        }

        let user_agent = original_user_agent.replace("HeadlessChrome", "Chrome");
        let metadata = user_agent_metadata_for(&user_agent)?;
        let navigator_platform = navigator_platform();

        let params = SetUserAgentOverrideParams::builder()
            .user_agent(user_agent.clone())
            .platform(navigator_platform)
            .user_agent_metadata(metadata)
            .build()
            .ok()?;

        Some(UserAgentOverrideProfile {
            user_agent: user_agent.clone(),
            params,
            script: user_agent_override_script(&user_agent),
            has_metadata: true,
        })
    }
}

fn environment_profile_for(
    options: &BrowserLaunchOptions,
    seed: u64,
) -> Option<EnvironmentProfile> {
    if options.stealth && options.headless {
        Some(EnvironmentProfile::for_seed(seed))
    } else {
        None
    }
}

fn user_agent_metadata_for(user_agent: &str) -> Option<UserAgentMetadata> {
    let platform_version = user_agent_platform_version(user_agent)?;

    UserAgentMetadata::builder()
        .platform(user_agent_metadata_platform())
        .platform_version(platform_version)
        .architecture(user_agent_metadata_architecture())
        .model("")
        .mobile(false)
        .bitness("64")
        .build()
        .ok()
}

fn navigator_platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "MacIntel",
        "windows" => "Win32",
        "linux" => "Linux x86_64",
        _ => "MacIntel",
    }
}

fn user_agent_metadata_platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "macOS",
        "windows" => "Windows",
        "linux" => "Linux",
        _ => "macOS",
    }
}

fn user_agent_metadata_architecture() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm",
        "x86_64" => "x86",
        "x86" => "x86",
        other if other.starts_with("arm") => "arm",
        _ => "x86",
    }
}

fn user_agent_platform_version(user_agent: &str) -> Option<String> {
    if let Some(start) = user_agent.find("Mac OS X ") {
        let version = &user_agent[start + "Mac OS X ".len()..];
        let version = version.split(')').next()?;
        return Some(version.replace('_', "."));
    }
    if let Some(start) = user_agent.find("Windows NT ") {
        let version = &user_agent[start + "Windows NT ".len()..];
        let version = version.split(';').next().unwrap_or(version);
        return Some(version.trim().to_string());
    }
    if user_agent.contains("Linux") {
        return Some("0.0.0".to_string());
    }
    None
}

fn user_agent_override_script(user_agent: &str) -> String {
    format!(
        r#"
(() => {{
    const markNative = globalThis[Symbol.for('rub.stealth.mark_native')];
    const value = '{}';
    try {{
        delete navigator.userAgent;
    }} catch (_) {{}}
    const target =
        (typeof Navigator !== 'undefined' && Navigator.prototype) ||
        Object.getPrototypeOf(navigator);
    if (!target) return;
    const nativeDesc = Object.getOwnPropertyDescriptor(target, 'userAgent');
    const wrappedGetter = function userAgent() {{
        return value;
    }};
    Object.defineProperty(target, 'userAgent', {{
        get: typeof markNative === 'function'
            ? markNative(
                wrappedGetter,
                nativeDesc && typeof nativeDesc.get === 'function' ? nativeDesc.get : undefined,
                'function get userAgent() {{ [native code] }}'
            )
            : wrappedGetter,
        configurable: true,
    }});
}})();
"#,
        user_agent.replace('\'', "\\'")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> BrowserLaunchOptions {
        BrowserLaunchOptions {
            headless: true,
            ignore_cert_errors: false,
            user_data_dir: None,
            managed_profile_ephemeral: false,
            download_dir: None,
            profile_directory: None,
            hide_infobars: true,
            stealth: true,
        }
    }

    #[test]
    fn user_agent_override_produces_metadata_for_headless_chrome() {
        let policy = IdentityPolicy::from_options(&options());
        let profile = policy
            .user_agent_override(
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) HeadlessChrome/146.0.0.0 Safari/537.36",
            )
            .expect("headless Chrome UA should produce override");

        assert!(!profile.user_agent.contains("HeadlessChrome"));
        assert!(profile.has_metadata);
        assert!(profile.script.contains("navigator"));
        assert!(profile.params.user_agent_metadata.is_some());
    }

    #[test]
    fn user_agent_override_is_skipped_when_stealth_disabled() {
        let mut options = options();
        options.stealth = false;
        let policy = IdentityPolicy::from_options(&options);

        assert!(policy
            .user_agent_override(
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) HeadlessChrome/146.0.0.0 Safari/537.36",
            )
            .is_none());
    }
}
