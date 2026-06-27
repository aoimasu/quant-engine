//! QE-101 acceptance, end-to-end over the public API: enumerate point-in-time targets, download
//! them checksum-verified, and prove a re-run fetches nothing already present + verified (AC #1);
//! plus schema-drift detection across months.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{Cursor, Write};
use std::rc::Rc;

use qe_config::{universe::parse_iso_date, InstrumentListing, Universe};
use qe_domain::{InstrumentId, Resolution};
use qe_ingest::checksum::sha256_hex;
use qe_ingest::{
    csv_header, enumerate_targets, DataKind, Downloader, DriftStatus, FetchError, Fetcher,
    RawCache, SchemaRegistry, YearMonth,
};
use zip::write::SimpleFileOptions;

const BASE: &str = "https://data.binance.vision";

/// In-memory fetcher serving a fixed url→bytes map; total fetches are counted through a shared
/// handle the test keeps, so we can assert a cached re-run hits the network zero times.
struct MapFetcher {
    responses: HashMap<String, Vec<u8>>,
    total_hits: Rc<RefCell<usize>>,
}

impl Fetcher for MapFetcher {
    fn get(&self, url: &str) -> Result<Vec<u8>, FetchError> {
        *self.total_hits.borrow_mut() += 1;
        self.responses
            .get(url)
            .cloned()
            .ok_or_else(|| FetchError::NotFound(url.to_owned()))
    }
}

fn inst(s: &str) -> InstrumentId {
    InstrumentId::new(s).unwrap()
}
fn ym(y: i32, m: u32) -> YearMonth {
    YearMonth { year: y, month: m }
}

#[test]
fn enumerate_download_and_rerun_is_idempotent() {
    // BTC open-ended; ETH listed mid-window — point-in-time membership is honoured by the plan.
    let universe = Universe::new(vec![
        InstrumentListing::open_ended(inst("BTCUSDT")),
        InstrumentListing::new(inst("ETHUSDT"), parse_iso_date("2020-02-01").unwrap(), None)
            .unwrap(),
    ]);
    let targets = enumerate_targets(
        &universe,
        &[DataKind::Klines(Resolution::M5), DataKind::FundingRate],
        ym(2020, 1),
        ym(2020, 3),
    );
    // BTC: 3 months × 2 kinds = 6; ETH: Feb+Mar × 2 kinds = 4. Total 10.
    assert_eq!(targets.len(), 10);

    // Serve every target with deterministic bytes + correct checksum sidecar.
    let mut responses = HashMap::new();
    for (i, f) in targets.iter().enumerate() {
        let bytes = format!("payload-{i}").into_bytes();
        responses.insert(
            f.checksum_url(BASE),
            format!("{}  x.zip", sha256_hex(&bytes)).into_bytes(),
        );
        responses.insert(f.url(BASE), bytes);
    }
    let hits = Rc::new(RefCell::new(0usize));
    let fetcher = MapFetcher {
        responses,
        total_hits: Rc::clone(&hits),
    };

    let tmp = tempfile::tempdir().unwrap();
    let dl = Downloader::new(fetcher, RawCache::new(tmp.path()), BASE);

    // First sync: everything fetched, nothing failed.
    let r1 = dl.sync_all(&targets);
    assert_eq!(r1.fetched, 10);
    assert_eq!(r1.skipped, 0);
    assert!(r1.failed.is_empty(), "{:?}", r1.failed);

    // Second sync (AC #1): all skipped, and not a single new fetch is issued.
    let hits_before_rerun = *hits.borrow();
    let r2 = dl.sync_all(&targets);
    assert_eq!(r2.skipped, 10);
    assert_eq!(r2.fetched, 0);
    assert_eq!(
        *hits.borrow(),
        hits_before_rerun,
        "a fully-cached re-run must hit the network zero times"
    );
}

#[test]
fn schema_drift_is_detected_across_months() {
    let zip = |content: &str| {
        let mut buf = Vec::new();
        {
            let mut w = zip::ZipWriter::new(Cursor::new(&mut buf));
            w.start_file("data.csv", SimpleFileOptions::default())
                .unwrap();
            w.write_all(content.as_bytes()).unwrap();
            w.finish().unwrap();
        }
        buf
    };

    let mut reg = SchemaRegistry::new();
    let kind = DataKind::Klines(Resolution::M5);

    let jan = csv_header(&zip("open_time,open,high,low,close,volume\n1,2,3,4,5,6\n")).unwrap();
    assert_eq!(reg.check(kind, &jan), DriftStatus::InSync); // baseline

    let feb = csv_header(&zip(
        "open_time,open,high,low,close,volume\n7,8,9,10,11,12\n",
    ))
    .unwrap();
    assert_eq!(reg.check(kind, &feb), DriftStatus::InSync); // same schema

    // March adds a column → drift flagged.
    let mar = csv_header(&zip(
        "open_time,open,high,low,close,volume,extra\n1,2,3,4,5,6,7\n",
    ))
    .unwrap();
    match reg.check(kind, &mar) {
        DriftStatus::Drift { added, .. } => assert_eq!(added, vec!["extra".to_owned()]),
        other => panic!("expected drift, got {other:?}"),
    }
}
