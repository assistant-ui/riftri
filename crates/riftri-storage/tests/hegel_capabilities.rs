use hegel::TestCase;
use hegel::generators as gs;
use riftri_storage::{
    BackendKind, OverlayFsMountContext, OverlayFsMountIdentity, OverlayFsMountProfile,
};

const KINDS: [BackendKind; 5] = [
    BackendKind::ApfsClone,
    BackendKind::Reflink,
    BackendKind::OverlayFs,
    BackendKind::RefsBlockClone,
    BackendKind::FullCopyFallback,
];

#[hegel::test]
fn backend_kinds_keep_distinct_names_and_round_trip_through_json(tc: TestCase) {
    let first = tc.draw(gs::sampled_from(KINDS.to_vec()));
    let second = tc.draw(gs::sampled_from(KINDS.to_vec()));

    assert_eq!(
        first == second,
        first.display_name() == second.display_name()
    );
    let json = serde_json::to_string(&first).expect("serialize kind");
    assert_eq!(
        json == serde_json::to_string(&second).expect("serialize kind"),
        first == second
    );
    // Journals and `--json` output promise kebab-case identifiers.
    assert!(
        json.trim_matches('"')
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'-'),
        "{json}"
    );
    assert_eq!(
        serde_json::from_str::<BackendKind>(&json).expect("parse kind"),
        first
    );
    // Only OverlayFS measures private layers instead of the whole tree.
    assert_eq!(
        first.allocation_excludes_shared_base(),
        first == BackendKind::OverlayFs
    );
}

#[hegel::test]
fn mount_identities_round_trip_and_old_journals_stay_rootless(tc: TestCase) {
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

    let mut json = serde_json::to_value(&identity).expect("serialize identity");
    assert_eq!(
        serde_json::from_value::<OverlayFsMountIdentity>(json.clone()).expect("parse identity"),
        identity
    );
    let context = identity.context();
    assert_eq!(
        context,
        OverlayFsMountContext {
            profile: identity.profile,
            boot_id: identity.boot_id.clone(),
            mount_namespace_device: identity.mount_namespace_device,
            mount_namespace_inode: identity.mount_namespace_inode,
        }
    );
    let context_json = serde_json::to_string(&context).expect("serialize context");
    assert_eq!(
        serde_json::from_str::<OverlayFsMountContext>(&context_json).expect("parse context"),
        context
    );

    // Journals written before the helper existed carry no profile and must
    // never be read as the privileged one.
    json.as_object_mut()
        .expect("identity object")
        .remove("profile");
    assert_eq!(
        serde_json::from_value::<OverlayFsMountIdentity>(json).expect("parse old identity"),
        OverlayFsMountIdentity {
            profile: OverlayFsMountProfile::RootlessUserXattr,
            ..identity
        }
    );
}

#[cfg(unix)]
#[hegel::test]
fn only_utf8_names_with_the_probe_prefix_are_abandoned_probes(tc: TestCase) {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    use riftri_storage::OverlayFsMounter;

    const PREFIX: &[u8] = b".riftri-overlay-probe-";
    let prefix_length = tc.draw(gs::integers::<usize>().max_value(PREFIX.len()));
    let mut name = PREFIX[..prefix_length].to_vec();
    name.extend(tc.draw(gs::binary().max_size(16)));

    let expected = name.starts_with(PREFIX) && std::str::from_utf8(&name).is_ok();
    assert_eq!(
        OverlayFsMounter::is_abandoned_probe_name(OsStr::from_bytes(&name)),
        expected
    );
}
