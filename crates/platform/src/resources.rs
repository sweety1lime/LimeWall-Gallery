//! CPU sampling for the wallpaper process tree — this process plus its
//! descendants (the WebView2 children that render web / 3D wallpapers). The
//! renderer's resource watchdog uses it to catch a runaway or hostile
//! wallpaper that pegs the CPU behind the desktop.
//!
//! The raw percentage is reported; the *policy* (budget, hysteresis, what to
//! pause) lives in the renderer, not here.

/// Samples the CPU used by the wallpaper stack across successive calls.
pub struct StackSampler {
    #[cfg(windows)]
    inner: crate::resources_win32::Win32StackSampler,
}

impl StackSampler {
    pub fn new() -> Self {
        Self {
            #[cfg(windows)]
            inner: crate::resources_win32::Win32StackSampler::new(),
        }
    }

    /// CPU used by this process and its descendants since the previous call,
    /// as a percentage of total machine capacity (0..=100, may briefly exceed
    /// under scheduling jitter). Returns `None` on the first call — there is no
    /// interval to measure yet — and on platforms without a backend.
    pub fn sample(&mut self) -> Option<f32> {
        #[cfg(windows)]
        {
            self.inner.sample()
        }
        #[cfg(not(windows))]
        {
            None
        }
    }
}

impl Default for StackSampler {
    fn default() -> Self {
        Self::new()
    }
}

/// Descendant PIDs of `root` given `(pid, parent_pid)` edges. Excludes `root`
/// and is cycle-safe against PID reuse.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn descendants(root: u32, edges: &[(u32, u32)]) -> std::collections::HashSet<u32> {
    use std::collections::{HashMap, HashSet, VecDeque};
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for &(pid, parent) in edges {
        children.entry(parent).or_default().push(pid);
    }
    let mut out = HashSet::new();
    let mut queue = VecDeque::from([root]);
    while let Some(pid) = queue.pop_front() {
        if let Some(kids) = children.get(&pid) {
            for &child in kids {
                if child != root && out.insert(child) {
                    queue.push_back(child);
                }
            }
        }
    }
    out
}

/// Busy CPU time over an interval as a percentage of total machine capacity.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn cpu_percent(busy_delta_ns: u128, wall_delta_ns: u128, cores: u32) -> f32 {
    if wall_delta_ns == 0 || cores == 0 {
        return 0.0;
    }
    let capacity = wall_delta_ns.saturating_mul(cores as u128);
    (busy_delta_ns as f64 / capacity as f64 * 100.0) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descendants_walks_the_whole_tree_and_skips_unrelated() {
        // 1 -> 2 -> 4, 1 -> 3, and an unrelated 5 -> 6.
        let edges = [(2, 1), (3, 1), (4, 2), (6, 5)];
        let mut kids: Vec<u32> = descendants(1, &edges).into_iter().collect();
        kids.sort_unstable();
        assert_eq!(kids, vec![2, 3, 4]);
        assert!(descendants(99, &edges).is_empty());
    }

    #[test]
    fn descendants_survive_a_pid_reuse_cycle() {
        // A pathological cycle 1 -> 2 -> 1 must not loop forever.
        let edges = [(2, 1), (1, 2)];
        let kids = descendants(1, &edges);
        assert!(kids.contains(&2));
    }

    #[test]
    fn cpu_percent_scales_by_cores_and_time() {
        // One core fully busy for the whole interval on a 4-core box = 25%.
        assert_eq!(cpu_percent(1_000, 1_000, 4), 25.0);
        // All capacity busy = 100%.
        assert_eq!(cpu_percent(4_000, 1_000, 4), 100.0);
        // Degenerate inputs never divide by zero.
        assert_eq!(cpu_percent(1_000, 0, 4), 0.0);
        assert_eq!(cpu_percent(1_000, 1_000, 0), 0.0);
    }
}
