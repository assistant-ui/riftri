#![cfg(target_os = "linux")]

use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::path::Path;
use std::process::Command;

use riftri_storage::{
    BackendKind, CapabilityStatus, OverlayFsMountContext, OverlayFsMountIdentity,
    OverlayFsMountState, OverlayFsMounter, OverlayFsRecoveryState, StorageError,
};
use tempfile::tempdir;

#[test]
fn active_probe_verifies_copy_up_without_leaving_artifacts() {
    let fixture = tempdir().expect("fixture directory");
    assert_clean_probe(fixture.path());
}

#[test]
fn active_probe_accepts_mount_option_delimiters_in_destination_path() {
    let fixture = tempdir().expect("fixture directory");
    let destination = fixture.path().join("comma,colon:backslash\\ and space");
    fs::create_dir(&destination).expect("unusual destination directory");
    assert_clean_probe(&destination);
}

#[test]
fn active_probe_accepts_non_utf8_destination_path() {
    let fixture = tempdir().expect("fixture directory");
    let destination = fixture
        .path()
        .join(OsString::from_vec(b"non-utf8-\xff".to_vec()));
    fs::create_dir(&destination).expect("non-UTF-8 destination directory");
    assert_clean_probe(&destination);
}

#[test]
fn caller_namespace_probe_verifies_persistent_mount_permission() {
    let fixture = tempdir().expect("fixture directory");
    let before = fs::read_dir(fixture.path())
        .expect("read fixture before caller probe")
        .count();

    let capability = OverlayFsMounter::probe_current_namespace(fixture.path());

    if overlayfs_mounts_required() {
        assert_eq!(capability.status, CapabilityStatus::Supported);
        assert!(
            capability.explanation.contains("caller-visible"),
            "unexpected capability explanation: {}",
            capability.explanation
        );
    }
    let after = fs::read_dir(fixture.path())
        .expect("read fixture after caller probe")
        .count();
    assert_eq!(before, after, "caller probe left a visible artifact");
}

#[test]
fn concurrent_isolated_and_caller_probes_release_their_mounts() {
    if !overlayfs_mounts_required() {
        return;
    }
    let start = std::sync::Barrier::new(4);
    std::thread::scope(|scope| {
        for worker in 0..4 {
            let start = &start;
            scope.spawn(move || {
                let fixture = tempdir().expect("worker fixture");
                start.wait();
                for iteration in 0..16 {
                    let capability = if (worker + iteration) % 2 == 0 {
                        OverlayFsMounter::probe(fixture.path())
                    } else {
                        OverlayFsMounter::probe_current_namespace(fixture.path())
                    };
                    assert_eq!(
                        capability.status,
                        CapabilityStatus::Supported,
                        "worker {worker}, iteration {iteration}: {}",
                        capability.explanation
                    );
                    assert_eq!(
                        fs::read_dir(fixture.path()).unwrap().count(),
                        0,
                        "concurrent probe left an artifact"
                    );
                }
            });
        }
    });
}

#[test]
fn durable_layout_mounts_directly_at_the_requested_view() {
    if !overlayfs_mounts_required() {
        return;
    }
    let fixture = tempdir().expect("fixture directory");
    let lower = fixture.path().join("lower,with:delimiters");
    let layout_root = fixture.path().join("state/overlays/v1/operation-1");
    let merged = fixture.path().join(OsString::from_vec(
        b"worktree,colon:space and non-utf8-\xff".to_vec(),
    ));
    fs::create_dir_all(&lower).expect("lower directory");
    fs::create_dir(&merged).expect("merged directory");
    fs::write(lower.join("payload"), b"base").expect("lower payload");

    let layout =
        OverlayFsMounter::prepare(&layout_root, &lower, &merged).expect("prepare durable layout");
    assert_eq!(layout.root(), layout_root);
    assert_eq!(layout.lower(), lower);
    assert_eq!(layout.upper(), layout_root.join("upper"));
    assert_eq!(layout.work(), layout_root.join("work"));
    assert_eq!(layout.merged(), merged);

    let identity = OverlayFsMounter::mount(&layout).expect("mount durable view");
    assert_eq!(
        OverlayFsMounter::mount_state(&layout, &identity).expect("inspect durable view"),
        OverlayFsMountState::Active
    );
    assert_eq!(
        fs::read(merged.join("payload")).expect("read lower"),
        b"base"
    );

    fs::write(merged.join("payload"), b"view").expect("copy up private write");
    assert_eq!(
        fs::read(lower.join("payload")).expect("read lower"),
        b"base"
    );
    assert_eq!(
        fs::read(layout.upper().join("payload")).expect("read upper"),
        b"view"
    );

    assert!(OverlayFsMounter::unmount(&layout, &identity).expect("unmount durable view"));
    assert_eq!(
        OverlayFsMounter::mount_state(&layout, &identity).expect("inspect unmounted view"),
        OverlayFsMountState::Absent
    );
    OverlayFsMounter::remove_private_layers(&layout, &identity)
        .expect("remove durable private layers");
    assert!(!layout_root.exists());
}

#[test]
fn unmount_refuses_a_mount_identity_mismatch() {
    if !overlayfs_mounts_required() {
        return;
    }
    let fixture = tempdir().expect("fixture directory");
    let lower = fixture.path().join("lower");
    let layout_root = fixture.path().join("layout");
    let merged = fixture.path().join("merged");
    fs::create_dir(&lower).expect("lower directory");
    fs::create_dir(&merged).expect("merged directory");

    let layout =
        OverlayFsMounter::prepare(&layout_root, &lower, &merged).expect("prepare durable layout");
    let identity = OverlayFsMounter::mount(&layout).expect("mount durable view");
    let mut wrong_identity = identity.clone();
    wrong_identity.mount_id = identity.mount_id.saturating_add(1);

    let error = OverlayFsMounter::unmount(&layout, &wrong_identity)
        .expect_err("mismatched identity must not unmount");
    assert!(matches!(error, StorageError::OverlayFsMountConflict { .. }));
    assert_eq!(
        OverlayFsMounter::mount_state(&layout, &identity).expect("original mount remains"),
        OverlayFsMountState::Active
    );

    OverlayFsMounter::unmount(&layout, &identity).expect("unmount original view");
    OverlayFsMounter::remove_private_layers(&layout, &identity)
        .expect("remove durable private layers");
}

#[test]
fn mount_recovery_survives_creator_process_exit() {
    if !overlayfs_mounts_required() {
        return;
    }
    let fixture = tempdir().expect("fixture directory");
    let lower = fixture.path().join("lower");
    let layout_root = fixture.path().join("layout");
    let merged = fixture.path().join("merged");
    let identity_path = fixture.path().join("mount-identity.json");
    fs::create_dir(&lower).expect("lower directory");
    fs::create_dir(&merged).expect("merged directory");
    fs::write(lower.join("payload"), b"base").expect("lower payload");
    OverlayFsMounter::prepare(&layout_root, &lower, &merged).expect("prepare durable layout");

    let status = Command::new(std::env::current_exe().expect("integration test executable"))
        .arg("--exact")
        .arg("persistent_mount_helper")
        .arg("--nocapture")
        .env("RIFTRI_OVERLAYFS_MOUNT_HELPER", "1")
        .env("RIFTRI_OVERLAYFS_LAYOUT_ROOT", &layout_root)
        .env("RIFTRI_OVERLAYFS_LOWER", &lower)
        .env("RIFTRI_OVERLAYFS_MERGED", &merged)
        .env("RIFTRI_OVERLAYFS_IDENTITY", &identity_path)
        .status()
        .expect("run mount helper process");
    assert!(status.success(), "mount helper process failed: {status}");

    let identity: OverlayFsMountIdentity =
        serde_json::from_slice(&fs::read(&identity_path).expect("read persisted mount identity"))
            .expect("decode persisted mount identity");
    let layout = OverlayFsMounter::load(&layout_root, &lower, &merged)
        .expect("reload durable layout after creator exit");
    assert_eq!(
        OverlayFsMounter::mount_state(&layout, &identity).expect("recover mount state"),
        OverlayFsMountState::Active
    );
    assert_eq!(
        fs::read(merged.join("payload")).expect("read recovered mount"),
        b"base"
    );

    OverlayFsMounter::unmount(&layout, &identity).expect("unmount recovered view");
    OverlayFsMounter::remove_private_layers(&layout, &identity)
        .expect("remove recovered private layers");
}

#[test]
fn recovery_marker_adopts_a_mount_when_identity_was_not_persisted() {
    if !overlayfs_mounts_required() {
        return;
    }
    let fixture = tempdir().expect("fixture directory");
    let lower = fixture.path().join("lower");
    let layout_root = fixture.path().join("layout");
    let merged = fixture.path().join("merged");
    fs::create_dir(&lower).expect("lower directory");
    fs::create_dir(&merged).expect("merged directory");
    fs::write(lower.join("payload"), b"base").expect("lower payload");
    let layout =
        OverlayFsMounter::prepare(&layout_root, &lower, &merged).expect("prepare durable layout");
    let context = OverlayFsMounter::current_mount_context().expect("capture mount context");
    let token = "ab".repeat(32);

    assert_eq!(
        OverlayFsMounter::recover_mount(&layout, &context, &token).expect("inspect unarmed layout"),
        OverlayFsRecoveryState::Absent
    );
    OverlayFsMounter::arm_recovery(&layout, &token).expect("arm recovery");
    assert_eq!(
        OverlayFsMounter::recover_mount(&layout, &context, &token)
            .expect("inspect prepared layout"),
        OverlayFsRecoveryState::Prepared
    );

    let mounted = OverlayFsMounter::mount(&layout).expect("mount recoverable view");
    let recovered =
        OverlayFsMounter::recover_mount(&layout, &context, &token).expect("recover mount identity");
    assert_eq!(recovered, OverlayFsRecoveryState::Mounted(mounted.clone()));
    assert_eq!(mounted.context(), context);

    OverlayFsMounter::clear_recovery(&layout, &token).expect("clear recovery marker");
    assert_eq!(
        OverlayFsMounter::mount_state(&layout, &mounted).expect("inspect adopted mount"),
        OverlayFsMountState::Active
    );
    OverlayFsMounter::unmount(&layout, &mounted).expect("unmount adopted view");
    OverlayFsMounter::remove_private_layers(&layout, &mounted)
        .expect("remove adopted private layers");
}

#[test]
fn recovery_marker_adopts_a_mount_after_creator_exit() {
    if !overlayfs_mounts_required() {
        return;
    }
    let fixture = tempdir().expect("fixture directory");
    let lower = fixture.path().join("lower");
    let layout_root = fixture.path().join("layout");
    let merged = fixture.path().join("merged");
    let context_path = fixture.path().join("mount-context.json");
    let token = "ef".repeat(32);
    fs::create_dir(&lower).expect("lower directory");
    fs::create_dir(&merged).expect("merged directory");
    fs::write(lower.join("payload"), b"base").expect("lower payload");
    OverlayFsMounter::prepare(&layout_root, &lower, &merged).expect("prepare durable layout");

    let status = Command::new(std::env::current_exe().expect("integration test executable"))
        .arg("--exact")
        .arg("recovery_gap_mount_helper")
        .arg("--nocapture")
        .env("RIFTRI_OVERLAYFS_RECOVERY_HELPER", "1")
        .env("RIFTRI_OVERLAYFS_LAYOUT_ROOT", &layout_root)
        .env("RIFTRI_OVERLAYFS_LOWER", &lower)
        .env("RIFTRI_OVERLAYFS_MERGED", &merged)
        .env("RIFTRI_OVERLAYFS_CONTEXT", &context_path)
        .env("RIFTRI_OVERLAYFS_TOKEN", &token)
        .status()
        .expect("run recovery-gap helper process");
    assert!(status.success(), "recovery-gap helper failed: {status}");

    let context: OverlayFsMountContext =
        serde_json::from_slice(&fs::read(&context_path).expect("read persisted mount context"))
            .expect("decode persisted mount context");
    let layout = OverlayFsMounter::load(&layout_root, &lower, &merged)
        .expect("reload layout after helper exit");
    let identity = match OverlayFsMounter::recover_mount(&layout, &context, &token)
        .expect("adopt mount after creator exit")
    {
        OverlayFsRecoveryState::Mounted(identity) => identity,
        state => panic!("expected mounted recovery state, got {state:?}"),
    };
    assert_eq!(
        fs::read(merged.join("payload")).expect("read view"),
        b"base"
    );

    OverlayFsMounter::clear_recovery(&layout, &token).expect("clear recovery marker");
    OverlayFsMounter::clear_recovery(&layout, &token)
        .expect("repeated recovery-marker cleanup is idempotent");
    OverlayFsMounter::unmount(&layout, &identity).expect("unmount recovered view");
    OverlayFsMounter::remove_private_layers(&layout, &identity).expect("remove recovered layers");
}

#[test]
fn recovery_marker_clears_through_the_merged_view_while_mounted() {
    if !overlayfs_mounts_required() {
        return;
    }
    let fixture = tempdir().expect("fixture directory");
    let lower = fixture.path().join("lower");
    let layout_root = fixture.path().join("layout");
    let merged = fixture.path().join("merged");
    fs::create_dir(&lower).expect("lower directory");
    fs::create_dir(&merged).expect("merged directory");
    fs::write(lower.join("payload"), b"base").expect("lower payload");
    let layout =
        OverlayFsMounter::prepare(&layout_root, &lower, &merged).expect("prepare durable layout");
    let token = "aa".repeat(32);
    OverlayFsMounter::arm_recovery(&layout, &token).expect("arm recovery marker");
    let identity = OverlayFsMounter::mount(&layout).expect("mount armed view");

    let marker_name = format!(".riftri-overlayfs-recovery-{token}");
    let merged_lists_marker = || {
        fs::read_dir(&merged)
            .expect("read merged root")
            .flatten()
            .any(|entry| entry.file_name() == OsStr::new(&marker_name))
    };
    assert!(merged_lists_marker(), "armed marker missing in merged view");

    // The upper directory is an underlying layer of a live overlay: the
    // marker must leave through the merged view, so the merged root stops
    // listing it immediately and the upper file is gone as well.
    OverlayFsMounter::clear_recovery(&layout, &token).expect("clear marker while mounted");
    assert!(
        !merged_lists_marker(),
        "cleared marker still listed in the merged root"
    );
    assert!(
        !layout.upper().join(&marker_name).exists(),
        "cleared marker still present in the upper layer"
    );
    OverlayFsMounter::clear_recovery(&layout, &token)
        .expect("repeated mounted cleanup is idempotent");

    assert_eq!(
        OverlayFsMounter::mount_state(&layout, &identity).expect("inspect cleared mount"),
        OverlayFsMountState::Active
    );
    OverlayFsMounter::unmount(&layout, &identity).expect("unmount cleared view");
    OverlayFsMounter::remove_private_layers(&layout, &identity).expect("remove cleared layers");
}

#[test]
fn recovery_marker_is_preserved_under_a_mount_that_does_not_expose_it() {
    if !overlayfs_mounts_required() {
        return;
    }
    let fixture = tempdir().expect("fixture directory");
    let lower = fixture.path().join("lower");
    let armed_root = fixture.path().join("armed-layout");
    let foreign_root = fixture.path().join("foreign-layout");
    let merged = fixture.path().join("merged");
    fs::create_dir(&lower).expect("lower directory");
    fs::create_dir(&merged).expect("merged directory");
    let armed = OverlayFsMounter::prepare(&armed_root, &lower, &merged).expect("prepare armed");
    let token = "bc".repeat(32);
    OverlayFsMounter::arm_recovery(&armed, &token).expect("arm recovery marker");

    // A different overlay now covers the merged destination. Its view does
    // not expose the armed journal's marker, so clearing must fail closed
    // instead of mutating the armed upper layer underneath a live mount.
    let foreign = OverlayFsMounter::prepare(&foreign_root, &lower, &merged).expect("prepare other");
    let foreign_identity = OverlayFsMounter::mount(&foreign).expect("mount other view");
    let error = OverlayFsMounter::clear_recovery(&armed, &token)
        .expect_err("marker under a mount that does not expose it must be preserved");
    assert!(matches!(error, StorageError::OverlayFsMountConflict { .. }));
    let marker = armed_root
        .join("upper")
        .join(format!(".riftri-overlayfs-recovery-{token}"));
    assert!(marker.exists(), "preserved marker was removed");

    OverlayFsMounter::unmount(&foreign, &foreign_identity).expect("unmount other view");
    OverlayFsMounter::remove_private_layers(&foreign, &foreign_identity)
        .expect("remove other layers");
    OverlayFsMounter::clear_recovery(&armed, &token).expect("clear marker once unmounted");
    assert!(!marker.exists());
}

#[test]
fn abandoned_probe_roots_are_reaped_only_when_provably_unmounted() {
    if !overlayfs_mounts_required() {
        return;
    }
    let fixture = tempdir().expect("fixture directory");
    let probe_root = fixture.path().join(".riftri-overlay-probe-1234-5678-0");
    let lower = probe_root.join("lower");
    let layout_root = probe_root.join("layout");
    let merged = probe_root.join("merged");
    fs::create_dir(&probe_root).expect("probe root");
    fs::create_dir(&lower).expect("probe lower");
    fs::create_dir(&merged).expect("probe merged");
    fs::write(lower.join("payload"), b"base").expect("probe payload");
    let layout =
        OverlayFsMounter::prepare(&layout_root, &lower, &merged).expect("prepare probe layout");
    let identity = OverlayFsMounter::mount(&layout).expect("mount probe view");

    // While any mount covers a path inside the root, nothing is touched.
    assert!(
        !OverlayFsMounter::remove_abandoned_probe_root(&probe_root)
            .expect("inspect covered probe root"),
        "a covered probe root must be preserved"
    );
    assert!(probe_root.is_dir(), "preserved probe root was removed");
    assert_eq!(
        fs::read(merged.join("payload")).expect("read live probe view"),
        b"base"
    );

    OverlayFsMounter::unmount(&layout, &identity).expect("unmount probe view");
    assert!(
        OverlayFsMounter::remove_abandoned_probe_root(&probe_root)
            .expect("reap unmounted probe root"),
        "an unmounted probe root must be removable"
    );
    assert!(!probe_root.exists());
    assert!(
        OverlayFsMounter::remove_abandoned_probe_root(&probe_root)
            .expect("repeated reap is idempotent")
    );
}

#[test]
fn probe_reaper_refuses_paths_that_are_not_probe_roots() {
    let fixture = tempdir().expect("fixture directory");
    let unrelated = fixture.path().join("user-directory");
    fs::create_dir(&unrelated).expect("unrelated directory");

    let error = OverlayFsMounter::remove_abandoned_probe_root(&unrelated)
        .expect_err("non-probe paths must be refused");
    assert!(matches!(error, StorageError::InvalidOverlayFsLayout { .. }));
    assert!(unrelated.is_dir());

    assert!(OverlayFsMounter::is_abandoned_probe_name(OsStr::new(
        ".riftri-overlay-probe-1-2-3"
    )));
    assert!(!OverlayFsMounter::is_abandoned_probe_name(OsStr::new(
        "riftri-overlay-probe-1-2-3"
    )));
}

#[test]
fn prepared_layers_require_the_original_mount_namespace_for_cleanup() {
    if !overlayfs_mounts_required() {
        return;
    }
    let fixture = tempdir().expect("fixture directory");
    let lower = fixture.path().join("lower");
    let layout_root = fixture.path().join("layout");
    let merged = fixture.path().join("merged");
    fs::create_dir(&lower).expect("lower directory");
    fs::create_dir(&merged).expect("merged directory");
    let layout =
        OverlayFsMounter::prepare(&layout_root, &lower, &merged).expect("prepare durable layout");
    let context = OverlayFsMounter::current_mount_context().expect("capture mount context");
    let mut foreign_context = context.clone();
    foreign_context.mount_namespace_inode = foreign_context.mount_namespace_inode.saturating_add(1);

    assert_eq!(
        OverlayFsMounter::recover_mount(&layout, &foreign_context, &"cd".repeat(32))
            .expect("detect namespace mismatch"),
        OverlayFsRecoveryState::DifferentNamespace
    );
    let error = OverlayFsMounter::remove_unmounted_private_layers(&layout, &foreign_context)
        .expect_err("foreign namespace must not remove layers");
    assert!(matches!(error, StorageError::OverlayFsMountConflict { .. }));
    assert!(layout_root.exists());

    OverlayFsMounter::remove_unmounted_private_layers(&layout, &context)
        .expect("original namespace removes unmounted layers");
    assert!(!layout_root.exists());
}

#[test]
fn prior_boot_prepared_layers_are_recoverable_when_no_mount_exists() {
    let fixture = tempdir().expect("fixture directory");
    let lower = fixture.path().join("lower");
    let layout_root = fixture.path().join("layout");
    let merged = fixture.path().join("merged");
    fs::create_dir(&lower).expect("lower directory");
    fs::create_dir(&merged).expect("merged directory");
    let layout =
        OverlayFsMounter::prepare(&layout_root, &lower, &merged).expect("prepare durable layout");
    let mut context = OverlayFsMounter::current_mount_context().expect("capture mount context");
    context.boot_id = "00000000-0000-0000-0000-000000000000".to_owned();
    let token = "ba".repeat(32);
    OverlayFsMounter::arm_recovery(&layout, &token).expect("arm recovery marker");

    assert_eq!(
        OverlayFsMounter::recover_mount(&layout, &context, &token)
            .expect("inspect prior-boot prepared layout"),
        OverlayFsRecoveryState::Prepared
    );
    OverlayFsMounter::remove_unmounted_private_layers(&layout, &context)
        .expect("remove prior-boot prepared layers");
    assert!(!layout_root.exists());
}

#[test]
fn remount_loader_resets_only_disposable_work_state() {
    let fixture = tempdir().expect("fixture directory");
    let lower = fixture.path().join("lower");
    let layout_root = fixture.path().join("layout");
    let merged = fixture.path().join("merged");
    fs::create_dir(&lower).expect("lower directory");
    fs::create_dir(&merged).expect("merged directory");
    let layout =
        OverlayFsMounter::prepare(&layout_root, &lower, &merged).expect("prepare durable layout");
    let context = OverlayFsMounter::current_mount_context().expect("capture mount context");
    fs::write(layout.upper().join("private-change"), b"preserve")
        .expect("write private upper fixture");
    fs::create_dir(layout.work().join("kernel-work")).expect("create stale work state");
    fs::write(layout.work().join("kernel-work/temporary"), b"discard")
        .expect("write stale work state");

    let reloaded = OverlayFsMounter::load_for_remount(&layout_root, &lower, &merged, &context)
        .expect("reload layout for remount");
    assert_eq!(
        fs::read(reloaded.upper().join("private-change")).expect("read preserved upper state"),
        b"preserve"
    );
    assert_eq!(
        fs::read_dir(reloaded.work())
            .expect("read reset work directory")
            .count(),
        0
    );
}

#[test]
fn recovery_loader_restores_only_a_missing_work_directory() {
    let fixture = tempdir().expect("fixture directory");
    let lower = fixture.path().join("lower");
    let layout_root = fixture.path().join("layout");
    let merged = fixture.path().join("merged");
    fs::create_dir(&lower).expect("lower directory");
    fs::create_dir(&merged).expect("merged directory");
    let layout =
        OverlayFsMounter::prepare(&layout_root, &lower, &merged).expect("prepare durable layout");
    fs::write(layout.upper().join("private-change"), b"preserve")
        .expect("write private upper fixture");
    fs::create_dir(layout.work().join("kernel-work")).expect("create existing work state");

    // Existing work state is inspection-only: nothing is reset.
    let reloaded = OverlayFsMounter::load_for_recovery(&layout_root, &lower, &merged)
        .expect("reload layout for recovery");
    assert!(reloaded.work().join("kernel-work").is_dir());

    // A crash between a remount reset's removal and recreation leaves the
    // layout without a work directory; recovery restores an empty one.
    fs::remove_dir_all(layout.work()).expect("simulate interrupted work reset");
    let restored = OverlayFsMounter::load_for_recovery(&layout_root, &lower, &merged)
        .expect("restore missing work directory");
    assert_eq!(
        fs::read_dir(restored.work())
            .expect("read restored work directory")
            .count(),
        0
    );
    assert_eq!(
        fs::read(restored.upper().join("private-change")).expect("read preserved upper state"),
        b"preserve"
    );
}

#[test]
fn remount_loader_preserves_work_state_the_journaled_namespace_cannot_rule_out() {
    let fixture = tempdir().expect("fixture directory");
    let lower = fixture.path().join("lower");
    let layout_root = fixture.path().join("layout");
    let merged = fixture.path().join("merged");
    fs::create_dir(&lower).expect("lower directory");
    fs::create_dir(&merged).expect("merged directory");
    let layout =
        OverlayFsMounter::prepare(&layout_root, &lower, &merged).expect("prepare durable layout");
    let context = OverlayFsMounter::current_mount_context().expect("capture mount context");
    fs::create_dir(layout.work().join("kernel-work")).expect("create live-looking work state");
    fs::write(layout.work().join("kernel-work/temporary"), b"preserve")
        .expect("write live-looking work state");

    // Same boot, different namespace: the mount recorded by the journal may
    // still be live where this process cannot see it, so nothing is reset.
    let mut foreign_namespace = context.clone();
    foreign_namespace.mount_namespace_inode =
        foreign_namespace.mount_namespace_inode.saturating_add(1);
    let error =
        OverlayFsMounter::load_for_remount(&layout_root, &lower, &merged, &foreign_namespace)
            .expect_err("a possibly live foreign-namespace mount must not be reset");
    assert!(matches!(error, StorageError::OverlayFsMountConflict { .. }));
    assert_eq!(
        fs::read(layout.work().join("kernel-work/temporary")).expect("read preserved work state"),
        b"preserve"
    );

    // A different boot cannot carry the mount forward, so the disposable work
    // directory is resettable again even from another namespace.
    let mut prior_boot = foreign_namespace;
    prior_boot.boot_id = "00000000-0000-0000-0000-000000000000".to_owned();
    let reloaded = OverlayFsMounter::load_for_remount(&layout_root, &lower, &merged, &prior_boot)
        .expect("reset prior-boot work state");
    assert_eq!(
        fs::read_dir(reloaded.work())
            .expect("read reset work directory")
            .count(),
        0
    );
}

#[test]
fn recovery_token_is_validated_before_a_marker_is_created() {
    let fixture = tempdir().expect("fixture directory");
    let lower = fixture.path().join("lower");
    let layout_root = fixture.path().join("layout");
    let merged = fixture.path().join("merged");
    fs::create_dir(&lower).expect("lower directory");
    fs::create_dir(&merged).expect("merged directory");
    let layout =
        OverlayFsMounter::prepare(&layout_root, &lower, &merged).expect("prepare durable layout");

    let error = OverlayFsMounter::arm_recovery(&layout, "not-a-token")
        .expect_err("invalid token must fail closed");
    assert!(matches!(error, StorageError::InvalidOverlayFsLayout { .. }));
    assert_eq!(
        fs::read_dir(layout.upper())
            .expect("read private upper")
            .count(),
        0
    );
}

#[test]
fn persistent_mount_helper() {
    if std::env::var_os("RIFTRI_OVERLAYFS_MOUNT_HELPER").as_deref() != Some(OsStr::new("1")) {
        return;
    }
    let layout_root = required_helper_path("RIFTRI_OVERLAYFS_LAYOUT_ROOT");
    let lower = required_helper_path("RIFTRI_OVERLAYFS_LOWER");
    let merged = required_helper_path("RIFTRI_OVERLAYFS_MERGED");
    let identity_path = required_helper_path("RIFTRI_OVERLAYFS_IDENTITY");
    let layout = OverlayFsMounter::load(&layout_root, &lower, &merged)
        .expect("mount helper loads durable layout");
    let identity = OverlayFsMounter::mount(&layout).expect("mount helper creates durable view");
    fs::write(
        identity_path,
        serde_json::to_vec(&identity).expect("encode mount identity"),
    )
    .expect("persist mount identity");
}

#[test]
fn recovery_gap_mount_helper() {
    if std::env::var_os("RIFTRI_OVERLAYFS_RECOVERY_HELPER").as_deref() != Some(OsStr::new("1")) {
        return;
    }
    let layout_root = required_helper_path("RIFTRI_OVERLAYFS_LAYOUT_ROOT");
    let lower = required_helper_path("RIFTRI_OVERLAYFS_LOWER");
    let merged = required_helper_path("RIFTRI_OVERLAYFS_MERGED");
    let context_path = required_helper_path("RIFTRI_OVERLAYFS_CONTEXT");
    let token = std::env::var("RIFTRI_OVERLAYFS_TOKEN").expect("missing recovery token");
    let layout = OverlayFsMounter::load(&layout_root, &lower, &merged)
        .expect("recovery helper loads durable layout");
    let context = OverlayFsMounter::current_mount_context().expect("capture helper context");
    fs::write(
        context_path,
        serde_json::to_vec(&context).expect("encode mount context"),
    )
    .expect("persist mount context before mounting");
    OverlayFsMounter::arm_recovery(&layout, &token).expect("arm helper recovery");
    OverlayFsMounter::mount(&layout).expect("mount helper view");
}

#[test]
fn prepare_rejects_a_nonempty_mountpoint_before_mutating_layout_state() {
    let fixture = tempdir().expect("fixture directory");
    let lower = fixture.path().join("lower");
    let layout_root = fixture.path().join("layout");
    let merged = fixture.path().join("merged");
    fs::create_dir(&lower).expect("lower directory");
    fs::create_dir(&merged).expect("merged directory");
    fs::write(merged.join("unexpected"), b"preserve").expect("unexpected destination file");

    let error = OverlayFsMounter::prepare(&layout_root, &lower, &merged)
        .expect_err("nonempty mountpoint must fail closed");
    assert!(matches!(error, StorageError::InvalidOverlayFsLayout { .. }));
    assert!(!layout_root.exists());
    assert_eq!(
        fs::read(merged.join("unexpected")).expect("preserved destination file"),
        b"preserve"
    );
}

fn assert_clean_probe(destination: &Path) {
    let before = fs::read_dir(destination)
        .expect("read destination before probe")
        .count();

    let capability = OverlayFsMounter::probe(destination);

    assert_eq!(capability.kind, BackendKind::OverlayFs);
    assert!(capability.volume.is_some());
    let after = fs::read_dir(destination)
        .expect("read destination after probe")
        .count();
    assert_eq!(before, after, "active probe left a visible artifact");
    if overlayfs_mounts_required() {
        assert_eq!(
            capability.status,
            CapabilityStatus::Supported,
            "required OverlayFS probe failed: {}",
            capability.explanation
        );
    }
}

fn overlayfs_mounts_required() -> bool {
    std::env::var_os("RIFTRI_REQUIRE_OVERLAYFS").as_deref() == Some(OsStr::new("1"))
}

fn required_helper_path(name: &str) -> std::path::PathBuf {
    std::env::var_os(name)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| panic!("missing {name}"))
}
