//! Shared harness for property tests of report rendering, plus the lifecycle
//! result renderers.
//!
//! The renderers write to the process's own stdout, which the test harness
//! shares. Each case is therefore described as a JSON spec and rendered in a
//! child copy of this test binary, whose stdout the parent reads back.

use std::path::{Path, PathBuf};

use hegel::TestCase;
use hegel::generators as gs;
use serde_json::{Value, json};

use super::hegel_tests::{check_report_value, decode_hex, native_path, path_from_bytes};
use super::{native_path_hex, print_add_result, print_move_result, print_remove_result, ui};
use crate::hegel_tests::process_cases;

const SPEC_ENV: &str = "RIFTRI_TEST_RENDER_SPEC";
const BEGIN: &str = "riftri-render-begin";
const END: &str = "riftri-render-end";

pub(super) const BACKENDS: [riftri_storage::BackendKind; 5] = [
    riftri_storage::BackendKind::ApfsClone,
    riftri_storage::BackendKind::Reflink,
    riftri_storage::BackendKind::OverlayFs,
    riftri_storage::BackendKind::RefsBlockClone,
    riftri_storage::BackendKind::FullCopyFallback,
];

pub(super) struct Rendered {
    pub(super) output: String,
    pub(super) succeeded: bool,
}

/// Render `spec` in a child process, as `--json` or as human text.
pub(super) fn render(spec: &Value, json: bool) -> Rendered {
    let mut spec = spec.clone();
    spec["json"] = json!(json);
    let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args([
            "--exact",
            "render_hegel_tests::render_spec_in_child",
            "--nocapture",
        ])
        .env(SPEC_ENV, spec.to_string())
        .output()
        .expect("run the rendering child");
    assert!(
        output.status.success(),
        "rendering child failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("rendered output is UTF-8");
    let (_, rendered) = stdout
        .split_once(&format!("{BEGIN}\n"))
        .expect("begin marker");
    let (rendered, outcome) = rendered
        .rsplit_once(&format!("{END} "))
        .expect("end marker");
    Rendered {
        output: rendered.to_owned(),
        succeeded: outcome.starts_with("ok"),
    }
}

#[test]
fn render_spec_in_child() {
    let Some(spec) = std::env::var_os(SPEC_ENV) else {
        return;
    };
    let spec: Value =
        serde_json::from_str(spec.to_str().expect("UTF-8 spec")).expect("parse render spec");
    let json = spec["json"].as_bool().expect("json flag");
    machineln!("{BEGIN}");
    let result = match spec["kind"].as_str().expect("kind") {
        "add" => print_add_result(&add_result(&spec), json),
        "remove" => print_remove_result(
            &riftri_core::RemoveWorktreeResult {
                destination: path_at(&spec["destination"]),
                base_path: path_at(&spec["base"]),
                journal_path: path_at(&spec["journal"]),
            },
            spec["forced"].as_bool().expect("forced"),
            json,
        ),
        "move" => print_move_result(
            &riftri_core::MoveWorktreeResult {
                source: path_at(&spec["source"]),
                destination: path_at(&spec["destination"]),
                base_path: path_at(&spec["base"]),
                journal_path: path_at(&spec["journal"]),
            },
            json,
        ),
        "recovery" => super::recovery_hegel_tests::render_recovery(&spec, json),
        "collection" => super::recovery_hegel_tests::render_collection(&spec, json),
        "status" | "inventory" | "all-states" => {
            super::storage_hegel_tests::render_storage(&spec, json)
        }
        "doctor" => {
            super::print_doctor(&super::doctor_hegel_tests::report(&spec));
            Ok(())
        }
        other => panic!("unknown render kind {other}"),
    };
    machineln!("{END} {}", if result.is_ok() { "ok" } else { "failed" });
}

pub(super) fn hex_path(tc: &TestCase) -> Value {
    json!(native_path_hex(&native_path(tc)))
}

pub(super) fn hex_paths(tc: &TestCase) -> Value {
    let count = tc.draw(gs::integers::<usize>().max_value(3));
    (0..count).map(|_| hex_path(tc)).collect()
}

pub(super) fn path_at(spec: &Value) -> PathBuf {
    path_from_bytes(decode_hex(spec.as_str().expect("hex path")))
}

pub(super) fn paths_at(spec: &Value) -> Vec<PathBuf> {
    spec.as_array()
        .expect("path list")
        .iter()
        .map(path_at)
        .collect()
}

pub(super) fn count_at(spec: &Value) -> usize {
    usize::try_from(spec.as_u64().expect("count")).expect("count fits")
}

/// The paths a report lists under `field` of each entry, decoded from hex.
pub(super) fn reported_paths(entries: &Value, field: &str) -> Vec<PathBuf> {
    entries
        .as_array()
        .expect("report list")
        .iter()
        .map(|entry| path_at(&entry[format!("{field}_native_hex")]))
        .collect()
}

/// Parse one `--json` report and check what every report promises: snake_case
/// keys, and beside each lossy display string the exact bytes it stands for.
pub(super) fn parse_report(output: &str) -> Value {
    let report: Value = serde_json::from_str(output).expect("stdout is one JSON document");
    check_report_value(&report);
    report
}

/// Whether `path` is shown on a line of its own after `label`.
pub(super) fn shows(output: &str, label: &str, path: &Path) -> bool {
    output.contains(&format!("{label}{}", path.display()))
}

fn add_result(spec: &Value) -> riftri_core::AddWorktreeResult {
    let object_id = |field: &str| {
        riftri_git::ObjectId::parse(spec[field].as_str().expect("object ID")).expect("object ID")
    };
    riftri_core::AddWorktreeResult {
        destination: path_at(&spec["destination"]),
        commit: object_id("commit"),
        tree: object_id("tree"),
        base_path: path_at(&spec["base"]),
        journal_path: path_at(&spec["journal"]),
        reused_base: spec["reused"].as_bool().expect("reused"),
        backend: BACKENDS[count_at(&spec["backend"])],
        post_checkout: spec["hook"]
            .as_object()
            .map(|hook| riftri_core::PostCheckoutOutcome {
                hook: path_at(&hook["path"]),
                exit_code: hook["exit_code"]
                    .as_i64()
                    .map(|code| i32::try_from(code).expect("exit code")),
                started: hook["started"].as_bool().expect("started"),
            }),
    }
}

pub(super) fn object_id(tc: &TestCase) -> Value {
    let length = tc.draw(gs::sampled_from(vec![20, 32]));
    json!(super::encode_hex(
        &tc.draw(gs::binary().min_size(length).max_size(length))
    ))
}

#[hegel::test(process_cases(30))]
fn add_results_report_the_hook_outcome_and_exact_paths(tc: TestCase) {
    let hook = tc.draw(gs::booleans()).then(|| {
        let started = tc.draw(gs::booleans());
        let exit_code = (started && tc.draw(gs::booleans()))
            .then(|| tc.draw(gs::integers::<i32>().min_value(0).max_value(255)));
        json!({ "path": hex_path(&tc), "started": started, "exit_code": exit_code })
    });
    let spec = json!({
        "kind": "add",
        "destination": hex_path(&tc),
        "base": hex_path(&tc),
        "journal": hex_path(&tc),
        "commit": object_id(&tc),
        "tree": object_id(&tc),
        "reused": tc.draw(gs::booleans()),
        "backend": tc.draw(gs::integers::<usize>().max_value(BACKENDS.len() - 1)),
        "hook": hook,
    });

    let machine = render(&spec, true);
    assert!(machine.succeeded);
    let report = parse_report(&machine.output);
    for (reported, field) in [
        ("destination", "destination"),
        ("base_path", "base"),
        ("journal_path", "journal"),
    ] {
        assert_eq!(
            path_at(&report[format!("{reported}_native_hex")]),
            path_at(&spec[field])
        );
    }
    assert_eq!(report["commit"], spec["commit"]);
    assert_eq!(report["tree"], spec["tree"]);
    assert_eq!(report["reused_base"], spec["reused"]);
    assert_eq!(report["post_checkout"].is_null(), hook.is_none());
    if let Some(hook) = &hook {
        assert_eq!(report["post_checkout"]["exit_code"], hook["exit_code"]);
        assert_eq!(report["post_checkout"]["started"], hook["started"]);
    }

    let human = render(&spec, false);
    assert!(human.succeeded);
    assert!(shows(
        &human.output,
        "Destination: ",
        &path_at(&spec["destination"])
    ));
    let reused = spec["reused"] == true;
    assert_eq!(human.output.contains("Base: reused\n"), reused);
    assert_eq!(human.output.contains("Base: created\n"), !reused);
    assert_eq!(
        human.output.contains("post-checkout hook: "),
        hook.is_some()
    );
    if let Some(hook) = &hook {
        let outcome = match (hook["started"] == true, hook["exit_code"].as_i64()) {
            (false, _) => "could not be started".to_owned(),
            (true, Some(0)) => "ran".to_owned(),
            (true, Some(code)) => format!("exited {code}"),
            (true, None) => "terminated by a signal".to_owned(),
        };
        assert!(
            human.output.ends_with(&format!(
                "post-checkout hook: {} ({outcome})\n",
                path_at(&hook["path"]).display()
            )),
            "{}",
            human.output
        );
    }
}

#[hegel::test(process_cases(30))]
fn remove_and_move_results_name_every_path_exactly(tc: TestCase) {
    let moved = tc.draw(gs::booleans());
    let forced = tc.draw(gs::booleans());
    let spec = json!({
        "kind": if moved { "move" } else { "remove" },
        "source": hex_path(&tc),
        "destination": hex_path(&tc),
        "base": hex_path(&tc),
        "journal": hex_path(&tc),
        "forced": forced,
    });

    let machine = render(&spec, true);
    assert!(machine.succeeded);
    let report = parse_report(&machine.output);
    let mut fields = vec![
        ("destination", "destination"),
        ("base_path", "base"),
        ("journal_path", "journal"),
    ];
    if moved {
        fields.push(("source", "source"));
    } else {
        assert_eq!(report["forced"], forced);
    }
    for (reported, field) in fields {
        assert_eq!(
            path_at(&report[format!("{reported}_native_hex")]),
            path_at(&spec[field])
        );
    }

    let human = render(&spec, false);
    assert!(human.succeeded);
    let headline = human.output.lines().next().expect("headline");
    assert_eq!(headline.starts_with("Moved "), moved);
    assert_eq!(headline.starts_with("Force-removed "), !moved && forced);
    assert_eq!(headline.starts_with("Removed "), !moved && !forced);
    assert_eq!(
        shows(&human.output, "Source: ", &path_at(&spec["source"])),
        moved
    );
    for (label, field) in [
        ("Destination: ", "destination"),
        ("Retained immutable base: ", "base"),
        ("Journal: ", "journal"),
    ] {
        assert!(shows(&human.output, label, &path_at(&spec[field])));
    }
}
