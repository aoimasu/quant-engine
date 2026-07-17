//! Read-only vintage load + in-place rollover (QE-219).
//!
//! The runtime loads a **sealed** vintage (ensemble repo + calibration profile) **read-only** at startup and,
//! when training emits a new vintage, **rolls over** to it in place. [`ActiveVintage`] is the holder: it owns
//! the current [`Vintage`] and a [`RolloverRecord`] history, hands out read-only views (what the QE-207
//! evaluator session and the calibration breaker read), and swaps the vintage **atomically** — verify the new
//! vintage *before* committing, so a bad one never becomes active and `repo`+`calibration` never come from
//! two different vintages.
//!
//! Only [`VintageRepository::load`] (open + hash-verify, never write) is used on the load/rollover path, so
//! the runtime never mutates the repository — the trainer (QE-129) is the sole writer. `qe-determinism`
//! ([`Lineage`]) is cross-cutting (QE-006), not on either side of the QE-132 firewall; recording lineage here
//! introduces no forbidden edge.

use qe_determinism::Lineage;
use qe_risk::CalibrationProfile;
use qe_vintage::{Vintage, VintageError, VintageRepository};

/// One recorded vintage transition: which vintage replaced which, with both endpoints' lineage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RolloverRecord {
    /// The vintage id that was active before the rollover.
    pub from_vintage_id: String,
    /// The vintage id that became active.
    pub to_vintage_id: String,
    /// The lineage of the outgoing vintage.
    pub from_lineage: Lineage,
    /// The lineage of the incoming vintage.
    pub to_lineage: Lineage,
}

/// The runtime's active vintage: a read-only holder over the current sealed [`Vintage`], with an in-place,
/// atomic [`rollover`](ActiveVintage::rollover) and a recorded lineage history.
pub struct ActiveVintage {
    current: Vintage,
    history: Vec<RolloverRecord>,
}

impl ActiveVintage {
    /// Load the vintage `vintage_id` from `repo` **read-only** at startup. The repository `load` opens the
    /// artefact for reading and **verifies its content hash** before returning — a load never yields an
    /// unverified vintage and never writes.
    ///
    /// # Errors
    /// [`VintageError::Io`] if the artefact is missing/unreadable, or [`VintageError::HashMismatch`] /
    /// deserialise errors from [`Vintage::load`].
    pub fn load(repo: &VintageRepository, vintage_id: &str) -> Result<Self, VintageError> {
        let current = repo.load(vintage_id)?;
        Ok(Self {
            current,
            history: Vec::new(),
        })
    }

    /// Hold an already-in-hand sealed vintage (verifying its hash first). For in-process wiring and tests.
    ///
    /// # Errors
    /// [`VintageError::HashMismatch`] if `vintage` does not verify.
    pub fn from_vintage(vintage: Vintage) -> Result<Self, VintageError> {
        vintage.verify()?;
        Ok(Self {
            current: vintage,
            history: Vec::new(),
        })
    }

    /// The current sealed vintage (read-only) — the ensemble repo + calibration the runtime consumes.
    #[must_use]
    pub fn current(&self) -> &Vintage {
        &self.current
    }

    /// The current vintage id.
    #[must_use]
    pub fn vintage_id(&self) -> &str {
        &self.current.content.vintage_id
    }

    /// The current per-vintage calibration profile (read-only).
    #[must_use]
    pub fn calibration(&self) -> &CalibrationProfile {
        &self.current.content.calibration
    }

    /// The current vintage's lineage (read-only).
    #[must_use]
    pub fn lineage(&self) -> &Lineage {
        &self.current.content.lineage
    }

    /// The recorded rollover history, oldest first.
    #[must_use]
    pub fn history(&self) -> &[RolloverRecord] {
        &self.history
    }

    /// Roll over to `next` **in place**, atomically. `next` is **verified before** anything is swapped, so on
    /// failure the current vintage and history are left untouched (a bad vintage never becomes active — no
    /// torn repo/calibration state). On success the transition is recorded (both endpoints' `vintage_id` +
    /// `lineage`) and `current` is replaced. Returns the recorded transition.
    ///
    /// This is the **single safety boundary**: it verifies `next` *unconditionally* — regardless of provenance
    /// — so the swap is safe whether `next` came in-hand or from [`rollover_from`] (which already loaded a
    /// verified vintage; the second verify is deliberate defence-in-depth on a rare path, not an oversight).
    ///
    /// No monotonic/same-id guard is imposed: any *verified* vintage may become active, which preserves
    /// legitimate **rollback** to a known-good vintage and re-emission of a rebuilt vintage under the same id
    /// (a real transition — its content hash differs — so it is honestly recorded, not suppressed).
    ///
    /// # Errors
    /// [`VintageError::HashMismatch`] (or a deserialise error) if `next` does not verify; the current vintage
    /// is unchanged.
    pub fn rollover(&mut self, next: Vintage) -> Result<&RolloverRecord, VintageError> {
        next.verify()?;
        let record = RolloverRecord {
            from_vintage_id: self.current.content.vintage_id.clone(),
            to_vintage_id: next.content.vintage_id.clone(),
            from_lineage: self.current.content.lineage.clone(),
            to_lineage: next.content.lineage.clone(),
        };
        self.current = next;
        self.history.push(record);
        Ok(self.history.last().expect("a record was just pushed"))
    }

    /// Load the vintage `next_id` the trainer emitted from `repo` (read-only, hash-verified) and
    /// [`rollover`](ActiveVintage::rollover) to it — the periodic in-place replacement path.
    ///
    /// # Errors
    /// The [`load`](ActiveVintage::load) errors if `next_id` is missing/unverifiable; the current vintage is
    /// unchanged on any failure.
    pub fn rollover_from(
        &mut self,
        repo: &VintageRepository,
        next_id: &str,
    ) -> Result<&RolloverRecord, VintageError> {
        let next = repo.load(next_id)?;
        self.rollover(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_determinism::Lineage;
    use qe_risk::{CalibrationProfile, Fraction};
    use qe_signal::{
        Clause, ExitParams, Genome, RiskParams, RuleSet, CLAUSES_PER_SET, REP_VERSION,
    };
    use qe_vintage::{VintageContent, VINTAGE_FORMAT_VERSION};
    use rust_decimal::Decimal;

    fn genome(hold: u16) -> Genome {
        let off = Clause {
            enabled: false,
            feature: 0,
            lo: 0,
            hi: 0,
        };
        let mut clauses = [off; CLAUSES_PER_SET];
        clauses[0] = Clause {
            enabled: true,
            feature: 0,
            lo: 1,
            hi: 2,
        };
        Genome {
            version: REP_VERSION,
            long_entry: RuleSet {
                clauses,
                min_satisfied: 1,
            },
            short_entry: RuleSet {
                clauses: [off; CLAUSES_PER_SET],
                min_satisfied: 1,
            },
            exit: ExitParams {
                max_holding_bars: hold,
                exit_on_opposite: false,
            },
            risk: RiskParams { size_bps: 5_000 },
        }
    }

    /// A sealed vintage `id` with a distinct calibration + lineage (so transitions are observable).
    fn vintage(id: &str, calib_tenths: i64, seed: u64) -> Vintage {
        let content = VintageContent {
            format_version: VINTAGE_FORMAT_VERSION,
            vintage_id: id.to_owned(),
            chromosomes: vec![genome(10), genome(25)],
            weights: vec![0.6, 0.4],
            calibration: CalibrationProfile::new(
                Fraction::new(Decimal::new(calib_tenths, 1)).unwrap(),
            ),
            slippage: qe_risk::SlippageCalibration::default(),
            sizer: qe_risk::PortfolioSizer::default(),
            worst_case_loss: Some(0.28),
            catalogue: qe_signal::CatalogueIdentity::current(),
            lineage: Lineage::new(
                format!("cfg-{id}"),
                format!("snap-{id}"),
                format!("commit-{id}"),
                vec![seed],
            ),
        };
        Vintage::seal(content).unwrap()
    }

    /// A unique temp repository for a test (mirrors the qe-vintage disk-test convention — no tempfile dep).
    fn temp_repo(tag: &str) -> (VintageRepository, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("qe-219-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        (VintageRepository::new(&dir), dir)
    }

    /// AC: startup loads a vintage read-only — the artefact is unchanged on disk and nothing is journalled.
    #[test]
    fn startup_loads_vintage_read_only() {
        let (repo, dir) = temp_repo("startup");
        let v1 = vintage("2024-06", 2, 7);
        let path = repo.write(&v1).unwrap();
        let before = std::fs::read(&path).unwrap();

        let active = ActiveVintage::load(&repo, "2024-06").unwrap();
        assert_eq!(active.current(), &v1);
        assert_eq!(active.vintage_id(), "2024-06");
        assert_eq!(active.calibration(), &v1.content.calibration);
        assert_eq!(active.lineage(), &v1.content.lineage);
        assert!(active.history().is_empty(), "a load records no rollover");

        // Read-only: the on-disk artefact is byte-for-byte unchanged.
        let after = std::fs::read(&path).unwrap();
        assert_eq!(before, after, "loading must not mutate the repository");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// AC: a rollover swaps the vintage (repo + calibration together) in place, recording the lineage.
    #[test]
    fn rollover_swaps_in_place_with_lineage_recorded() {
        let v1 = vintage("2024-06", 2, 7);
        let v2 = vintage("2024-07", 3, 11);
        let mut active = ActiveVintage::from_vintage(v1.clone()).unwrap();

        let record = active.rollover(v2.clone()).unwrap().clone();

        assert_eq!(active.current(), &v2, "current is now the new vintage");
        assert_eq!(
            active.calibration(),
            &v2.content.calibration,
            "calibration swapped with the repo, indivisibly"
        );
        assert_eq!(record.from_vintage_id, "2024-06");
        assert_eq!(record.to_vintage_id, "2024-07");
        assert_eq!(record.from_lineage, v1.content.lineage);
        assert_eq!(record.to_lineage, v2.content.lineage);
        assert_eq!(active.history().len(), 1);
    }

    /// Atomicity: a rollover to an unverifiable vintage is rejected and leaves the current vintage intact.
    #[test]
    fn rollover_rejects_unverified_vintage_keeping_current() {
        let v1 = vintage("2024-06", 2, 7);
        let mut active = ActiveVintage::from_vintage(v1.clone()).unwrap();

        // A tampered vintage: mutate the content after sealing so the stored hash no longer matches.
        let mut tampered = vintage("2024-07", 3, 11);
        tampered.content.weights[0] = 0.99;

        let err = active.rollover(tampered).unwrap_err();
        assert!(matches!(err, VintageError::HashMismatch { .. }));

        // Unchanged: still v1, no partial swap, nothing recorded.
        assert_eq!(active.current(), &v1);
        assert!(active.history().is_empty());
    }

    /// The real path: the trainer writes a new vintage to the repo; the runtime rolls over by loading it.
    #[test]
    fn rollover_from_repo_loads_and_swaps() {
        let (repo, dir) = temp_repo("rollover-from");
        let v1 = vintage("2024-06", 2, 7);
        let v2 = vintage("2024-07", 3, 11);
        repo.write(&v1).unwrap();
        repo.write(&v2).unwrap();

        let mut active = ActiveVintage::load(&repo, "2024-06").unwrap();
        let record = active.rollover_from(&repo, "2024-07").unwrap().clone();

        assert_eq!(active.current(), &v2);
        assert_eq!(record.from_vintage_id, "2024-06");
        assert_eq!(record.to_vintage_id, "2024-07");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A rollover chain records the full lineage history, in order.
    #[test]
    fn rollover_chain_records_full_lineage() {
        let v1 = vintage("2024-06", 2, 7);
        let v2 = vintage("2024-07", 3, 11);
        let v3 = vintage("2024-08", 4, 13);
        let mut active = ActiveVintage::from_vintage(v1).unwrap();
        active.rollover(v2).unwrap();
        active.rollover(v3).unwrap();

        let hist = active.history();
        assert_eq!(hist.len(), 2);
        assert_eq!(hist[0].from_vintage_id, "2024-06");
        assert_eq!(hist[0].to_vintage_id, "2024-07");
        assert_eq!(hist[1].from_vintage_id, "2024-07");
        assert_eq!(hist[1].to_vintage_id, "2024-08");
        assert_eq!(active.vintage_id(), "2024-08");
    }

    /// Loading a missing vintage is a clean error, not a panic.
    #[test]
    fn load_missing_vintage_errors() {
        let (repo, dir) = temp_repo("missing");
        assert!(matches!(
            ActiveVintage::load(&repo, "absent"),
            Err(VintageError::Io(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
