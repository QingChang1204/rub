//! L2 behavioral humanization — simulate human-like mouse movement,
//! typing rhythm, and scroll patterns to evade behavioral detection.
//!
//! Enabled via `--humanize` CLI flag. Runs on top of L1 stealth baseline.

/// Speed presets for humanized interactions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HumanizeSpeed {
    /// Faster interactions (30-80ms typing, quick mouse)
    Fast,
    /// Normal human pace (50-150ms typing, natural mouse)
    #[default]
    Normal,
    /// Deliberate pace (100-300ms typing, slow mouse)
    Slow,
}

impl HumanizeSpeed {
    /// Parse from CLI string value.
    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "fast" => Some(Self::Fast),
            "normal" => Some(Self::Normal),
            "slow" => Some(Self::Slow),
            _ => None,
        }
    }

    /// Typing delay range in milliseconds (min, max).
    pub fn typing_delay_range(self) -> (u64, u64) {
        match self {
            Self::Fast => (30, 80),
            Self::Normal => (50, 150),
            Self::Slow => (100, 300),
        }
    }

    /// Mouse movement duration in milliseconds.
    pub fn mouse_move_duration(self) -> u64 {
        match self {
            Self::Fast => 100,
            Self::Normal => 250,
            Self::Slow => 500,
        }
    }

    /// Number of steps for mouse movement interpolation.
    pub fn mouse_move_steps(self) -> u32 {
        match self {
            Self::Fast => 8,
            Self::Normal => 15,
            Self::Slow => 25,
        }
    }

    /// Scroll step delay range in milliseconds (min, max).
    pub fn scroll_delay_range(self) -> (u64, u64) {
        match self {
            Self::Fast => (20, 50),
            Self::Normal => (40, 100),
            Self::Slow => (80, 200),
        }
    }
}

/// Runtime humanize configuration threaded from CLI to daemon.
#[derive(Debug, Clone)]
pub struct HumanizeConfig {
    pub enabled: bool,
    pub speed: HumanizeSpeed,
}

impl Default for HumanizeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            speed: HumanizeSpeed::Normal,
        }
    }
}

/// Cheap deterministic-enough delay helper shared by humanized actuation paths.
pub fn random_delay(min: u64, max: u64) -> u64 {
    if min >= max {
        return min;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64;
    min + (nanos % (max - min + 1))
}

/// Generate a cubic Bézier curve path from start to end with natural-looking
/// control points. Returns a sequence of (x, y) points.
pub fn bezier_mouse_path(
    start_x: f64,
    start_y: f64,
    end_x: f64,
    end_y: f64,
    steps: u32,
) -> Vec<(f64, f64)> {
    let mut points = Vec::with_capacity(steps as usize + 1);

    // Control points: offset from straight line for natural curve
    let dx = end_x - start_x;
    let dy = end_y - start_y;

    // Add slight randomized curvature via control points
    let cp1_x = start_x + dx * 0.25 + dy * 0.1;
    let cp1_y = start_y + dy * 0.25 - dx * 0.05;
    let cp2_x = start_x + dx * 0.75 - dy * 0.1;
    let cp2_y = start_y + dy * 0.75 + dx * 0.05;

    for i in 0..=steps {
        let t = i as f64 / steps as f64;
        let t2 = t * t;
        let t3 = t2 * t;
        let mt = 1.0 - t;
        let mt2 = mt * mt;
        let mt3 = mt2 * mt;

        let x = mt3 * start_x + 3.0 * mt2 * t * cp1_x + 3.0 * mt * t2 * cp2_x + t3 * end_x;
        let y = mt3 * start_y + 3.0 * mt2 * t * cp1_y + 3.0 * mt * t2 * cp2_y + t3 * end_y;
        points.push((x, y));
    }

    points
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speed_from_str() {
        assert_eq!(
            HumanizeSpeed::from_str_opt("fast"),
            Some(HumanizeSpeed::Fast)
        );
        assert_eq!(
            HumanizeSpeed::from_str_opt("NORMAL"),
            Some(HumanizeSpeed::Normal)
        );
        assert_eq!(
            HumanizeSpeed::from_str_opt("slow"),
            Some(HumanizeSpeed::Slow)
        );
        assert_eq!(HumanizeSpeed::from_str_opt("invalid"), None);
    }

    #[test]
    fn bezier_path_starts_and_ends_at_endpoints() {
        let path = bezier_mouse_path(0.0, 0.0, 100.0, 200.0, 10);
        assert_eq!(path.len(), 11);
        let (sx, sy) = path[0];
        let (ex, ey) = path[10];
        assert!((sx - 0.0).abs() < 0.01);
        assert!((sy - 0.0).abs() < 0.01);
        assert!((ex - 100.0).abs() < 0.01);
        assert!((ey - 200.0).abs() < 0.01);
    }

    #[test]
    fn typing_delay_ranges_are_ordered() {
        let (fmin, fmax) = HumanizeSpeed::Fast.typing_delay_range();
        let (nmin, nmax) = HumanizeSpeed::Normal.typing_delay_range();
        let (smin, smax) = HumanizeSpeed::Slow.typing_delay_range();
        assert!(fmin < nmin);
        assert!(nmin < smin);
        assert!(fmax < nmax);
        assert!(nmax < smax);
    }
}
