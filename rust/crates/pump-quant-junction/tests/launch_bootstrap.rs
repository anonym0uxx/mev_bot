//! Launch bootstrap adapter. Real transactions come from the 2026-09-09 capture (converted to the RPC
//! `full/json` shape, labelled in the fixture). Everything served through `MockPages` is a MOCKED
//! Helius response: pagination, errors and budgets are exercised without network or credential.

use std::collections::BTreeMap;

use pump_quant_junction::launch_bootstrap::{
    b58, create_in_tx, creator_prior_launches, evidence_from_json, evidence_json, find_launch,
    launch_cached, load_key, parse_full_tx, parse_page, request_body, Budget, CoverageWindow,
    CredError, EvidenceCache, FetchError, LaunchOutcome, MockPages, Page, PriorLaunches,
};
use serde_json::{json, Value};

fn fx() -> Value {
    serde_json::from_str(include_str!("fixtures/launch_bootstrap_capture_txs.json")).unwrap()
}
fn create(i: usize) -> (String, String, Value) {
    let f = fx();
    let c = &f["creates"][i];
    (
        c["mint"].as_str().unwrap().into(),
        c["table_creator"].as_str().unwrap().into(),
        c["tx"].clone(),
    )
}
fn non_create(i: usize) -> (String, Value) {
    let f = fx();
    let c = &f["non_creates"][i];
    (c["mint"].as_str().unwrap().into(), c["tx"].clone())
}
type MockEntry<'a> = ((&'a str, &'a str), Result<Page, FetchError>);
fn mock(pages: Vec<MockEntry<'_>>) -> MockPages {
    MockPages {
        pages: pages
            .into_iter()
            .map(|((a, t), p)| ((a.to_string(), t.to_string()), p))
            .collect::<BTreeMap<_, _>>(),
        calls: Default::default(),
    }
}
fn page(txs: Vec<Value>, next: Option<&str>) -> Result<Page, FetchError> {
    Ok(Page {
        txs,
        next: next.map(str::to_string),
    })
}
const B: Budget = Budget { max_pages: 10 };

#[test]
fn real_create_v2_decodes_with_program_mint_and_trained_creator() {
    for i in 0..2 {
        let (mint, table_creator, tx) = create(i);
        let t = parse_full_tx(&tx).unwrap();
        let m = bs58::decode(&mint).into_vec().unwrap();
        let m: [u8; 32] = m.try_into().unwrap();
        let (creator, kind, _inner) = create_in_tx(&t, &m).expect("a create").expect("verified");
        assert_eq!(kind, "create_v2");
        // The trained definition (launch table) and the decoded create agree.
        assert_eq!(b58(&creator), table_creator);
    }
}

#[test]
fn a_table_launch_that_is_really_a_router_swap_is_not_a_create() {
    // These three "launches" in discovery_raw are ATA-creation + router swaps (logs: CreateTokenAccount,
    // SwapTob, Sell): no pump.fun create instruction for the mint exists in them.
    for i in 0..3 {
        let (mint, tx) = non_create(i);
        let t = parse_full_tx(&tx).unwrap();
        let m: [u8; 32] = bs58::decode(&mint).into_vec().unwrap().try_into().unwrap();
        assert!(create_in_tx(&t, &m).is_none(), "{mint}");
    }
}

#[test]
fn the_oldest_signature_is_not_assumed_to_be_the_creation() {
    // Page 1 (oldest) holds only a non-create for the mint; the create is on page 2.
    let (mint, _, ctx) = create(0);
    let (_, junk) = non_create(0);
    let src = mock(vec![
        ((&mint, ""), page(vec![junk], Some("100:1"))),
        ((&mint, "100:1"), page(vec![ctx], None)),
    ]);
    match find_launch(&src, &mint, B, 42) {
        LaunchOutcome::Verified(e) => {
            assert_eq!(b58(&e.mint), mint);
            assert_eq!(e.source, "MOCK");
            assert_eq!(
                e.retrieved_unix_ms, 42,
                "retrieval time is ours, not the chain's"
            );
            assert_eq!(
                e.block_time_s, None,
                "capture has no block time: unknown, not 0"
            );
        }
        o => panic!("{o:?}"),
    }
    let calls = src.calls.borrow();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[1].2.as_deref(),
        Some("100:1"),
        "pagination token forwarded"
    );
}

#[test]
fn history_without_a_create_is_named_and_errors_are_incomplete_not_absent() {
    let (mint, _) = non_create(1);
    let (_, junk) = non_create(1);
    let src = mock(vec![((&mint, ""), page(vec![junk], None))]);
    assert_eq!(
        find_launch(&src, &mint, B, 0),
        LaunchOutcome::CreateNotFound { pages: 1, txs: 1 }
    );
    let src = mock(vec![((&mint, ""), Err(FetchError::RateLimited))]);
    assert!(matches!(
        find_launch(&src, &mint, B, 0),
        LaunchOutcome::HistoryIncomplete { pages: 0, .. }
    ));
    // Endless pagination hits the budget => incomplete, never "not found".
    let src = mock(vec![
        ((&mint, ""), page(vec![non_create(1).1], Some("a"))),
        ((&mint, "a"), page(vec![non_create(1).1], Some(""))),
    ]);
    assert!(matches!(
        find_launch(&src, &mint, Budget { max_pages: 2 }, 0),
        LaunchOutcome::HistoryIncomplete { pages: 2, .. }
    ));
}

#[test]
fn a_failed_create_and_a_creator_mismatch_are_named() {
    let (mint, _, mut tx) = create(0);
    tx["meta"]["err"] = json!({"InstructionError": [3, "Custom"]});
    let src = mock(vec![((&mint, ""), page(vec![tx], None))]);
    assert!(matches!(
        find_launch(&src, &mint, B, 0),
        LaunchOutcome::CreateFailedTx { .. }
    ));
    // Point the create's `user` account (index 5) at a different key than key 0: the decoded create
    // still names the mint, but key 0 (the trained creator) is not its paying user -> named, not guessed.
    let (mint, _, mut tx) = create(0);
    tx["transaction"]["message"]["instructions"][2]["accounts"][5] = json!(14);
    let src = mock(vec![((&mint, ""), page(vec![tx], None))]);
    assert!(matches!(
        find_launch(&src, &mint, B, 0),
        LaunchOutcome::CreatorAmbiguous { .. }
    ));
}

#[test]
fn request_is_full_detail_ascending_paginated_and_carries_no_secret() {
    let f = json!({"status": "any"});
    let b = request_body("MintXYZ", &f, Some("123:4"), 100);
    assert_eq!(b["method"], "getTransactionsForAddress");
    let o = &b["params"][1];
    assert_eq!(o["transactionDetails"], "full");
    assert_eq!(o["sortOrder"], "asc");
    assert_eq!(o["encoding"], "json");
    assert_eq!(o["maxSupportedTransactionVersion"], 0);
    assert_eq!(o["paginationToken"], "123:4");
    assert!(!b.to_string().contains("api-key"));
    assert_eq!(
        parse_page(&json!({"error": {"code": 429}})).unwrap_err(),
        FetchError::RateLimited
    );
    let p = parse_page(&json!({"result": {"data": [], "paginationToken": null}})).unwrap();
    assert!(p.next.is_none() && p.txs.is_empty());
}

#[test]
fn creator_count_is_exact_only_when_exhausted_and_never_a_partial_zero() {
    let (_, creator, ctx) = create(0);
    let slot = ctx["slot"].as_u64().unwrap();
    let w = CoverageWindow {
        from_slot: slot,
        to_slot_excl: slot + 1,
    };
    // Exhausted: one verified create by this creator in the window.
    let src = mock(vec![((&creator, ""), page(vec![ctx.clone()], None))]);
    match creator_prior_launches(&src, &creator, w, B) {
        PriorLaunches::Exact { n, mints, .. } => assert_eq!((n, mints.len()), (1, 1)),
        o => panic!("{o:?}"),
    }
    // Same create delivered twice across pages: deduplicated by mint.
    let src = mock(vec![
        ((&creator, ""), page(vec![ctx.clone()], Some("p2"))),
        ((&creator, "p2"), page(vec![ctx.clone()], None)),
    ]);
    assert!(matches!(
        creator_prior_launches(&src, &creator, w, B),
        PriorLaunches::Exact { n: 1, .. }
    ));
    // A transport failure mid-walk: Incomplete with a lower bound, never `Exact { n: 0 }`.
    let src = mock(vec![(
        (&creator, ""),
        Err(FetchError::Http("status 503".into())),
    )]);
    assert!(matches!(
        creator_prior_launches(&src, &creator, w, B),
        PriorLaunches::Incomplete { at_least: 0, .. }
    ));
    // The server ignoring the slot filter is detected.
    let narrow = CoverageWindow {
        from_slot: slot + 10,
        to_slot_excl: slot + 20,
    };
    let src = mock(vec![((&creator, ""), page(vec![ctx], None))]);
    assert!(matches!(
        creator_prior_launches(&src, &creator, narrow, B),
        PriorLaunches::Incomplete { .. }
    ));
}

#[test]
fn evidence_round_trips_through_the_cache_and_corrupt_files_are_refetched() {
    let (mint, _, ctx) = create(1);
    let dir = std::env::temp_dir().join(format!("lb_cache_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let cache = EvidenceCache::new(&dir);
    let src = mock(vec![((&mint, ""), page(vec![ctx], None))]);
    let a = launch_cached(&cache, &src, &mint, B, 7);
    let LaunchOutcome::Verified(e) = a else {
        panic!("{a:?}")
    };
    assert_eq!(evidence_from_json(&evidence_json(&e)), Some(e.clone()));
    // Second call: served from cache, no network call.
    let empty = mock(vec![]);
    assert_eq!(
        launch_cached(&cache, &empty, &mint, B, 8),
        LaunchOutcome::Verified(e)
    );
    assert!(empty.calls.borrow().is_empty());
    // Corrupt the file: not a launch; falls through to the source.
    std::fs::write(dir.join(format!("{mint}.json")), b"{\"schema\":\"x\"}").unwrap();
    assert!(matches!(
        launch_cached(&cache, &empty, &mint, B, 9),
        LaunchOutcome::HistoryIncomplete { .. }
    ));
    // Negative results are never cached.
    let (m2, junk) = non_create(2);
    let src = mock(vec![((&m2, ""), page(vec![junk], None))]);
    let _ = launch_cached(&cache, &src, &m2, B, 1);
    assert!(cache.get(&m2).is_none());
    // A path-like "mint" cannot escape the directory.
    assert!(cache.get("../../etc/passwd").is_none());
}

#[test]
fn hindsight_evidence_is_not_available_to_an_earlier_decision() {
    let (mint, _, ctx) = create(0);
    let src = mock(vec![((&mint, ""), page(vec![ctx], None))]);
    let LaunchOutcome::Verified(e) = find_launch(&src, &mint, B, 2_000_000_000_000) else {
        panic!()
    };
    assert!(!pump_quant_junction::launch_bootstrap::available_at(
        &e,
        1_788_965_347_168
    ));
    assert!(pump_quant_junction::launch_bootstrap::available_at(
        &e,
        2_000_000_000_001
    ));
}

#[test]
fn the_credential_loader_refuses_loose_permissions_and_bad_shapes_without_echoing() {
    use std::os::unix::fs::PermissionsExt;
    let d = std::env::temp_dir().join(format!("lb_cred_{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    let p = d.join("helius.env");
    let fake = "00000000-1111-2222-3333-444444444444"; // MOCK value, not a real key
    std::fs::write(&p, format!("HELIUS_API_KEY={fake}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        load_key(&p),
        Err(CredError::InsecureMode { mode: 0o644, .. })
    ));
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
    let k = load_key(&p).unwrap();
    assert_eq!(format!("{k:?}"), "ApiKey(<redacted>)");
    std::fs::write(&p, "HELIUS_API_KEY=not-a-key\n").unwrap();
    let e = load_key(&p).unwrap_err();
    assert!(matches!(e, CredError::BadShape(_)));
    assert!(
        !format!("{e:?}").contains("not-a-key"),
        "errors never carry the value"
    );
    assert!(matches!(
        load_key(&d.join("absent")),
        Err(CredError::Missing(_))
    ));
}

/// Broad check over the fixed audit sample (97 table creates: 60 renormalized_v7 + 37 discovery_raw, and 3
/// discovery_raw non-creates), real capture transactions in RPC shape. Lives outside the repo, so opt-in:
/// `LB_AUDIT=/training/mh_build/proc/m2_audit_rpc_shape.json cargo test -- --ignored`.
#[test]
#[ignore = "needs LB_AUDIT file outside the repo"]
fn audit_sample_decoder_matches_python_classification() {
    let p = std::env::var("LB_AUDIT").expect("LB_AUDIT");
    let a: Value = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
    let mut ok = 0;
    for c in a["creates"].as_array().unwrap() {
        let mint = c["mint"].as_str().unwrap();
        let src = mock(vec![((mint, ""), page(vec![c["tx"].clone()], None))]);
        match find_launch(&src, mint, B, 0) {
            LaunchOutcome::Verified(e) => {
                assert_eq!(
                    b58(&e.creator),
                    c["table_creator"].as_str().unwrap(),
                    "{mint}"
                );
                ok += 1;
            }
            o => panic!("{mint}: {o:?}"),
        }
    }
    for c in a["non_creates"].as_array().unwrap() {
        let mint = c["mint"].as_str().unwrap();
        let src = mock(vec![((mint, ""), page(vec![c["tx"].clone()], None))]);
        assert!(matches!(
            find_launch(&src, mint, B, 0),
            LaunchOutcome::CreateNotFound { .. }
        ));
    }
    assert_eq!(ok, 97);
}
