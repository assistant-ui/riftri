//! macOS builds carry the Linux and Windows backends only as refusals.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

use hegel::TestCase;
use hegel::generators as gs;

use super::{
    BackendKind, CapabilityStatus, OverlayFsLayout, OverlayFsMountIdentity, OverlayFsMountProfile,
    OverlayFsMounter, ReflinkCloner, RefsBlockCloner, StorageError,
};

const REFLINK: &str = "Linux reflinking";
const OVERLAYFS: &str = "Linux OverlayFS";
const REFS: &str = "Windows ReFS block cloning";

/// Arbitrary bytes under `root`. A leading separator makes the path absolute
/// instead, which the refusals must tolerate as well.
fn draw_path(tc: &TestCase, root: &std::path::Path) -> PathBuf {
    root.join(OsString::from_vec(tc.draw(gs::binary().max_size(12))))
}

#[hegel::test]
fn foreign_backends_refuse_every_operation_without_touching_the_paths(tc: TestCase) {
    let fixture = tempfile::tempdir().expect("fixture directory");
    let path = || draw_path(&tc, fixture.path());
    let (source, destination) = (path(), path());
    let layout = OverlayFsLayout {
        root: path(),
        lower: path(),
        upper: path(),
        work: path(),
        merged: path(),
    };
    let identity = OverlayFsMountIdentity {
        profile: tc.draw(gs::sampled_from(vec![
            OverlayFsMountProfile::RootlessUserXattr,
            OverlayFsMountProfile::PrivilegedTrustedXattr,
        ])),
        boot_id: tc.draw(gs::text()),
        mount_namespace_device: tc.draw(gs::integers::<u64>()),
        mount_namespace_inode: tc.draw(gs::integers::<u64>()),
        mount_id: tc.draw(gs::integers::<u64>()),
    };
    let context = identity.context();
    let token = tc.draw(gs::text());
    let (root, lower, merged) = (layout.root(), layout.lower(), layout.merged());
    assert_eq!(
        [root, lower, layout.upper(), layout.work(), merged],
        [
            &layout.root,
            &layout.lower,
            &layout.upper,
            &layout.work,
            &layout.merged
        ]
    );

    let refusals = [
        (
            REFLINK,
            ReflinkCloner::clone_tree_owner_writable(&source, &destination),
        ),
        (REFLINK, ReflinkCloner::clone_tree(&source, &destination)),
        (REFLINK, ReflinkCloner::make_tree_read_only(&source)),
        (REFLINK, ReflinkCloner::make_tree_owner_writable(&source)),
        (
            REFS,
            RefsBlockCloner::clone_tree_owner_writable(&source, &destination),
        ),
        (REFS, RefsBlockCloner::clone_tree(&source, &destination)),
        (REFS, RefsBlockCloner::make_tree_read_only(&source)),
        (REFS, RefsBlockCloner::make_tree_owner_writable(&source)),
        (
            OVERLAYFS,
            OverlayFsMounter::current_mount_context().map(drop),
        ),
        (
            OVERLAYFS,
            OverlayFsMounter::prepare(root, lower, merged).map(drop),
        ),
        (
            OVERLAYFS,
            OverlayFsMounter::load(root, lower, merged).map(drop),
        ),
        (
            OVERLAYFS,
            OverlayFsMounter::load_for_recovery(root, lower, merged).map(drop),
        ),
        (
            OVERLAYFS,
            OverlayFsMounter::load_for_remount(root, lower, merged, &context).map(drop),
        ),
        (OVERLAYFS, OverlayFsMounter::mount(&layout).map(drop)),
        (
            OVERLAYFS,
            OverlayFsMounter::make_view_owner_writable(&layout, &identity),
        ),
        (OVERLAYFS, OverlayFsMounter::arm_recovery(&layout, &token)),
        (
            OVERLAYFS,
            OverlayFsMounter::recover_mount(&layout, &context, &token).map(drop),
        ),
        (OVERLAYFS, OverlayFsMounter::clear_recovery(&layout, &token)),
        (
            OVERLAYFS,
            OverlayFsMounter::remove_unmounted_private_layers(&layout, &context),
        ),
        (
            OVERLAYFS,
            OverlayFsMounter::mount_state(&layout, &identity).map(drop),
        ),
        (
            OVERLAYFS,
            OverlayFsMounter::unmount(&layout, &identity).map(drop),
        ),
        (
            OVERLAYFS,
            OverlayFsMounter::remove_private_layers(&layout, &identity),
        ),
        (
            OVERLAYFS,
            OverlayFsMounter::remove_abandoned_probe_root(root).map(drop),
        ),
    ];

    for (expected, refusal) in refusals {
        match refusal {
            Err(StorageError::UnsupportedPlatform { backend }) => assert_eq!(backend, expected),
            other => panic!("{expected} must be refused on macOS: {other:?}"),
        }
    }
    assert_eq!(
        std::fs::read_dir(fixture.path())
            .expect("read fixture")
            .count(),
        0
    );
}

#[hegel::test]
fn foreign_backend_probes_are_unsupported_for_any_destination(tc: TestCase) {
    let fixture = tempfile::tempdir().expect("fixture directory");
    let destination = draw_path(&tc, fixture.path());

    let probes = [
        (BackendKind::Reflink, ReflinkCloner::probe(&destination)),
        (
            BackendKind::OverlayFs,
            OverlayFsMounter::probe(&destination),
        ),
        (
            BackendKind::OverlayFs,
            OverlayFsMounter::probe_current_namespace(&destination),
        ),
        (
            BackendKind::RefsBlockClone,
            RefsBlockCloner::probe(&destination),
        ),
    ];

    for (kind, capability) in probes {
        assert_eq!(capability.kind, kind);
        // A definite "no", so selection never waits on or retries the probe.
        assert_eq!(capability.status, CapabilityStatus::Unsupported);
        assert_eq!(capability.volume, None);
        assert!(!capability.requires_explicit_fallback);
        assert!(
            capability
                .explanation
                .ends_with(&destination.display().to_string()),
            "{capability:?}"
        );
    }
}
