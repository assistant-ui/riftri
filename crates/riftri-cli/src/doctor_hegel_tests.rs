//! Property tests for the `doctor` report in both renderings.

use hegel::TestCase;
use hegel::generators as gs;
use riftri_core::{DestinationReadinessStatus, OverlayFsHelperReadiness};
use serde_json::{Value, json};

use super::doctor_json;
use super::hegel_tests::check_report_value;
use super::render_hegel_tests::{BACKENDS, count_at, hex_path, object_id, path_at, render, shows};
use crate::hegel_tests::process_cases;

const STATUSES: [(DestinationReadinessStatus, &str); 3] = [
    (DestinationReadinessStatus::Ready, "ready"),
    (
        DestinationReadinessStatus::NeedsActivation,
        "needs activation",
    ),
    (DestinationReadinessStatus::Blocked, "blocked"),
];

const HELPERS: [(OverlayFsHelperReadiness, &str); 4] = [
    (OverlayFsHelperReadiness::NotApplicable, "not applicable"),
    (OverlayFsHelperReadiness::NotRequired, "not required"),
    (OverlayFsHelperReadiness::Ready, "required and verified"),
    (
        OverlayFsHelperReadiness::Unavailable,
        "required but unavailable",
    ),
];

const READINESS_BLOCKERS: [&str; 3] = ["repository-activation", "copy-on-write", "destination"];

const COMPATIBILITY_BLOCKERS: [riftri_core::RepositoryCompatibilityBlockerKind; 6] = [
    riftri_core::RepositoryCompatibilityBlockerKind::InTreeAttributes,
    riftri_core::RepositoryCompatibilityBlockerKind::EffectiveAttributes,
    riftri_core::RepositoryCompatibilityBlockerKind::GitLfs,
    riftri_core::RepositoryCompatibilityBlockerKind::Submodules,
    riftri_core::RepositoryCompatibilityBlockerKind::SparseCheckout,
    riftri_core::RepositoryCompatibilityBlockerKind::CheckoutConfiguration,
];

const CAPABILITY_STATUSES: [riftri_storage::CapabilityStatus; 3] = [
    riftri_storage::CapabilityStatus::Supported,
    riftri_storage::CapabilityStatus::Unsupported,
    riftri_storage::CapabilityStatus::Unavailable,
];

fn index(tc: &TestCase, length: usize) -> usize {
    tc.draw(gs::integers::<usize>().max_value(length - 1))
}

fn text(tc: &TestCase) -> String {
    tc.draw(gs::text().max_size(24))
}

fn maybe(tc: &TestCase, value: impl FnOnce(&TestCase) -> Value) -> Value {
    if tc.draw(gs::booleans()) {
        value(tc)
    } else {
        Value::Null
    }
}

fn list(tc: &TestCase, item: impl Fn(&TestCase) -> Value) -> Value {
    (0..index(tc, 3)).map(|_| item(tc)).collect()
}

fn items(spec: &Value) -> &[Value] {
    spec.as_array().expect("list")
}

fn string(spec: &Value) -> String {
    spec.as_str().expect("text").to_owned()
}

fn diagnostic<T>(
    spec: &Value,
    error: &Value,
    value: impl FnOnce(&Value) -> T,
) -> riftri_core::Diagnostic<T> {
    riftri_core::Diagnostic {
        available: !spec.is_null(),
        value: (!spec.is_null()).then(|| value(spec)),
        error: error.as_str().map(str::to_owned),
    }
}

fn object_id_at(spec: &Value) -> riftri_git::ObjectId {
    riftri_git::ObjectId::parse(spec.as_str().expect("object ID")).expect("object ID")
}

pub(super) fn report(spec: &Value) -> riftri_core::DoctorReport {
    riftri_core::DoctorReport {
        project_stage: "native-cow-with-repository-activation",
        operating_system: "test-os",
        architecture: "test-arch",
        cow_backend_active: spec["cow"].as_bool().expect("cow"),
        repository_enabled: spec["enabled"].as_bool(),
        git_shim_active: spec["shim"].as_bool().expect("shim"),
        git: diagnostic(&spec["git"], &spec["error"], |git| riftri_git::GitInfo {
            command: path_at(&git["command"]),
            version: string(&git["version"]),
        }),
        repository: diagnostic(&spec["repository"], &spec["error"], |repository| {
            riftri_git::RepositoryInfo {
                root: (!repository["root"].is_null()).then(|| path_at(&repository["root"])),
                identity: riftri_git::RepositoryIdentity {
                    common_git_dir: path_at(&repository["common"]),
                },
                is_bare: repository["root"].is_null(),
                head_commit: (!repository["head"].is_null())
                    .then(|| object_id_at(&repository["head"])),
                head_tree: None,
                clean: repository["clean"].as_bool(),
            }
        }),
        repository_compatibility: diagnostic(
            &spec["compatibility"],
            &spec["error"],
            |compatibility| riftri_core::RepositoryCompatibilityReport {
                commit: object_id_at(&compatibility["commit"]),
                tree: object_id_at(&compatibility["tree"]),
                compatible: compatibility["compatible"].as_bool().expect("compatible"),
                blockers: items(&compatibility["blockers"])
                    .iter()
                    .map(|blocker| riftri_core::RepositoryCompatibilityBlocker {
                        kind: COMPATIBILITY_BLOCKERS[count_at(&blocker["kind"])],
                        explanation: string(&blocker["explanation"]),
                    })
                    .collect(),
            },
        ),
        destination_readiness: riftri_core::DestinationReadiness {
            destination: path_at(&spec["destination"]),
            status: STATUSES[count_at(&spec["status"])].0,
            backend: spec["backend"]
                .as_u64()
                .map(|_| BACKENDS[count_at(&spec["backend"])]),
            copy_on_write: spec["cow"].as_bool().expect("cow"),
            overlayfs_helper: HELPERS[count_at(&spec["helper"])].0,
            blockers: items(&spec["blockers"])
                .iter()
                .map(|blocker| riftri_core::DestinationReadinessBlocker {
                    kind: READINESS_BLOCKERS[count_at(&blocker["kind"])],
                    explanation: string(&blocker["explanation"]),
                    remedy: string(&blocker["remedy"]),
                })
                .collect(),
            next_command: spec["next_command"].as_str().map(str::to_owned),
        },
        storage_capabilities: items(&spec["capabilities"])
            .iter()
            .map(|capability| riftri_storage::BackendCapability {
                kind: BACKENDS[count_at(&capability["kind"])],
                status: CAPABILITY_STATUSES[count_at(&capability["status"])],
                volume: capability["volume"].as_object().map(|volume| {
                    riftri_storage::DestinationVolume {
                        requested_path: path_at(&volume["requested"]),
                        probe_path: path_at(&volume["probe"]),
                        identity: riftri_storage::VolumeIdentity {
                            device_id: volume["device"].as_u64().expect("device"),
                            filesystem: string(&volume["filesystem"]),
                        },
                        read_only: volume["read_only"].as_bool().expect("read only"),
                    }
                }),
                explanation: string(&capability["explanation"]),
                requires_explicit_fallback: capability["fallback"].as_bool().expect("fallback"),
            })
            .collect(),
    }
}

fn doctor_spec(tc: &TestCase) -> Value {
    json!({
        "kind": "doctor",
        "destination": hex_path(tc),
        "status": index(tc, STATUSES.len()),
        "backend": maybe(tc, |tc| json!(index(tc, BACKENDS.len()))),
        "cow": tc.draw(gs::booleans()),
        "helper": index(tc, HELPERS.len()),
        "blockers": list(tc, |tc| json!({
            "kind": index(tc, READINESS_BLOCKERS.len()),
            "explanation": text(tc),
            "remedy": text(tc),
        })),
        "next_command": maybe(tc, |tc| json!(text(tc))),
        "enabled": maybe(tc, |tc| json!(tc.draw(gs::booleans()))),
        "shim": tc.draw(gs::booleans()),
        "error": maybe(tc, |tc| json!(text(tc))),
        "git": maybe(tc, |tc| json!({ "command": hex_path(tc), "version": text(tc) })),
        "repository": maybe(tc, |tc| json!({
            "root": maybe(tc, hex_path),
            "common": hex_path(tc),
            "head": maybe(tc, object_id),
            "clean": maybe(tc, |tc| json!(tc.draw(gs::booleans()))),
        })),
        "compatibility": maybe(tc, |tc| json!({
            "commit": object_id(tc),
            "tree": object_id(tc),
            "compatible": tc.draw(gs::booleans()),
            "blockers": list(tc, |tc| json!({
                "kind": index(tc, COMPATIBILITY_BLOCKERS.len()),
                "explanation": text(tc),
            })),
        })),
        "capabilities": list(tc, |tc| json!({
            "kind": index(tc, BACKENDS.len()),
            "status": index(tc, CAPABILITY_STATUSES.len()),
            "explanation": text(tc),
            "fallback": tc.draw(gs::booleans()),
            "volume": maybe(tc, |tc| json!({
                "requested": hex_path(tc),
                "probe": hex_path(tc),
                "device": tc.draw(gs::integers::<u64>()),
                "filesystem": text(tc),
                "read_only": tc.draw(gs::booleans()),
            })),
        })),
    })
}

#[hegel::test]
fn doctor_json_keeps_every_key_and_exact_paths(tc: TestCase) {
    let spec = doctor_spec(&tc);

    let document = doctor_json(&report(&spec));

    check_report_value(&document);
    let readiness = document["destination_readiness"]
        .as_object()
        .expect("destination readiness");
    // Present on every path, null when nothing applies, so a strictly typed
    // consumer of a ready report can read a blocked one.
    assert_eq!(readiness["backend"].is_null(), spec["backend"].is_null());
    assert_eq!(readiness["next_command"], spec["next_command"]);
    assert_eq!(
        path_at(&readiness["destination_native_hex"]),
        path_at(&spec["destination"])
    );
    assert_eq!(document["git"]["available"], !spec["git"].is_null());
    if !spec["git"].is_null() {
        assert_eq!(
            document["git"]["value"]["command_native_hex"],
            spec["git"]["command"]
        );
    }
    assert_eq!(
        document["repository"]["available"],
        !spec["repository"].is_null()
    );
    if !spec["repository"].is_null() {
        let repository = &document["repository"]["value"];
        assert_eq!(repository["root_native_hex"], spec["repository"]["root"]);
        assert_eq!(
            repository["identity"]["common_git_dir_native_hex"],
            spec["repository"]["common"]
        );
        assert_eq!(repository["head_commit"], spec["repository"]["head"]);
    }
    let capabilities = items(&document["storage_capabilities"]);
    assert_eq!(capabilities.len(), items(&spec["capabilities"]).len());
    for (reported, capability) in capabilities.iter().zip(items(&spec["capabilities"])) {
        assert_eq!(
            reported.get("volume").is_some(),
            !capability["volume"].is_null()
        );
        if let Some(volume) = reported.get("volume") {
            assert_eq!(
                volume["requested_path_native_hex"],
                capability["volume"]["requested"]
            );
            assert_eq!(
                volume["probe_path_native_hex"],
                capability["volume"]["probe"]
            );
        }
    }
}

#[hegel::test(process_cases(40))]
fn doctor_text_states_each_fact_of_the_report(tc: TestCase) {
    let spec = doctor_spec(&tc);

    let human = render(&spec, false);

    assert!(human.succeeded);
    let output = &human.output;
    let line = |text: String| output.contains(&format!("{text}\n"));
    assert!(line(format!(
        "Destination readiness: {}",
        STATUSES[count_at(&spec["status"])].1
    )));
    assert!(shows(
        output,
        "Destination: ",
        &path_at(&spec["destination"])
    ));
    assert!(line(format!(
        "Selected backend: {}",
        match spec["backend"].as_u64() {
            Some(_) => BACKENDS[count_at(&spec["backend"])].display_name(),
            None => "none",
        }
    )));
    assert!(line(format!(
        "Copy-on-write: {}",
        if spec["cow"] == true {
            "verified"
        } else {
            "unavailable"
        }
    )));
    assert!(line(format!(
        "OverlayFS helper: {}",
        HELPERS[count_at(&spec["helper"])].1
    )));
    let blockers = items(&spec["blockers"]);
    assert_eq!(
        line("Readiness blockers: none".to_owned()),
        blockers.is_empty()
    );
    for blocker in blockers {
        assert!(line(format!(
            "- {}: {}\n  Fix: {}",
            READINESS_BLOCKERS[count_at(&blocker["kind"])],
            string(&blocker["explanation"]),
            string(&blocker["remedy"])
        )));
    }
    assert_eq!(
        output.contains("Next command: "),
        !spec["next_command"].is_null()
    );
    assert!(line(format!(
        "Repository enabled: {}",
        match spec["enabled"].as_bool() {
            Some(true) => "yes",
            Some(false) => "no",
            None => "unknown",
        }
    )));
    assert!(line(format!(
        "Process-scoped Git interception: {}",
        if spec["shim"] == true {
            "active"
        } else {
            "inactive"
        }
    )));

    let error = spec["error"].as_str().unwrap_or("unknown error");
    match spec["git"].as_object() {
        Some(git) => assert!(line(format!(
            "Git: {} ({})",
            string(&git["version"]),
            path_at(&git["command"]).display()
        ))),
        None => assert!(line(format!("Git: unavailable ({error})"))),
    }
    match spec["repository"].as_object() {
        Some(repository) => {
            match repository["root"].as_str() {
                Some(_) => assert!(shows(output, "Repository: ", &path_at(&repository["root"]))),
                None => assert!(line("Repository: bare".to_owned())),
            }
            assert!(shows(
                output,
                "Common Git directory: ",
                &path_at(&repository["common"])
            ));
            assert!(line(format!(
                "HEAD: {}",
                repository["head"].as_str().unwrap_or("unborn")
            )));
            assert!(line(match repository["clean"].as_bool() {
                Some(clean) => format!("Working tree clean: {clean}"),
                None => "Working tree clean: not applicable (bare repository)".to_owned(),
            }));
        }
        None => assert!(line(format!("Repository: unavailable ({error})"))),
    }
    match spec["compatibility"].as_object() {
        Some(compatibility) => {
            assert!(line(format!(
                "Repository checkout compatibility (HEAD): {}",
                if compatibility["compatible"] == true {
                    "supported"
                } else {
                    "blocked"
                }
            )));
            for blocker in items(&compatibility["blockers"]) {
                assert!(line(format!(
                    "- {}: {}",
                    COMPATIBILITY_BLOCKERS[count_at(&blocker["kind"])].as_str(),
                    string(&blocker["explanation"])
                )));
            }
        }
        None => assert!(line(format!(
            "Repository checkout compatibility (HEAD): unavailable ({error})"
        ))),
    }
    for capability in items(&spec["capabilities"]) {
        assert!(line(format!(
            "- {} ({}): {}",
            BACKENDS[count_at(&capability["kind"])].display_name(),
            CAPABILITY_STATUSES[count_at(&capability["status"])].display_name(),
            string(&capability["explanation"])
        )));
    }
}
