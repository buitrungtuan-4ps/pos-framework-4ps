use std::collections::BTreeMap;
use std::sync::Mutex;

use pos_cloud_reports::types::DailyRollup;

use super::{
    FederationSection, FederationSource, OUT_OF_TIME, RollupFetcher, RollupWindow, SourceKey,
    federate,
};
use crate::cell::directory::tests::vn1;
use crate::cell::signed::{CellDirectory, parse_public_key};
use crate::cell::testkit::{V3, public_key_line};

const STORE_A: &str = "01M2219QK4T3W6Z0Y8FBQ2X5MV";
const STORE_B: &str = "01M22190WCY5PS7KCA7ET7H679";

fn source(cell_id: &str, stores: &[&str]) -> FederationSource {
    FederationSource {
        cell_id: cell_id.to_owned(),
        api_key: SourceKey::new("pos_k1_secret"),
        stores: stores.iter().map(|store| (*store).to_owned()).collect(),
    }
}

fn section(sources: Vec<FederationSource>) -> FederationSection {
    FederationSection { sources }
}

fn day(date: &str, total: u64) -> DailyRollup {
    DailyRollup {
        business_date: date.to_owned(),
        total_events: total,
        by_type: BTreeMap::from([("sales.order.created".to_owned(), total)]),
    }
}

/// Answers from a script, and records what it was asked.
#[derive(Default)]
struct ScriptedFetcher {
    asked: Mutex<Vec<(String, String, String, RollupWindow)>>,
}

impl RollupFetcher for ScriptedFetcher {
    async fn fetch(
        &self,
        base_url: &str,
        store_id: &str,
        key: &SourceKey,
        window: &RollupWindow,
    ) -> Result<Vec<DailyRollup>, String> {
        if let Ok(mut asked) = self.asked.lock() {
            asked.push((
                base_url.to_owned(),
                store_id.to_owned(),
                key.expose().to_owned(),
                window.clone(),
            ));
        }
        if store_id == STORE_B {
            Err("jp1 answered 403: the key lacks read_rollups".to_owned())
        } else {
            Ok(vec![day("2026-10-26", 41), day("2026-10-27", 12)])
        }
    }
}

fn far() -> tokio::time::Instant {
    tokio::time::Instant::now() + std::time::Duration::from_secs(60)
}

fn holding_directory() -> CellDirectory {
    let directory = CellDirectory::trusting(
        vn1(),
        parse_public_key(public_key_line()).expect("the key parses"),
    );
    assert!(directory.offer(V3.document, V3.minisig).is_ok());
    directory
}

#[tokio::test]
async fn every_store_answers_and_one_failure_never_fails_the_others() {
    let fetcher = ScriptedFetcher::default();
    let window = RollupWindow {
        from: Some("2026-10-26".to_owned()),
        to: None,
    };
    let answers = federate(
        &holding_directory(),
        &section(vec![source("jp1", &[STORE_A, STORE_B])]),
        &fetcher,
        &window,
        far(),
    )
    .await;
    assert_eq!(answers.len(), 2);
    assert_eq!(answers[0].cell_id, "jp1");
    assert_eq!(answers[0].store_id, STORE_A);
    assert_eq!(answers[0].rollups.as_ref().map(Vec::len), Some(2));
    assert_eq!(answers[0].error, None);
    assert_eq!(answers[1].rollups, None);
    assert!(
        answers[1]
            .error
            .as_deref()
            .is_some_and(|why| why.contains("read_rollups"))
    );

    let asked = fetcher.asked.lock().expect("lock").clone();
    assert_eq!(asked.len(), 2);
    assert_eq!(
        asked[0].0, "https://jp1.example.com",
        "the base URL is the directory's"
    );
    assert_eq!(asked[0].2, "pos_k1_secret");
    assert_eq!(asked[0].3, window, "the window is passed on");
}

#[tokio::test]
async fn a_cell_the_directory_does_not_list_is_an_answer_not_a_call() {
    let fetcher = ScriptedFetcher::default();
    let answers = federate(
        &holding_directory(),
        &section(vec![source("sg1", &[STORE_A])]),
        &fetcher,
        &RollupWindow::default(),
        far(),
    )
    .await;
    assert_eq!(answers.len(), 1);
    assert!(
        answers[0]
            .error
            .as_deref()
            .is_some_and(|why| why.contains("does not list the cell sg1"))
    );
    assert!(
        fetcher.asked.lock().expect("lock").is_empty(),
        "nothing was called"
    );

    // A cell holding no directory knows no other cell at all.
    let waiting = CellDirectory::trusting(
        vn1(),
        parse_public_key(public_key_line()).expect("the key parses"),
    );
    let answers = federate(
        &waiting,
        &section(vec![source("jp1", &[STORE_A])]),
        &fetcher,
        &RollupWindow::default(),
        far(),
    )
    .await;
    assert!(answers[0].error.is_some());
}

#[test]
fn the_key_is_never_printed() {
    let printed = format!("{:?}", section(vec![source("jp1", &[STORE_A])]));
    assert!(!printed.contains("pos_k1_secret"), "{printed}");
    assert!(printed.contains("SourceKey(redacted)"));
}

#[test]
fn sources_are_checked() {
    assert!(
        section(vec![]).validate().is_ok(),
        "no sources is no federation"
    );
    assert!(section(vec![source("jp1", &[STORE_A])]).validate().is_ok());

    let cases: Vec<(FederationSource, &str)> = vec![
        (source("JP", &[STORE_A]), "cell_id"),
        (source("jp1", &[]), "1 to 1000"),
        (source("jp1", &["not-a-ulid"]), "not a ULID"),
        (
            FederationSource {
                api_key: SourceKey::new("   "),
                ..source("jp1", &[STORE_A])
            },
            "no api_key",
        ),
    ];
    for (bad, why) in cases {
        let error = section(vec![bad]).validate().expect_err("refused");
        assert!(error.contains(why), "{error}");
        assert!(error.contains("[federation]"), "{error}");
    }

    let twice = section(vec![source("jp1", &[STORE_A]), source("jp1", &[STORE_B])]);
    assert!(twice.validate().expect_err("refused").contains("twice"));

    let too_many: Vec<FederationSource> = (0..65)
        .map(|n| source(&format!("c{n:03}"), &[STORE_A]))
        .collect();
    assert!(
        section(too_many)
            .validate()
            .expect_err("refused")
            .contains("at most 64")
    );

    let many_stores = vec![STORE_A; 1_001];
    assert!(
        section(vec![source("jp1", &many_stores)])
            .validate()
            .expect_err("refused")
            .contains("1 to 1000")
    );
}

#[test]
fn the_section_parses_and_refuses_unknown_keys() {
    let parsed: FederationSection = toml::from_str(&format!(
        "[[sources]]\ncell_id = \"jp1\"\napi_key = \"pos_k1_secret\"\nstores = [\"{STORE_A}\"]\n"
    ))
    .expect("parses");
    assert_eq!(parsed.sources.len(), 1);
    assert!(
        toml::from_str::<FederationSection>(
            "[[sources]]\ncell_id = \"jp1\"\napi_key = \"k\"\nstores = []\nbase_url = \"https://x\"\n"
        )
        .is_err(),
        "a base URL is the directory's to give, never configuration's"
    );
}

/// Only aggregates cross a cell boundary. A rollup is a business date, a total and counts by event
/// type; this pins exactly those fields, so a field added to `DailyRollup`, person-level or not,
/// fails here until somebody decides it may cross a border (ADR-0179 C10; cross-border personal data
/// needs a legal basis this code cannot have).
#[test]
fn a_rollup_that_crosses_cells_carries_aggregates_only() {
    let value = serde_json::to_value(day("2026-10-27", 3)).expect("serialises");
    let keys: Vec<&str> = value
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, ["business_date", "by_type", "total_events"]);
    assert!(
        value["by_type"]
            .as_object()
            .expect("counts")
            .values()
            .all(serde_json::Value::is_u64),
        "by_type holds counts and nothing else"
    );
}

#[test]
fn the_total_number_of_stores_is_bounded() {
    let stores = vec![STORE_A; 1_000];
    let sources: Vec<FederationSource> = (0..3)
        .map(|n| source(&format!("c{n:03}"), &stores))
        .collect();
    assert!(
        section(sources)
            .validate()
            .expect_err("refused")
            .contains("at most 2000")
    );
}

#[tokio::test]
async fn stores_not_reached_by_the_deadline_say_so_and_are_not_called() {
    let fetcher = ScriptedFetcher::default();
    let answers = federate(
        &holding_directory(),
        &section(vec![source("jp1", &[STORE_A, STORE_A])]),
        &fetcher,
        &RollupWindow::default(),
        tokio::time::Instant::now(),
    )
    .await;
    assert_eq!(answers.len(), 2, "every configured store still answers");
    assert!(
        answers
            .iter()
            .all(|answer| answer.error.as_deref() == Some(OUT_OF_TIME) && answer.rollups.is_none())
    );
    assert!(
        fetcher.asked.lock().expect("lock").is_empty(),
        "nothing was called"
    );
}

/// Never answers.
struct Silent;

impl RollupFetcher for Silent {
    async fn fetch(
        &self,
        _base_url: &str,
        _store_id: &str,
        _key: &SourceKey,
        _window: &RollupWindow,
    ) -> Result<Vec<DailyRollup>, String> {
        std::future::pending().await
    }
}

#[tokio::test]
async fn a_cell_that_never_answers_cannot_hold_the_read_past_its_deadline() {
    let answers = federate(
        &holding_directory(),
        &section(vec![source("jp1", &[STORE_A, STORE_B])]),
        &Silent,
        &RollupWindow::default(),
        tokio::time::Instant::now() + std::time::Duration::from_millis(50),
    )
    .await;
    assert_eq!(answers.len(), 2);
    assert!(
        answers
            .iter()
            .all(|answer| answer.error.as_deref() == Some(OUT_OF_TIME))
    );
}
