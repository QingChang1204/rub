use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct EnvironmentProfile {
    pub window_width: u32,
    pub window_height: u32,
    pub device_scale_factor: f64,
    pub screen_width: u32,
    pub screen_height: u32,
    pub outer_width: u32,
    pub outer_height: u32,
    pub max_touch_points: u8,
    pub touch_enabled: bool,
}

impl EnvironmentProfile {
    pub fn for_seed(seed: u64) -> Self {
        match std::env::consts::OS {
            "macos" => {
                const PROFILES: [EnvironmentProfile; 2] = [
                    EnvironmentProfile {
                        window_width: 1440,
                        window_height: 900,
                        device_scale_factor: 2.0,
                        screen_width: 1440,
                        screen_height: 900,
                        outer_width: 1440,
                        outer_height: 900,
                        max_touch_points: 0,
                        touch_enabled: false,
                    },
                    EnvironmentProfile {
                        window_width: 1512,
                        window_height: 982,
                        device_scale_factor: 2.0,
                        screen_width: 1512,
                        screen_height: 982,
                        outer_width: 1512,
                        outer_height: 982,
                        max_touch_points: 0,
                        touch_enabled: false,
                    },
                ];
                PROFILES[(seed as usize) % PROFILES.len()]
            }
            "windows" => {
                const PROFILES: [EnvironmentProfile; 2] = [
                    EnvironmentProfile {
                        window_width: 1366,
                        window_height: 768,
                        device_scale_factor: 1.0,
                        screen_width: 1366,
                        screen_height: 768,
                        outer_width: 1366,
                        outer_height: 768,
                        max_touch_points: 0,
                        touch_enabled: false,
                    },
                    EnvironmentProfile {
                        window_width: 1536,
                        window_height: 864,
                        device_scale_factor: 1.25,
                        screen_width: 1536,
                        screen_height: 864,
                        outer_width: 1536,
                        outer_height: 864,
                        max_touch_points: 0,
                        touch_enabled: false,
                    },
                ];
                PROFILES[(seed as usize) % PROFILES.len()]
            }
            "linux" => {
                const PROFILES: [EnvironmentProfile; 2] = [
                    EnvironmentProfile {
                        window_width: 1366,
                        window_height: 768,
                        device_scale_factor: 1.0,
                        screen_width: 1366,
                        screen_height: 768,
                        outer_width: 1366,
                        outer_height: 768,
                        max_touch_points: 0,
                        touch_enabled: false,
                    },
                    EnvironmentProfile {
                        window_width: 1440,
                        window_height: 900,
                        device_scale_factor: 1.0,
                        screen_width: 1440,
                        screen_height: 900,
                        outer_width: 1440,
                        outer_height: 900,
                        max_touch_points: 0,
                        touch_enabled: false,
                    },
                ];
                PROFILES[(seed as usize) % PROFILES.len()]
            }
            _ => EnvironmentProfile {
                window_width: 1440,
                window_height: 900,
                device_scale_factor: 1.0,
                screen_width: 1440,
                screen_height: 900,
                outer_width: 1440,
                outer_height: 900,
                max_touch_points: 0,
                touch_enabled: false,
            },
        }
    }

    pub fn launch_window_arg(self) -> String {
        format!("--window-size={},{}", self.window_width, self.window_height)
    }

    pub fn launch_scale_arg(self) -> String {
        let mut scale = format!("{:.2}", self.device_scale_factor);
        while scale.contains('.') && scale.ends_with('0') {
            scale.pop();
        }
        if scale.ends_with('.') {
            scale.pop();
        }
        format!("--force-device-scale-factor={scale}")
    }
}

#[cfg(test)]
mod tests {
    use super::EnvironmentProfile;

    #[test]
    fn environment_profile_for_seed_is_stable() {
        let first = EnvironmentProfile::for_seed(42);
        let second = EnvironmentProfile::for_seed(42);

        assert_eq!(first, second);
    }

    #[test]
    fn launch_args_render_without_trailing_zero_noise() {
        let mac = EnvironmentProfile {
            window_width: 1440,
            window_height: 900,
            device_scale_factor: 2.0,
            screen_width: 1440,
            screen_height: 900,
            outer_width: 1440,
            outer_height: 900,
            max_touch_points: 0,
            touch_enabled: false,
        };
        let windows = EnvironmentProfile {
            device_scale_factor: 1.25,
            ..mac
        };

        assert_eq!(mac.launch_window_arg(), "--window-size=1440,900");
        assert_eq!(mac.launch_scale_arg(), "--force-device-scale-factor=2");
        assert_eq!(
            windows.launch_scale_arg(),
            "--force-device-scale-factor=1.25"
        );
    }
}
