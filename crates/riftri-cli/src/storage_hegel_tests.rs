//! Property tests for the `status` and `worktree list` report renderers.

use anyhow::Result;
use hegel::TestCase;
use hegel::generators as gs;
use serde_json::{Value, json};

use super::hegel_tests::decode_hex;
use super::render_hegel_tests::{
    BACKENDS, count_at, hex_path, object_id, parse_report, path_at, render, reported_paths, shows,
};
use super::{
    display_byte_count, encode_hex, print_all_states_worktree_inventory, print_storage_accounting,
    print_worktree_inventory,
};
use crate::hegel_tests::process_cases;

/// JSON key and human label of every `status` operation counter.
const OPERATION_COUNTERS: [(&str, &str); 17] = [
    ("active_views", "Active views"),
    ("pending_adds", "Pending adds"),
    ("completed_removals", "Completed removals"),
    ("cancelled_removals", "Cancelled removals"),
    ("pending_removals", "Pending removals"),
    ("completed_moves", "Completed moves"),
    ("cancelled_moves", "Cancelled moves"),
    ("pending_moves", "Pending moves"),
    ("completed_compactions", "Completed compactions"),
    ("cancelled_compactions", "Cancelled compactions"),
    ("pending_compactions", "Pending compactions"),
    ("completed_prunes", "Completed prunes"),
    ("pending_prunes", "Pending prunes"),
    ("completed_collections", "Completed collections"),
    ("cancelled_collections", "Cancelled collections"),
    ("pending_collections", "Pending collections"),
    ("coordination_locks", "Coordination locks"),
];

/// Pending counter and what `status` says repair will do about it.
const PENDING_REMEDIES: [(&str, &str); 6] = [
    ("pending_adds", "to roll back pending adds"),
    ("pending_removals", "to resume pending removals"),
    ("pending_moves", "to resume pending moves"),
    ("pending_compactions", "to resume pending compactions"),
    ("pending_prunes", "to resume pending prunes"),
    ("pending_collections", "to resume pending collections"),
];

fn list(tc: &TestCase, item: impl Fn(&TestCase) -> Value) -> Value {
    let count = tc.draw(gs::integers::<usize>().max_value(2));
    (0..count).map(|_| item(tc)).collect()
}

fn items(spec: &Value) -> &[Value] {
    spec.as_array().expect("list")
}

fn git_bytes(tc: &TestCase) -> Value {
    if tc.draw(gs::booleans()) {
        json!(encode_hex(&tc.draw(gs::binary().max_size(16))))
    } else {
        Value::Null
    }
}

fn view_spec(tc: &TestCase) -> Value {
    json!({
        "repository": hex_path(tc),
        "destination": hex_path(tc),
        "base": hex_path(tc),
        "backend": tc.draw(gs::integers::<usize>().max_value(BACKENDS.len() - 1)),
        "head": object_id(tc),
        "branch": git_bytes(tc),
        "detached": tc.draw(gs::booleans()),
        "locked": git_bytes(tc),
        "prunable": git_bytes(tc),
        "logical": tc.draw(gs::integers::<u64>()),
        "allocated": tc.draw(gs::integers::<u64>()),
    })
}

fn issue_spec(tc: &TestCase) -> Value {
    json!({
        "path": hex_path(tc),
        "reason": tc.draw(gs::text().max_size(24)),
        "may_hide": tc.draw(gs::booleans()),
    })
}

fn view(spec: &Value) -> riftri_core::ViewStorageAccounting {
    let git_bytes = |field: &str| spec[field].as_str().map(decode_hex);
    riftri_core::ViewStorageAccounting {
        repository: path_at(&spec["repository"]),
        destination: path_at(&spec["destination"]),
        base_path: path_at(&spec["base"]),
        backend: BACKENDS[count_at(&spec["backend"])],
        head: riftri_git::ObjectId::parse(spec["head"].as_str().expect("head")).expect("head"),
        branch: git_bytes("branch"),
        detached: spec["detached"].as_bool().expect("detached"),
        locked_reason: git_bytes("locked"),
        prunable_reason: git_bytes("prunable"),
        logical_bytes: spec["logical"].as_u64().expect("logical"),
        allocated_bytes: spec["allocated"].as_u64().expect("allocated"),
    }
}

fn issue(spec: &Value) -> riftri_core::StateDiagnosticIssue {
    riftri_core::StateDiagnosticIssue {
        path: path_at(&spec["path"]),
        reason: spec["reason"].as_str().expect("reason").to_owned(),
        base_count_impact: if spec["may_hide"] == true {
            riftri_core::BaseCountImpact::MayHideReference
        } else {
            riftri_core::BaseCountImpact::CountsUnaffected
        },
    }
}

fn accounting(spec: &Value) -> riftri_core::StorageAccountingReport {
    let counter = |field: &str| count_at(&spec["counters"][field]);
    riftri_core::StorageAccountingReport {
        active_views: counter("active_views"),
        pending_adds: counter("pending_adds"),
        completed_removals: counter("completed_removals"),
        cancelled_removals: counter("cancelled_removals"),
        pending_removals: counter("pending_removals"),
        completed_moves: counter("completed_moves"),
        cancelled_moves: counter("cancelled_moves"),
        pending_moves: counter("pending_moves"),
        completed_compactions: counter("completed_compactions"),
        cancelled_compactions: counter("cancelled_compactions"),
        pending_compactions: counter("pending_compactions"),
        completed_prunes: counter("completed_prunes"),
        pending_prunes: counter("pending_prunes"),
        completed_collections: counter("completed_collections"),
        cancelled_collections: counter("cancelled_collections"),
        pending_collections: counter("pending_collections"),
        coordination_locks: counter("coordination_locks"),
        bases: items(&spec["bases"])
            .iter()
            .map(|base| riftri_core::BaseStorageAccounting {
                path: path_at(&base["path"]),
                reference_count: count_at(&base["references"]),
                logical_bytes: base["logical"].as_u64().expect("logical"),
                allocated_bytes: base["allocated"].as_u64().expect("allocated"),
                damaged: base["damaged"].as_bool().expect("damaged"),
            })
            .collect(),
        views: items(&spec["views"]).iter().map(view).collect(),
        diagnostic_issues: items(&spec["issues"]).iter().map(issue).collect(),
        total_logical_bytes: spec["total_logical"].as_u64().expect("total logical"),
        total_allocated_bytes: spec["total_allocated"].as_u64().expect("total allocated"),
    }
}

pub(super) fn render_storage(spec: &Value, json: bool) -> Result<()> {
    match spec["kind"].as_str().expect("kind") {
        "status" => print_storage_accounting(&path_at(&spec["state"]), &accounting(spec), json),
        "inventory" => print_worktree_inventory(&path_at(&spec["state"]), &accounting(spec), json),
        _ => print_all_states_worktree_inventory(
            &riftri_core::AllStatesWorktreeInventory {
                states: items(&spec["states"])
                    .iter()
                    .map(|state| riftri_core::StateWorktreeInventory {
                        state_directory: path_at(&state["path"]),
                        source: if state["registered"] == true {
                            riftri_core::StateDirectorySource::Registered
                        } else {
                            riftri_core::StateDirectorySource::Default
                        },
                        views: items(&state["views"]).iter().map(view).collect(),
                        diagnostic_issues: items(&state["issues"]).iter().map(issue).collect(),
                    })
                    .collect(),
                registration_issues: items(&spec["registration_issues"])
                    .iter()
                    .map(issue)
                    .collect(),
            },
            json,
        ),
    }
}

fn accounting_spec(tc: &TestCase, kind: &str) -> Value {
    let counters = OPERATION_COUNTERS
        .iter()
        .map(|(field, _)| {
            (
                (*field).to_owned(),
                json!(tc.draw(gs::integers::<u32>().max_value(2))),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let bases = list(tc, |tc| {
        json!({
            "path": hex_path(tc),
            "references": tc.draw(gs::integers::<u32>().max_value(2)),
            "logical": tc.draw(gs::integers::<u64>()),
            "allocated": tc.draw(gs::integers::<u64>()),
            "damaged": tc.draw(gs::booleans()),
        })
    });
    json!({
        "kind": kind,
        "state": hex_path(tc),
        "counters": counters,
        "bases": bases,
        "views": list(tc, view_spec),
        "issues": list(tc, issue_spec),
        "total_logical": tc.draw(gs::integers::<u64>()),
        "total_allocated": tc.draw(gs::integers::<u64>()),
    })
}

/// Check one reported worktree against the view it was rendered from.
fn assert_view_reported(reported: &Value, spec: &Value) {
    for (key, field) in [
        ("repository", "repository"),
        ("path", "destination"),
        ("base_path", "base"),
    ] {
        assert_eq!(
            path_at(&reported[format!("{key}_native_hex")]),
            path_at(&spec[field])
        );
    }
    for (key, field) in [
        ("branch", "branch"),
        ("locked_reason", "locked"),
        ("prunable_reason", "prunable"),
    ] {
        assert_eq!(reported[format!("{key}_hex")], spec[field], "{key}");
    }
    assert_eq!(reported["head"], spec["head"]);
    assert_eq!(reported["detached"], spec["detached"]);
    assert_eq!(reported["logical_bytes"], spec["logical"]);
    assert_eq!(reported["allocated_bytes"], spec["allocated"]);
}

fn count_where(spec: &Value, predicate: impl Fn(&Value) -> bool) -> usize {
    items(spec).iter().filter(|item| predicate(item)).count()
}

#[hegel::test(process_cases(30))]
fn status_never_calls_a_base_unused_on_an_incomplete_inventory(tc: TestCase) {
    let spec = accounting_spec(&tc, "status");
    let state = path_at(&spec["state"]);
    let complete = count_where(&spec["issues"], |issue| issue["may_hide"] == true) == 0;

    let machine = render(&spec, true);
    assert!(machine.succeeded);
    let report = parse_report(&machine.output);
    assert_eq!(path_at(&report["state_directory_native_hex"]), state);
    assert_eq!(report["counts_complete"], complete);
    for (field, _) in OPERATION_COUNTERS {
        assert_eq!(
            report["operations"][field], spec["counters"][field],
            "{field}"
        );
    }
    let reported_bases = items(&report["bases"]);
    assert_eq!(reported_bases.len(), items(&spec["bases"]).len());
    for (reported, base) in reported_bases.iter().zip(items(&spec["bases"])) {
        assert_eq!(
            path_at(&reported["path_native_hex"]),
            path_at(&base["path"])
        );
        assert_eq!(reported["reference_count"], base["references"]);
        assert_eq!(reported["reference_count_complete"], complete);
        assert_eq!(reported["in_use"], base["references"] != 0 || !complete);
        assert_eq!(reported["damaged"], base["damaged"]);
    }
    let reported_views = items(&report["worktrees"]);
    assert_eq!(reported_views.len(), items(&spec["views"]).len());
    for (reported, view) in reported_views.iter().zip(items(&spec["views"])) {
        assert_view_reported(reported, view);
    }
    let reported_issues = items(&report["diagnostic_issues"]);
    assert_eq!(reported_issues.len(), items(&spec["issues"]).len());
    for (reported, issue) in reported_issues.iter().zip(items(&spec["issues"])) {
        assert_eq!(
            path_at(&reported["path_native_hex"]),
            path_at(&issue["path"])
        );
        assert_eq!(reported["reason"], issue["reason"]);
        assert_eq!(reported["may_hide_base_reference"], issue["may_hide"]);
    }
    assert_eq!(report["total_logical_bytes"], spec["total_logical"]);
    assert_eq!(report["total_allocated_bytes"], spec["total_allocated"]);

    let human = render(&spec, false);
    assert!(human.succeeded);
    assert!(shows(&human.output, "State: ", &state));
    for (field, label) in OPERATION_COUNTERS {
        assert!(
            human
                .output
                .contains(&format!("{label}: {}", spec["counters"][field])),
            "{label}"
        );
    }
    // Every hint names this state directory, or no command at all when the
    // path cannot be written as a shell argument.
    let hint = match riftri_core::repair_command(&state) {
        Some(command) => format!("run `{command}`"),
        None => "run riftri repair against the state directory shown above".to_owned(),
    };
    for (field, remedy) in PENDING_REMEDIES {
        assert_eq!(
            human
                .output
                .contains(&format!("Attention: {hint} {remedy}\n")),
            spec["counters"][field] != 0,
            "{remedy}"
        );
    }
    for (state, expected) in [
        (
            "state=in use\n",
            count_where(&spec["bases"], |base| base["references"] != 0),
        ),
        (
            "state=retained cache; no active views\n",
            count_where(&spec["bases"], |base| base["references"] == 0 && complete),
        ),
        (
            "state=reference count unconfirmed",
            count_where(&spec["bases"], |base| base["references"] == 0 && !complete),
        ),
        (
            "  Damaged: ",
            count_where(&spec["bases"], |base| base["damaged"] == true),
        ),
    ] {
        assert_eq!(human.output.matches(state).count(), expected, "{state}");
    }
    let issues = items(&spec["issues"]).len();
    assert!(human.output.contains(&format!("State issues: {issues}\n")));
    assert_eq!(
        human
            .output
            .contains("Attention: Riftri preserves unexplained state"),
        issues > 0
    );
    assert!(human.output.contains(&format!(
        "Total logical: {}\n",
        display_byte_count(spec["total_logical"].as_u64().expect("total"))
    )));
}

/// How many times the human listing must show each optional view detail.
fn assert_view_details(output: &str, views: &[&Value], states_shown: usize) {
    let count =
        |predicate: &dyn Fn(&Value) -> bool| views.iter().filter(|view| predicate(view)).count();
    for (detail, expected) in [
        ("  State: ", states_shown),
        ("  Head: ", views.len()),
        (
            "  Branch: detached\n",
            count(&|view| view["branch"].is_null() && view["detached"] == true),
        ),
        ("  Locked: ", count(&|view| !view["locked"].is_null())),
        ("  Prunable: ", count(&|view| !view["prunable"].is_null())),
    ] {
        assert_eq!(output.matches(detail).count(), expected, "{detail}");
    }
    for view in views {
        assert!(shows(output, "- ", &path_at(&view["destination"])));
        if let Some(branch) = view["branch"].as_str() {
            assert!(output.contains(&format!(
                "  Branch: {}\n",
                String::from_utf8_lossy(&decode_hex(branch))
            )));
        }
    }
}

#[hegel::test(process_cases(30))]
fn worktree_listings_agree_across_renderings(tc: TestCase) {
    let spec = accounting_spec(&tc, "inventory");

    let machine = render(&spec, true);
    assert!(machine.succeeded);
    let report = parse_report(&machine.output);
    assert_eq!(
        path_at(&report["state_directory_native_hex"]),
        path_at(&spec["state"])
    );
    let reported_views = items(&report["worktrees"]);
    assert_eq!(reported_views.len(), items(&spec["views"]).len());
    for (reported, view) in reported_views.iter().zip(items(&spec["views"])) {
        assert_view_reported(reported, view);
    }
    assert_eq!(
        reported_paths(&report["diagnostic_issues"], "path"),
        items(&spec["issues"])
            .iter()
            .map(|issue| path_at(&issue["path"]))
            .collect::<Vec<_>>()
    );

    let human = render(&spec, false);
    assert!(human.succeeded);
    let views = items(&spec["views"]).iter().collect::<Vec<_>>();
    assert!(
        human
            .output
            .contains(&format!("Managed worktrees: {}\n", views.len()))
    );
    assert_view_details(&human.output, &views, 0);
    let issues = items(&spec["issues"]);
    assert_eq!(
        human
            .output
            .contains(&format!("Diagnostic issues: {}\n", issues.len())),
        !issues.is_empty()
    );
    for issue in issues {
        assert!(human.output.contains(&format!(
            "- {}: {}\n",
            path_at(&issue["path"]).display(),
            issue["reason"].as_str().expect("reason")
        )));
    }
}

#[hegel::test(process_cases(30))]
fn all_states_listings_attribute_every_entry_to_its_state_directory(tc: TestCase) {
    let states = list(&tc, |tc| {
        json!({
            "path": hex_path(tc),
            "registered": tc.draw(gs::booleans()),
            "views": list(tc, view_spec),
            "issues": list(tc, issue_spec),
        })
    });
    let spec = json!({
        "kind": "all-states",
        "states": states,
        "registration_issues": list(&tc, issue_spec),
    });
    let states = items(&spec["states"]);
    let registration_issues = items(&spec["registration_issues"]);
    // Each view and each issue, paired with the state directory that owns it.
    let owned = |field: &str| {
        states
            .iter()
            .flat_map(|state| {
                items(&state[field])
                    .iter()
                    .map(move |entry| (&state["path"], entry))
            })
            .collect::<Vec<_>>()
    };
    let views = owned("views");
    let state_issues = owned("issues");

    let machine = render(&spec, true);
    assert!(machine.succeeded);
    let report = parse_report(&machine.output);
    assert_eq!(report["schema_version"], 2);
    let reported_states = items(&report["state_directories"]);
    assert_eq!(reported_states.len(), states.len());
    for (reported, state) in reported_states.iter().zip(states) {
        assert_eq!(
            path_at(&reported["path_native_hex"]),
            path_at(&state["path"])
        );
        assert_eq!(
            reported["source"] == "registered",
            state["registered"] == true
        );
    }
    let reported_views = items(&report["worktrees"]);
    assert_eq!(reported_views.len(), views.len());
    for (reported, (state, view)) in reported_views.iter().zip(&views) {
        assert_view_reported(reported, view);
        assert_eq!(reported["state_directory_native_hex"], **state);
    }
    let reported_issues = items(&report["diagnostic_issues"]);
    assert_eq!(
        reported_issues.len(),
        registration_issues.len() + state_issues.len()
    );
    let (unowned, owned_issues) = reported_issues.split_at(registration_issues.len());
    for (reported, issue) in unowned.iter().zip(registration_issues) {
        // A registration issue belongs to no usable state directory.
        assert!(reported["state_directory"].is_null());
        assert_eq!(reported["path_native_hex"], issue["path"]);
    }
    for (reported, (state, issue)) in owned_issues.iter().zip(&state_issues) {
        assert_eq!(reported["state_directory_native_hex"], **state);
        assert_eq!(reported["path_native_hex"], issue["path"]);
    }

    let human = render(&spec, false);
    assert!(human.succeeded);
    assert!(
        human
            .output
            .contains(&format!("State directories: {}\n", states.len()))
    );
    for state in states {
        let source = if state["registered"] == true {
            "registered"
        } else {
            "default"
        };
        assert!(human.output.contains(&format!(
            "- {} ({source})\n",
            path_at(&state["path"]).display()
        )));
    }
    assert!(
        human
            .output
            .contains(&format!("Managed worktrees: {}\n", views.len()))
    );
    let view_specs = views.iter().map(|(_, view)| *view).collect::<Vec<_>>();
    assert_view_details(&human.output, &view_specs, views.len());
    for (state, _) in &views {
        assert!(shows(&human.output, "  State: ", &path_at(state)));
    }
    let issues = registration_issues.len() + state_issues.len();
    assert_eq!(
        human
            .output
            .contains(&format!("Diagnostic issues: {issues}\n")),
        issues > 0
    );
}
