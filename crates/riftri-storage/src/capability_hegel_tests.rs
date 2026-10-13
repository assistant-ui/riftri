use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

use hegel::TestCase;
use hegel::generators as gs;

use super::{
    BackendKind, CapabilityStatus, DestinationVolume, VolumeIdentity, VolumeProbeError,
    fallback_capability, platform_capabilities,
};

fn draw_path(tc: &TestCase) -> PathBuf {
    PathBuf::from(OsString::from_vec(tc.draw(gs::binary().max_size(16))))
}

fn draw_volume(tc: &TestCase) -> Result<DestinationVolume, VolumeProbeError> {
    if tc.draw(gs::booleans()) {
        return Err(VolumeProbeError::new(tc.draw(gs::text())));
    }
    let filesystem = if tc.draw(gs::booleans()) {
        tc.draw(gs::sampled_from(vec![
            "apfs", "APFS", "Apfs", "hfs", "msdos", "exfat", "nfs", "smbfs", "apfs2", "",
        ]))
        .to_owned()
    } else {
        tc.draw(gs::text())
    };
    Ok(DestinationVolume {
        requested_path: draw_path(tc),
        probe_path: draw_path(tc),
        identity: VolumeIdentity {
            device_id: tc.draw(gs::integers::<u64>()),
            filesystem,
        },
        read_only: tc.draw(gs::booleans()),
    })
}

#[hegel::test]
fn capabilities_fail_closed_for_any_inspected_volume(tc: TestCase) {
    let volume = draw_volume(&tc);

    let native = platform_capabilities(&volume);
    let [native] = native.as_slice() else {
        panic!("macOS has exactly one native backend: {native:?}");
    };
    let fallback = fallback_capability(&volume);
    assert_eq!(native.kind, BackendKind::ApfsClone);
    assert_eq!(fallback.kind, BackendKind::FullCopyFallback);
    assert!(!native.requires_explicit_fallback);
    assert!(fallback.requires_explicit_fallback);

    match &volume {
        Ok(volume) => {
            assert_eq!(native.volume.as_ref(), Some(volume));
            assert_eq!(fallback.volume.as_ref(), Some(volume));
            let writable_apfs =
                !volume.read_only && volume.identity.filesystem.eq_ignore_ascii_case("apfs");
            assert_eq!(native.status == CapabilityStatus::Supported, writable_apfs);
            assert_eq!(
                fallback.status == CapabilityStatus::Supported,
                !volume.read_only
            );
            // The volume was inspected, so a refusal is a definite answer.
            for capability in [native, &fallback] {
                assert_ne!(capability.status, CapabilityStatus::Unavailable);
            }
        }
        Err(error) => {
            for capability in [native, &fallback] {
                assert_eq!(capability.status, CapabilityStatus::Unavailable);
                assert_eq!(capability.volume, None);
                assert_eq!(capability.explanation, error.message);
            }
        }
    }
}
