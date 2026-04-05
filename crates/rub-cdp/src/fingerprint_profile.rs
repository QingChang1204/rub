use serde::Serialize;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize)]
pub struct FingerprintProfile {
    pub webgl_vendor: String,
    pub webgl_renderer: String,
    pub canvas_noise: CanvasNoiseProfile,
    pub audio_noise: AudioNoiseProfile,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct CanvasNoiseProfile {
    pub red_offset: u8,
    pub green_offset: u8,
    pub blue_offset: u8,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct AudioNoiseProfile {
    pub first_index: usize,
    pub second_index: usize,
    pub delta: f32,
}

pub fn generate_session_seed() -> u64 {
    let mut hasher = DefaultHasher::new();
    std::process::id().hash(&mut hasher);
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .hash(&mut hasher);
    hasher.finish()
}

impl FingerprintProfile {
    pub fn for_seed(seed: u64) -> Self {
        let (webgl_vendor, webgl_renderer) = webgl_identity_pair(seed);
        let canvas_noise = CanvasNoiseProfile {
            red_offset: 1 + (seed % 3) as u8,
            green_offset: 1 + ((seed >> 3) % 3) as u8,
            blue_offset: 1 + ((seed >> 6) % 3) as u8,
        };
        let audio_noise = AudioNoiseProfile {
            first_index: 1 + (seed % 3) as usize,
            second_index: 6 + ((seed >> 3) % 4) as usize,
            delta: 0.000_011 + (((seed >> 8) % 5) as f32 * 0.000_001),
        };

        Self {
            webgl_vendor: webgl_vendor.to_string(),
            webgl_renderer: webgl_renderer.to_string(),
            canvas_noise,
            audio_noise,
        }
    }
}

fn webgl_identity_pair(seed: u64) -> (&'static str, &'static str) {
    match std::env::consts::OS {
        "macos" => {
            const PROFILES: [(&str, &str); 2] = [
                (
                    "Google Inc. (Apple)",
                    "ANGLE (Apple, ANGLE Metal Renderer: Apple M2, Unspecified Version)",
                ),
                (
                    "Google Inc. (Apple)",
                    "ANGLE (Apple, ANGLE Metal Renderer: Apple M4 Pro, Unspecified Version)",
                ),
            ];
            PROFILES[(seed as usize) % PROFILES.len()]
        }
        "windows" => {
            const PROFILES: [(&str, &str); 2] = [
                (
                    "Google Inc. (Intel)",
                    "ANGLE (Intel, Intel(R) Iris(R) Xe Graphics Direct3D11 vs_5_0 ps_5_0, D3D11)",
                ),
                (
                    "Google Inc. (Intel)",
                    "ANGLE (Intel, Intel(R) UHD Graphics 620 Direct3D11 vs_5_0 ps_5_0, D3D11)",
                ),
            ];
            PROFILES[(seed as usize) % PROFILES.len()]
        }
        "linux" => {
            const PROFILES: [(&str, &str); 2] = [
                (
                    "Google Inc. (Intel)",
                    "ANGLE (Intel, Mesa Intel(R) Xe Graphics (TGL GT2), OpenGL 4.6)",
                ),
                (
                    "Google Inc. (AMD)",
                    "ANGLE (AMD, AMD Radeon Graphics (RADV GFX1031), OpenGL 4.6)",
                ),
            ];
            PROFILES[(seed as usize) % PROFILES.len()]
        }
        _ => (
            "Google Inc. (Apple)",
            "ANGLE (Apple, ANGLE Metal Renderer: Apple GPU, Unspecified Version)",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::FingerprintProfile;

    #[test]
    fn fingerprint_profile_for_seed_is_stable() {
        let first = FingerprintProfile::for_seed(42);
        let second = FingerprintProfile::for_seed(42);

        assert_eq!(first.webgl_vendor, second.webgl_vendor);
        assert_eq!(first.webgl_renderer, second.webgl_renderer);
        assert_eq!(
            first.canvas_noise.red_offset,
            second.canvas_noise.red_offset
        );
        assert_eq!(
            first.audio_noise.first_index,
            second.audio_noise.first_index
        );
    }
}
