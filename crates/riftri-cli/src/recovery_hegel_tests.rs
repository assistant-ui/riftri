//! Property tests for the `repair` and `gc` report renderers.

use crate::hegel_tests::process_cases;
use anyhow::Result;
use hegel::TestCase;
use hegel::generators as gs;
use serde_json::{Value, json};

use super::render_hegel_tests::{
    count_at, hex_path, hex_paths, parse_report, path_at, paths_at, render, reported_paths, shows,
};
use super::{print_garbage_collection_report, print_recovery_report};

/// JSON key, human label, and spec field of every plain repair counter.
const RECOVERY_COUNTERS: [(&str, &str); 19] = [
    ("scanned", "Scanned operations"),
    ("busy_adds", "Busy add operations skipped"),
    ("active", "Active worktrees"),
    ("recovered_mounts", "Recovered mounts"),
    ("recovered_adds", "Recovered add operations"),
    ("completed_removals", "Completed removals"),
    ("recovered_removals", "Recovered removals"),
    ("cancelled_removals", "Cancelled removals"),
    ("completed_moves", "Completed moves"),
    ("recovered_moves", "Recovered moves"),
    ("cancelled_moves", "Cancelled moves"),
    ("completed_compactions", "Completed compactions"),
    ("recovered_compactions", "Recovered compactions"),
    ("completed_prunes", "Completed prunes"),
    ("recovered_prunes", "Recovered prunes"),
    ("completed_collections", "Completed collections"),
    ("recovered_collections", "Recovered collections"),
    ("retired_adds", "Retired add operations"),
    ("rehomed_adds", "Re-homed add operations"),
];

fn texts(tc: &TestCase) -> Value {
    let count = tc.draw(gs::integers::<usize>().max_value(2));
    (0..count)
        .map(|_| json!(tc.draw(gs::text().max_size(24))))
        .collect()
}

fn texts_at(spec: &Value) -> Vec<String> {
    spec.as_array()
        .expect("text list")
        .iter()
        .map(|text| text.as_str().expect("text").to_owned())
        .collect()
}

pub(super) fn render_recovery(spec: &Value, json: bool) -> Result<()> {
    let counter = |field: &str| count_at(&spec["counters"][field]);
    let report = riftri_core::RecoveryReport {
        scanned: counter("scanned"),
        busy_adds: counter("busy_adds"),
        recovered: counter("recovered_adds"),
        active: counter("active"),
        recovered_mounts: counter("recovered_mounts"),
        completed_removals: counter("completed_removals"),
        recovered_removals: counter("recovered_removals"),
        cancelled_removals: counter("cancelled_removals"),
        completed_moves: counter("completed_moves"),
        recovered_moves: counter("recovered_moves"),
        cancelled_moves: counter("cancelled_moves"),
        completed_compactions: counter("completed_compactions"),
        recovered_compactions: counter("recovered_compactions"),
        completed_prunes: counter("completed_prunes"),
        recovered_prunes: counter("recovered_prunes"),
        completed_collections: counter("completed_collections"),
        recovered_collections: counter("recovered_collections"),
        retired_adds: counter("retired_adds"),
        released_adds: paths_at(&spec["released"]),
        rehomed_adds: counter("rehomed_adds"),
        relocations: spec["relocations"]
            .as_array()
            .expect("relocations")
            .iter()
            .map(|relocation| riftri_core::RelocatedWorktree {
                operation_id: relocation["id"].as_str().expect("id").to_owned(),
                journal_destination: path_at(&relocation["journal"]),
                registered_path: path_at(&relocation["registered"]),
            })
            .collect(),
        unresolvable_worktrees: paths_at(&spec["unresolvable"]),
        reaped_artifacts: paths_at(&spec["reaped_artifacts"]),
        reaped_probe_roots: paths_at(&spec["reaped_probe_roots"]),
        preserved_probe_mounts: paths_at(&spec["preserved_probe_mounts"]),
        errors: texts_at(&spec["errors"]),
    };
    print_recovery_report(&path_at(&spec["state"]), &report, json)
}

#[hegel::test(process_cases(30))]
fn repair_reports_agree_across_renderings_and_fail_only_on_errors(tc: TestCase) {
    let counters = RECOVERY_COUNTERS
        .iter()
        .map(|(field, _)| ((*field).to_owned(), json!(tc.draw(gs::integers::<u32>()))))
        .collect::<serde_json::Map<_, _>>();
    let relocations = (0..tc.draw(gs::integers::<usize>().max_value(2)))
        .map(|_| {
            json!({
                "id": tc.draw(gs::text().max_size(12)),
                "journal": hex_path(&tc),
                "registered": hex_path(&tc),
            })
        })
        .collect::<Value>();
    let spec = json!({
        "kind": "recovery",
        "state": hex_path(&tc),
        "counters": counters,
        "released": hex_paths(&tc),
        "relocations": relocations,
        "unresolvable": hex_paths(&tc),
        "reaped_artifacts": hex_paths(&tc),
        "reaped_probe_roots": hex_paths(&tc),
        "preserved_probe_mounts": hex_paths(&tc),
        "errors": texts(&tc),
    });
    let errors = texts_at(&spec["errors"]);

    let machine = render(&spec, true);
    // A report that lists operations needing attention must not exit 0.
    assert_eq!(machine.succeeded, errors.is_empty());
    let report = parse_report(&machine.output);
    assert_eq!(
        path_at(&report["state_directory_native_hex"]),
        path_at(&spec["state"])
    );
    for (field, _) in RECOVERY_COUNTERS {
        assert_eq!(report[field], spec["counters"][field], "{field}");
    }
    for (reported, field) in [
        ("unresolvable_worktrees", "unresolvable"),
        ("reaped_artifacts", "reaped_artifacts"),
        ("reaped_probe_roots", "reaped_probe_roots"),
        ("preserved_probe_mounts", "preserved_probe_mounts"),
    ] {
        assert_eq!(
            reported_paths(&report[reported], "path"),
            paths_at(&spec[field]),
            "{reported}"
        );
    }
    for (reported, field) in [
        ("journal_destination", "journal"),
        ("registered_path", "registered"),
    ] {
        assert_eq!(
            reported_paths(&report["relocated_worktrees"], reported),
            spec["relocations"]
                .as_array()
                .expect("relocations")
                .iter()
                .map(|relocation| path_at(&relocation[field]))
                .collect::<Vec<_>>()
        );
    }
    assert_eq!(report["errors"], spec["errors"]);

    let human = render(&spec, false);
    assert_eq!(human.succeeded, errors.is_empty());
    assert!(shows(&human.output, "State: ", &path_at(&spec["state"])));
    for (field, label) in RECOVERY_COUNTERS {
        assert!(
            human
                .output
                .contains(&format!("{label}: {}\n", spec["counters"][field])),
            "{label}"
        );
    }
    for path in paths_at(&spec["released"]) {
        assert!(shows(&human.output, "Released ", &path));
    }
    for field in [
        "unresolvable",
        "reaped_artifacts",
        "reaped_probe_roots",
        "preserved_probe_mounts",
    ] {
        for path in paths_at(&spec[field]) {
            assert!(shows(&human.output, "- ", &path), "{field}");
        }
    }
    for relocation in spec["relocations"].as_array().expect("relocations") {
        assert!(human.output.contains(&format!(
            "journaled {} is now registered by Git at {}",
            path_at(&relocation["journal"]).display(),
            path_at(&relocation["registered"]).display()
        )));
    }
    for (heading, field) in [
        (
            "Abandoned OverlayFS probes still covered by a mount (preserved):",
            "preserved_probe_mounts",
        ),
        (
            "Relocated worktrees Riftri no longer tracks:",
            "relocations",
        ),
        (
            "Worktrees Git lists without a resolvable HEAD:",
            "unresolvable",
        ),
        ("Operations needing attention:", "errors"),
    ] {
        let listed = !spec[field].as_array().expect("list").is_empty();
        assert_eq!(human.output.contains(heading), listed, "{heading}");
    }
    for error in &errors {
        assert!(human.output.contains(&format!("- {error}\n")));
    }
    assert_eq!(
        human
            .output
            .ends_with("No journaled operation needs manual attention\n"),
        errors.is_empty()
    );
}

pub(super) fn render_collection(spec: &Value, json: bool) -> Result<()> {
    let report = riftri_core::GarbageCollectionReport {
        applied: spec["applied"].as_bool().expect("applied"),
        candidates: spec["candidates"]
            .as_array()
            .expect("candidates")
            .iter()
            .map(|candidate| riftri_core::GarbageCollectionCandidate {
                base_path: path_at(&candidate["base"]),
                logical_bytes: candidate["logical"].as_u64().expect("logical"),
                allocated_bytes: candidate["allocated"].as_u64().expect("allocated"),
            })
            .collect(),
        collected: paths_at(&spec["collected"]),
        skipped_in_use: paths_at(&spec["skipped_in_use"]),
        skipped_protected: spec["skipped_protected"]
            .as_array()
            .expect("protected bases")
            .iter()
            .map(|protection| riftri_core::ProtectedBase {
                base_path: path_at(&protection["base"]),
                operation_id: protection["id"].as_str().expect("id").to_owned(),
                reason: protection["reason"].as_str().expect("reason").to_owned(),
            })
            .collect(),
        resumed_collections: count_at(&spec["resumed"]),
        removed_logical_bytes: spec["removed_logical"].as_u64().expect("removed logical"),
        removed_allocated_bytes: spec["removed_allocated"]
            .as_u64()
            .expect("removed allocated"),
        retirable_journals: count_at(&spec["retirable"]),
        retired_journals: count_at(&spec["retired"]),
    };
    print_garbage_collection_report(&path_at(&spec["state"]), &report, json)
}

#[hegel::test(process_cases(30))]
fn collection_reports_agree_across_renderings_and_only_plans_offer_apply(tc: TestCase) {
    let candidates = (0..tc.draw(gs::integers::<usize>().max_value(2)))
        .map(|_| {
            json!({
                "base": hex_path(&tc),
                "logical": tc.draw(gs::integers::<u64>()),
                "allocated": tc.draw(gs::integers::<u64>()),
            })
        })
        .collect::<Value>();
    let protected = (0..tc.draw(gs::integers::<usize>().max_value(2)))
        .map(|_| {
            json!({
                "base": hex_path(&tc),
                "id": tc.draw(gs::text().max_size(12)),
                "reason": tc.draw(gs::text().max_size(24)),
            })
        })
        .collect::<Value>();
    let spec = json!({
        "kind": "collection",
        "state": hex_path(&tc),
        "applied": tc.draw(gs::booleans()),
        "candidates": candidates,
        "collected": hex_paths(&tc),
        "skipped_in_use": hex_paths(&tc),
        "skipped_protected": protected,
        "resumed": tc.draw(gs::integers::<u32>()),
        "removed_logical": tc.draw(gs::integers::<u64>()),
        "removed_allocated": tc.draw(gs::integers::<u64>()),
        "retirable": tc.draw(gs::integers::<u32>().max_value(2)),
        "retired": tc.draw(gs::integers::<u32>()),
    });
    let state = path_at(&spec["state"]);
    let applied = spec["applied"] == true;

    let machine = render(&spec, true);
    assert!(machine.succeeded);
    let report = parse_report(&machine.output);
    assert_eq!(path_at(&report["state_directory_native_hex"]), state);
    assert_eq!(report["applied"], applied);
    for (reported, field, key) in [
        ("candidates", "candidates", "base"),
        ("skipped_protected", "skipped_protected", "base"),
    ] {
        assert_eq!(
            reported_paths(&report[reported], "base_path"),
            spec[field]
                .as_array()
                .expect("list")
                .iter()
                .map(|entry| path_at(&entry[key]))
                .collect::<Vec<_>>()
        );
    }
    for field in ["collected", "skipped_in_use"] {
        assert_eq!(
            reported_paths(&report[field], "path"),
            paths_at(&spec[field])
        );
    }
    for (reported, field) in [
        ("resumed_collections", "resumed"),
        ("removed_logical_bytes", "removed_logical"),
        ("removed_allocated_bytes", "removed_allocated"),
        ("retirable_journals", "retirable"),
        ("retired_journals", "retired"),
    ] {
        assert_eq!(report[reported], spec[field], "{reported}");
    }

    let human = render(&spec, false);
    assert!(human.succeeded);
    assert!(shows(&human.output, "State: ", &state));
    assert_eq!(human.output.contains("Mode: applied\n"), applied);
    assert_eq!(human.output.contains("Mode: plan only\n"), !applied);
    let listed = |field: &str| spec[field].as_array().expect("list").len();
    for (label, field) in [
        ("Eligible bases", "candidates"),
        ("Collected bases", "collected"),
        ("Skipped because now in use", "skipped_in_use"),
        (
            "Skipped because a journaled operation still claims them",
            "skipped_protected",
        ),
    ] {
        assert!(
            human
                .output
                .contains(&format!("{label}: {}\n", listed(field))),
            "{label}"
        );
    }
    for protection in spec["skipped_protected"].as_array().expect("protected") {
        assert!(human.output.contains(&format!(
            "- {}: {} (operation {})\n",
            path_at(&protection["base"]).display(),
            protection["reason"].as_str().expect("reason"),
            protection["id"].as_str().expect("id"),
        )));
    }
    assert_eq!(
        human
            .output
            .contains(&format!("Retired finished journals: {}\n", spec["retired"])),
        applied
    );
    assert_eq!(
        human.output.contains(&format!(
            "Finished journals to retire: {}\n",
            spec["retirable"]
        )),
        !applied
    );
    // Only a plan with something in it tells the caller how to apply it, and
    // never with a command that would address a different directory.
    let offers_apply = !applied && (listed("candidates") > 0 || spec["retirable"] != 0);
    assert_eq!(
        human.output.contains("Nothing was deleted; rerun with "),
        offers_apply
    );
    if offers_apply {
        match riftri_core::shell_quoted_path(&state) {
            Some(quoted) => assert!(human.output.contains(&format!(
                "`riftri gc --apply --state-dir {quoted}` to collect this plan\n"
            ))),
            None => assert!(!human.output.contains("--state-dir")),
        }
    }
}
