use std::path::PathBuf;

use super::{ProtectedBase, index_gc_protections};

fn record(base_path: &std::path::Path, operation_id: &str, reason: &str) -> ProtectedBase {
    ProtectedBase {
        base_path: base_path.to_path_buf(),
        operation_id: operation_id.to_owned(),
        reason: reason.to_owned(),
    }
}

fn fixture() -> (Vec<PathBuf>, Vec<ProtectedBase>, Vec<(PathBuf, String)>) {
    let mut bases = vec![
        PathBuf::from("/bases/a"),
        PathBuf::from("/bases/b"),
        PathBuf::from("/bases/unprotected"),
        PathBuf::from("/bases/caf\u{e9}"),
        PathBuf::from("/bases/cafe\u{301}"),
        PathBuf::from("/bases//a/"),
    ];
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        bases.push(PathBuf::from(std::ffi::OsString::from_vec(
            b"/bases/\xff".to_vec(),
        )));
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        bases.push(PathBuf::from(std::ffi::OsString::from_wide(&[0xd800])));
    }
    let mut protections = bases
        .iter()
        .enumerate()
        .filter(|(n, _)| *n != 2)
        .map(|(n, base)| record(base, &format!("add-{n}"), "active add"))
        .collect::<Vec<_>>();
    protections.push(protections[0].clone());
    protections.push(record(&bases[0], "compact", "old compaction base"));
    protections.push(record(&bases[1], "compact", "new compaction base"));
    protections.push(record(
        &PathBuf::from("/bases/no-marker"),
        "absent",
        "not inventoried",
    ));
    let pending = vec![
        (bases[0].clone(), "gc-a".to_owned()),
        (bases[1].clone(), "gc-b".to_owned()),
        (bases[0].clone(), "gc-a".to_owned()),
        (PathBuf::from("/bases/no-marker"), "gc-absent".to_owned()),
    ];
    bases.sort_unstable();
    bases.dedup(); // retained_base_paths always supplies unique sorted paths.
    (bases, protections, pending)
}

fn legacy(
    base: &std::path::Path,
    protections: &[ProtectedBase],
    pending: &[(PathBuf, String)],
) -> Vec<ProtectedBase> {
    let mut result = protections
        .iter()
        .filter(|p| p.base_path == base)
        .cloned()
        .collect::<Vec<_>>();
    result.extend(
        pending
            .iter()
            .filter(|(path, _)| path == base)
            .map(|(path, id)| {
                record(
                    path,
                    id,
                    "an unfinished garbage-collection journal already claims this base",
                )
            }),
    );
    result
}

#[test]
fn indexing_preserves_every_reason_duplicate_order_and_native_path_equivalence() {
    let (bases, protections, pending) = fixture();
    let mut indexed = index_gc_protections(protections.clone(), pending.clone());
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for base in &bases {
        let old = legacy(base, &protections, &pending);
        let new = indexed.remove(base).unwrap_or_default();
        assert_eq!(new, old, "{base:?}");
        expected.extend(old);
        actual.extend(new);
    }
    for report in [&mut actual, &mut expected] {
        report.sort_unstable_by(|a, b| {
            (&a.base_path, &a.operation_id).cmp(&(&b.base_path, &b.operation_id))
        });
        report.dedup();
    }
    assert_eq!(actual, expected);
    assert!(indexed.contains_key(&PathBuf::from("/bases/no-marker")));
}

#[test]
fn empty_and_pending_only_snapshots_are_supported() {
    assert!(index_gc_protections(vec![], []).is_empty());
    let base = PathBuf::from("/bases/pending");
    let pending = vec![(base.clone(), "gc-pending".to_owned())];
    assert_eq!(
        index_gc_protections(vec![], pending.clone())
            .remove(&base)
            .unwrap(),
        legacy(&base, &[], &pending)
    );
}

#[test]
#[ignore = "manual grouping-only benchmark; includes index construction, not a CLI latency claim"]
fn reports_gc_protection_grouping_latency() {
    for count in [8, 1000] {
        let bases = (0..count)
            .map(|n| PathBuf::from(format!("/bases/{n:04}")))
            .collect::<Vec<_>>();
        let protections = bases
            .iter()
            .map(|base| record(base, "add", "active add"))
            .collect::<Vec<_>>();
        for round in 0..4 {
            for indexed in if round % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            } {
                // Both lanes own the already-loaded journal records before timing.
                let input = protections.clone();
                let start = std::time::Instant::now();
                let mut rows = Vec::new();
                if indexed {
                    let mut index = index_gc_protections(input, []);
                    for base in &bases {
                        rows.extend(index.remove(base).unwrap());
                    }
                } else {
                    for base in &bases {
                        rows.extend(legacy(base, &input, &[]));
                    }
                }
                let elapsed = start.elapsed();
                assert_eq!(rows, protections);
                println!(
                    "count={count} round={round} indexed={indexed} elapsed_us={}",
                    elapsed.as_micros()
                );
                std::hint::black_box(rows);
            }
        }
    }
}
