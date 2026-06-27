//! Walk-forward window manager (QE-117) — anchored/rolling train→validate windows with a purge+embargo
//! gap, the backbone of WFO and continuous adaptation without catastrophic forgetting.
//!
//! Each [`Window`] places a `validate` block a `purge + embargo` gap **after** its `train` block, so the
//! two are leakage-free *including the indicator lookback* — the same invariant QE-113 fixed for k-fold
//! ([`Window::windows_disjoint`]). The manager [`run`](WalkForward::run)s a caller-owned archive through
//! every window without ever resetting it (QE-118 owns the archive internals); the per-window callback
//! displaces degraded entries while retaining the rest.

use std::ops::Range;

/// Train-window shape across successive walk-forward steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowMode {
    /// Fixed-width train window that slides forward by `step` each transition.
    Rolling,
    /// Train window anchored at bar 0 that grows each transition (keeps all history).
    Anchored,
}

/// Walk-forward window configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalkForward {
    /// Anchored vs rolling train geometry.
    pub mode: WindowMode,
    /// Train-window length (bars). For `Anchored` this is the *initial* width; the window grows by `step`.
    pub train: usize,
    /// Validate-window length (bars).
    pub validate: usize,
    /// Bars the train end advances each transition.
    pub step: usize,
    /// Max indicator lookback (bars) — the feature-dependency span (QE-107).
    pub lookback: usize,
    /// Label horizon (bars).
    pub label_horizon: usize,
    /// Embargo (bars) added to the purge gap between train and validate (QE-113).
    pub embargo: usize,
}

/// One walk-forward split: a `train` block and a gapped `validate` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    /// 0-based window index in generation order.
    pub index: usize,
    /// Train bar range `[start, end)`.
    pub train: Range<usize>,
    /// Validate bar range `[start, end)`, starting `purge + embargo` bars after `train.end`.
    pub validate: Range<usize>,
}

impl Window {
    /// Whether every (train, validate) pair has `|tr − val| > lookback + label_horizon` — train and
    /// validate information windows are disjoint *including the lookback* (the QE-113 invariant).
    #[must_use]
    pub fn windows_disjoint(&self, lookback: usize, label_horizon: usize) -> bool {
        let span = lookback + label_horizon;
        self.train
            .clone()
            .all(|tr| self.validate.clone().all(|val| tr.abs_diff(val) > span))
    }
}

impl WalkForward {
    /// The purge gap = `lookback + label_horizon` (QE-113).
    #[must_use]
    pub fn purge(&self) -> usize {
        self.lookback + self.label_horizon
    }

    /// Generate the walk-forward windows over `0..n_bars`. Train ends advance by `step`; `validate`
    /// starts a `purge + embargo` gap after the train end. Generation stops before any partial validate
    /// window (`validate_end > n_bars`).
    #[must_use]
    pub fn windows(&self, n_bars: usize) -> Vec<Window> {
        let step = self.step.max(1);
        let gap = self.purge() + self.embargo;
        let mut out = Vec::new();
        let mut origin = 0usize;
        loop {
            let train_end = origin + self.train;
            let train_start = match self.mode {
                WindowMode::Rolling => origin,
                WindowMode::Anchored => 0,
            };
            let validate_start = train_end + gap;
            let validate_end = validate_start + self.validate;
            if self.train == 0 || self.validate == 0 || validate_end > n_bars {
                break;
            }
            out.push(Window {
                index: out.len(),
                train: train_start..train_end,
                validate: validate_start..validate_end,
            });
            origin += step;
        }
        out
    }

    /// Run a caller-owned `archive` through every window, invoking `on_window(&mut archive, &window)` per
    /// transition. The **same** archive instance is threaded through all windows — the manager never
    /// resets it (QE-117/D3): persistence across transitions is structural.
    pub fn run<A, F>(&self, n_bars: usize, archive: &mut A, mut on_window: F)
    where
        F: FnMut(&mut A, &Window),
    {
        for window in self.windows(n_bars) {
            on_window(archive, &window);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn cfg(mode: WindowMode) -> WalkForward {
        WalkForward {
            mode,
            train: 50,
            validate: 20,
            step: 20,
            lookback: 5,
            label_horizon: 2,
            embargo: 3,
        }
    }

    #[test]
    fn rolling_geometry_and_gap() {
        let wf = cfg(WindowMode::Rolling);
        let w = wf.windows(300);
        assert!(!w.is_empty());
        // First window: train [0,50), gap = purge(7)+embargo(3)=10, validate [60,80).
        assert_eq!(w[0].train, 0..50);
        assert_eq!(w[0].validate, 60..80);
        // Rolling train slides by step=20.
        assert_eq!(w[1].train, 20..70);
        assert_eq!(w[1].validate, 80..100);
        // Index is monotonic.
        assert_eq!(w[1].index, 1);
    }

    #[test]
    fn anchored_train_starts_at_zero_and_grows() {
        let wf = cfg(WindowMode::Anchored);
        let w = wf.windows(300);
        assert_eq!(w[0].train, 0..50);
        assert_eq!(w[1].train, 0..70); // grows (end advances by step), start stays 0
        assert_eq!(w[2].train, 0..90);
        for win in &w {
            assert_eq!(win.train.start, 0);
        }
    }

    #[test]
    fn every_window_is_disjoint_including_lookback() {
        for mode in [WindowMode::Rolling, WindowMode::Anchored] {
            let wf = cfg(mode);
            let windows = wf.windows(300);
            assert!(!windows.is_empty());
            for win in &windows {
                assert!(
                    win.windows_disjoint(wf.lookback, wf.label_horizon),
                    "{mode:?} window {:?} leaks",
                    win.index
                );
            }
        }
    }

    #[test]
    fn a_zero_gap_split_would_leak() {
        // Contrast: with no purge/embargo, validate is adjacent to train → leaks under a real lookback.
        let naive = WalkForward {
            embargo: 0,
            lookback: 0,
            label_horizon: 0,
            ..cfg(WindowMode::Rolling)
        };
        let w = naive.windows(300);
        // train [0,50), validate [50,70): bar 49 is adjacent to bar 50.
        assert_eq!(w[0].train, 0..50);
        assert_eq!(w[0].validate, 50..70);
        assert!(
            !w[0].windows_disjoint(5, 2),
            "zero-gap split must leak under lookback 5"
        );
    }

    #[test]
    fn generation_stops_before_partial_validate() {
        let wf = cfg(WindowMode::Rolling);
        let n = 95; // first validate ends at 80; next would end at 100 > 95.
        let w = wf.windows(n);
        assert_eq!(w.len(), 1);
        assert!(w.iter().all(|win| win.validate.end <= n));
        // Degenerate sizes → no windows.
        assert!(WalkForward { train: 0, ..wf }.windows(300).is_empty());
        assert!(WalkForward { validate: 0, ..wf }.windows(300).is_empty());
    }

    #[test]
    fn archive_persists_across_transitions_displacing_only_degraded() {
        // Toy archive: strategy id → best fitness. The callback re-evaluates a couple of strategies per
        // window and displaces ones that degrade below a floor, while untouched entries persist.
        let wf = WalkForward {
            step: 60,
            ..cfg(WindowMode::Rolling)
        }; // 2 windows over 300 bars
        let windows = wf.windows(300);
        assert!(windows.len() >= 2);

        let mut archive: BTreeMap<u32, f64> = BTreeMap::new();
        archive.insert(1, 0.10); // seeded before the run; never re-evaluated → must persist
        let floor = 0.0;

        wf.run(300, &mut archive, |arch, w| {
            match w.index {
                0 => {
                    arch.insert(2, 0.20); // a strong strategy enters
                    arch.insert(3, 0.05); // a marginal one enters
                }
                1 => {
                    // strategy 3 degrades below the floor → displaced; strategy 2 improves → updated.
                    let degraded = -0.30;
                    if degraded < floor {
                        arch.remove(&3);
                    }
                    arch.insert(2, 0.25);
                }
                _ => {}
            }
        });

        assert_eq!(
            archive.get(&1),
            Some(&0.10),
            "untouched strategy must persist (not reset)"
        );
        assert_eq!(
            archive.get(&2),
            Some(&0.25),
            "improved strategy updated across windows"
        );
        assert_eq!(archive.get(&3), None, "degraded strategy displaced");
        // Not a wholesale reset — survivors from window 0 carried into window 1.
        assert_eq!(archive.len(), 2);
    }
}
