//! The labeled proposal sets of the built-in judges (spec §10), one per judge under
//! `tests/fixtures/judges/`: a §6.5 judge input from an invented project and the verdict each
//! proposal should get. A `revise` counts as not accepted. The live test in `tests/live.rs`
//! replays them against a real agent; this one keeps them well-formed.

use serde_json::Value;

pub const JUDGES: [&str; 3] = ["support", "removal", "duplicate"];

fn set(judge: &str) -> Value {
    let p = format!(
        "{}/tests/fixtures/judges/{judge}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap()
}

#[test]
fn each_labeled_set_is_a_judge_input_with_one_label_per_proposal() {
    for judge in JUDGES {
        let set = set(judge);
        assert_eq!(set["judge"], judge);
        let input: rrecall::wire::JudgeInput =
            serde_json::from_value(set["input"].clone()).unwrap_or_else(|e| panic!("{judge}: {e}"));
        let keys: Vec<&str> = input.proposals.iter().map(|j| j.key.as_str()).collect();
        let expected = set["expected"].as_object().unwrap();
        let mut labeled: Vec<&str> = expected.keys().map(String::as_str).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        labeled.sort();
        assert_eq!(sorted, labeled, "{judge}: one label per proposal");
        for verdict in ["accept", "reject"] {
            assert!(
                expected.values().any(|v| v == verdict),
                "{judge}: no `{verdict}` label"
            );
        }
        for j in &input.proposals {
            let refs = j.sources.as_ref().unwrap();
            assert!(!refs.is_empty(), "{judge}: {} cites nothing", j.key);
            for r in refs.keys().chain(j.proposal.sources.iter()) {
                rrecall::node::check_source(r).unwrap_or_else(|e| panic!("{judge}: {r}: {e}"));
            }
        }
        // One model call per set, as a run would make it: no two proposals on one node (§4.1
        // step 5 would reject both) and one text per reference (a resolve gives one).
        let mut targets = std::collections::BTreeSet::new();
        let mut texts = std::collections::BTreeMap::new();
        for j in &input.proposals {
            if let Some(id) = &j.proposal.id {
                assert!(targets.insert(id.clone()), "{judge}: two proposals on `{id}`");
            }
            for (r, t) in j.sources.as_ref().unwrap() {
                let first = texts.entry(r.clone()).or_insert(t.clone());
                assert_eq!(first, t, "{judge}: `{r}` resolves twice");
            }
        }
        for n in &input.nodes {
            n.validate()
                .unwrap_or_else(|e| panic!("{judge}: {}: {e}", n.id));
        }
    }
}
