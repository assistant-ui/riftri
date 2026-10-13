#![cfg(target_os = "macos")]

#[allow(dead_code)]
mod support;

use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use hegel::TestCase;
use hegel::generators as gs;
use riftri_storage::ApfsCloner;

use support::{Fixture, draw_base, draw_name, files, set_mode, snapshot, with_modes};

// Each case builds and clones real trees, so run fewer of them.
#[hegel::test(test_cases = 40)]
fn a_clone_reproduces_the_tree_under_any_destination_name(tc: TestCase) {
    let fixture = Fixture::new();
    let base = draw_base(&tc, &fixture);
    let name = if tc.draw(gs::booleans()) {
        OsString::from("view")
    } else {
        draw_name(&tc)
    };
    let view = fixture.path().join(&name);
    let before = snapshot(fixture.path());
    let base_before = snapshot(&base);

    match ApfsCloner::clone_tree(&base, &view) {
        Ok(()) => {
            assert_eq!(snapshot(&view), base_before);
            assert_eq!(snapshot(&base), base_before);
            for file in files(&base_before) {
                let cloned = fs::metadata(view.join(&file)).expect("cloned file");
                let original = fs::metadata(base.join(&file)).expect("base file");
                // A hard link would share later writes with the base.
                assert_ne!(cloned.ino(), original.ino(), "{}", file.display());
            }
        }
        Err(error) => {
            // The volume may refuse a generated name (non-UTF-8, or an alias
            // of `base`), but never a plain one, and never half-way.
            assert_ne!(name, "view", "{error}");
            assert_eq!(snapshot(fixture.path()), before, "{error}");
        }
    }
}

// Each case builds and clones real trees, so run fewer of them.
#[hegel::test(test_cases = 40)]
fn a_writable_clone_of_a_protected_base_gets_normal_checkout_modes(tc: TestCase) {
    let fixture = Fixture::new();
    let base = draw_base(&tc, &fixture);
    let view = fixture.path().join("view");
    let two_pass = fixture.path().join("two-pass");
    let original = snapshot(&base);

    ApfsCloner::make_tree_read_only(&base).expect("protect base");
    let read_only = |mode| mode & !0o222;
    let protected = with_modes(&original, read_only, read_only);
    assert_eq!(snapshot(&base), protected);
    ApfsCloner::make_tree_read_only(&base).expect("protect base again");
    assert_eq!(snapshot(&base), protected);

    let write_bits = support::umask_write_bits(fixture.path());
    let expected = with_modes(
        &protected,
        |mode| mode | write_bits,
        |mode| mode | 0o700 | write_bits,
    );
    ApfsCloner::clone_tree_owner_writable(&base, &view).expect("writable clone");
    assert_eq!(snapshot(&view), expected);

    ApfsCloner::clone_tree(&base, &two_pass).expect("read-only clone");
    assert_eq!(snapshot(&two_pass), protected);
    ApfsCloner::make_tree_owner_writable(&two_pass).expect("restore write access");
    assert_eq!(snapshot(&two_pass), expected);
    ApfsCloner::make_tree_owner_writable(&two_pass).expect("restore write access again");
    assert_eq!(snapshot(&two_pass), expected);

    assert_eq!(snapshot(&base), protected);
}

// Each case builds and clones real trees, so run fewer of them.
#[hegel::test(test_cases = 40)]
fn changes_to_one_tree_never_reach_its_base_or_sibling_clones(tc: TestCase) {
    let fixture = Fixture::new();
    let base = draw_base(&tc, &fixture);
    ApfsCloner::make_tree_owner_writable(&base).expect("writable base");
    let trees = [
        base.clone(),
        fixture.path().join("first"),
        fixture.path().join("second"),
    ];
    ApfsCloner::clone_tree(&base, &trees[1]).expect("first clone");
    ApfsCloner::clone_tree_owner_writable(&base, &trees[2]).expect("second clone");
    let before = trees.each_ref().map(|tree| snapshot(tree));

    let changed = tc.draw(gs::integers::<usize>().max_value(2));
    let candidates = files(&before[changed]);
    for step in 0..tc.draw(gs::integers::<usize>().min_value(1).max_value(6)) {
        if candidates.is_empty() || tc.draw(gs::booleans()) {
            let created = trees[changed].join(format!("private-change-{step}"));
            fs::write(created, tc.draw(gs::binary().max_size(64))).expect("create private file");
        } else {
            let file = tc.draw(gs::sampled_from(candidates.clone()));
            change_file(&tc, &trees[changed].join(file));
        }
    }

    for untouched in (0..3).filter(|index| *index != changed) {
        assert_eq!(snapshot(&trees[untouched]), before[untouched]);
    }
}

/// Apply one generated edit to `path`, which an earlier step may have removed.
fn change_file(tc: &TestCase, path: &Path) {
    if !path.exists() {
        return;
    }
    // A clone keeps its base's mode, which need not include owner write.
    set_mode(path, 0o600);
    match tc.draw(gs::integers::<u8>().max_value(2)) {
        0 => fs::write(path, tc.draw(gs::binary().max_size(64))).expect("overwrite"),
        1 => fs::OpenOptions::new()
            .append(true)
            .open(path)
            .and_then(|mut file| file.write_all(&tc.draw(gs::binary().max_size(64))))
            .expect("append"),
        _ => fs::remove_file(path).expect("remove"),
    }
}
