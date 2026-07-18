//! Regenerates the committed sealed-vintage fixture (`tests/fixtures/sample_vintage.json`) via the
//! **real seal path** ([`Vintage::seal`]) — never by hand-editing the JSON/hash. Run once after a
//! `VINTAGE_FORMAT_VERSION` bump, eyeball the diff, and commit:
//!
//! `cargo test -p qe-server --test regenerate_fixture regenerate_sample_vintage -- --ignored --exact`
//!
//! The content mirrors the historical fixture (one feature-0 genome, unit weight, the QE-130 stress
//! figure) plus the QE-467 seal-evidence / holdout-series / provenance blocks at their defaults, so the
//! server read/audit tests keep asserting the same structural shape against a validly re-sealed artefact.

use std::path::Path;

use qe_determinism::Lineage;
use qe_risk::{CalibrationProfile, Fraction, PortfolioSizer, ShockConfig, SlippageCalibration};
use qe_signal::{
    CatalogueIdentity, Clause, ExitParams, Genome, RiskParams, RuleSet, CLAUSES_PER_SET,
    REP_VERSION,
};
use qe_vintage::{
    HoldoutReturnSeries, ResearchProvenance, SealEvidence, Vintage, VintageContent,
    VintageRepository, VINTAGE_FORMAT_VERSION,
};
use rust_decimal::Decimal;

fn fixture_genome() -> Genome {
    let off = Clause {
        enabled: false,
        feature: 0,
        lo: 0,
        hi: 0,
    };
    let mut long = [off; CLAUSES_PER_SET];
    long[0] = Clause {
        enabled: true,
        feature: 0,
        lo: 3,
        hi: 4,
    };
    let mut short = [off; CLAUSES_PER_SET];
    short[0] = Clause {
        enabled: true,
        feature: 0,
        lo: 0,
        hi: 1,
    };
    Genome {
        version: REP_VERSION,
        long_entry: RuleSet {
            clauses: long,
            min_satisfied: 1,
        },
        short_entry: RuleSet {
            clauses: short,
            min_satisfied: 1,
        },
        exit: ExitParams {
            max_holding_bars: 3,
            exit_on_opposite: true,
        },
        risk: RiskParams { size_bps: 5_000 },
    }
}

#[test]
#[ignore = "regenerates the committed sealed-vintage fixture; run manually after a format bump"]
fn regenerate_sample_vintage() {
    let content = VintageContent {
        format_version: VINTAGE_FORMAT_VERSION,
        vintage_id: "sample_vintage".to_owned(),
        chromosomes: vec![fixture_genome()],
        weights: vec![1.0],
        calibration: CalibrationProfile::new(Fraction::new(Decimal::new(1, 1)).unwrap()),
        slippage: SlippageCalibration::default(),
        sizer: PortfolioSizer::default(),
        shocks: ShockConfig::default(),
        worst_case_loss: Some(0.1),
        catalogue: CatalogueIdentity::current(),
        lineage: Lineage::new(
            "fixture-config-hash",
            "fixture-snapshot",
            "fixture-commit",
            vec![42],
        ),
        // QE-467: the persistence-foundation blocks. Defaults here (the fixture is a read-target, not a
        // real train output); the real seal path (`qe-cli::jobs::train`) populates them for true vintages.
        seal_evidence: SealEvidence::default(),
        holdout_series: HoldoutReturnSeries::default(),
        provenance: ResearchProvenance::default(),
    };
    let vintage = Vintage::seal(content).expect("fixture content seals");
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let path = VintageRepository::new(&dir)
        .write(&vintage)
        .expect("write fixture");
    eprintln!("regenerated {}", path.display());
}
