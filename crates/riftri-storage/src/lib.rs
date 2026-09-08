//! Read-only storage capability probing and backend contracts.
//!
//! Milestone 1 deliberately stops at capability discovery. Implementations may
//! inspect a destination volume, but no backend in this crate creates or
//! removes files yet.

use std::path::{Path, PathBuf};

use serde::Serialize;

/// A storage strategy Riftri may eventually use to materialize a worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackendKind {
    ApfsClone,
    Reflink,
    OverlayFs,
    RefsBlockClone,
    FullCopyFallback,
}

/// The result of probing one backend against a concrete destination volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CapabilityStatus {
    /// Read-only checks show that the volume supplies the required primitive.
    Supported,
    /// The destination was inspected and does not supply the primitive.
    Unsupported,
    /// The check could not establish a reliable answer.
    Unavailable,
}

/// Identity of the volume that would contain the requested destination.
///
/// `device_id` is the platform device identifier. The filesystem name is part
/// of the identity so a reused device number cannot alias a differently
/// formatted volume during one Riftri state lifetime.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct VolumeIdentity {
    pub device_id: u64,
    pub filesystem: String,
}

/// Facts discovered about the destination's nearest existing ancestor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DestinationVolume {
    pub requested_path: PathBuf,
    pub probe_path: PathBuf,
    pub identity: VolumeIdentity,
    pub read_only: bool,
}

/// A destination-specific backend result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BackendCapability {
    pub kind: BackendKind,
    pub status: CapabilityStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volume: Option<DestinationVolume>,
    pub explanation: String,
    /// Full copies are never selected unless the caller explicitly opts in.
    pub requires_explicit_fallback: bool,
}

/// Read-only half of the storage backend contract.
///
/// Mutation methods will be added only with a concrete Milestone 2 backend and
/// its rollback tests. This prevents the foundation from exposing an unsafe,
/// partially specified creation API.
pub trait StorageBackend {
    fn kind(&self) -> BackendKind;
    fn probe(&self, destination: &Path) -> BackendCapability;
}

/// Probe every backend relevant to this build against `destination`.
///
/// A destination does not need to exist yet. Its nearest existing ancestor is
/// used, which is the volume on which a new child would normally be created.
pub fn probe_backends(destination: &Path) -> Vec<BackendCapability> {
    let volume = inspect_destination(destination);
    let mut capabilities = platform_capabilities(&volume);
    capabilities.push(fallback_capability(&volume));
    capabilities
}

fn fallback_capability(volume: &Result<DestinationVolume, VolumeProbeError>) -> BackendCapability {
    match volume {
        Ok(volume) if volume.read_only => BackendCapability {
            kind: BackendKind::FullCopyFallback,
            status: CapabilityStatus::Unsupported,
            volume: Some(volume.clone()),
            explanation: "destination volume is read-only".to_owned(),
            requires_explicit_fallback: true,
        },
        Ok(volume) => BackendCapability {
            kind: BackendKind::FullCopyFallback,
            status: CapabilityStatus::Supported,
            volume: Some(volume.clone()),
            explanation: "ordinary Git worktree; available only when explicitly allowed".to_owned(),
            requires_explicit_fallback: true,
        },
        Err(error) => unavailable(BackendKind::FullCopyFallback, error),
    }
}

fn unavailable(kind: BackendKind, error: &VolumeProbeError) -> BackendCapability {
    BackendCapability {
        kind,
        status: CapabilityStatus::Unavailable,
        volume: None,
        explanation: error.message.clone(),
        requires_explicit_fallback: kind == BackendKind::FullCopyFallback,
    }
}

#[derive(Debug)]
struct VolumeProbeError {
    message: String,
}

impl VolumeProbeError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[cfg(unix)]
fn nearest_existing_ancestor(destination: &Path) -> Result<PathBuf, VolumeProbeError> {
    let absolute = if destination.is_absolute() {
        destination.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| VolumeProbeError::new(format!("resolve current directory: {error}")))?
            .join(destination)
    };

    for candidate in absolute.ancestors() {
        match std::fs::metadata(candidate) {
            Ok(_) => {
                return std::fs::canonicalize(candidate).map_err(|error| {
                    VolumeProbeError::new(format!(
                        "resolve destination ancestor {}: {error}",
                        candidate.display()
                    ))
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(VolumeProbeError::new(format!(
                    "inspect destination ancestor {}: {error}",
                    candidate.display()
                )));
            }
        }
    }

    Err(VolumeProbeError::new(format!(
        "no existing ancestor for destination {}",
        destination.display()
    )))
}

#[cfg(unix)]
fn inspect_destination(destination: &Path) -> Result<DestinationVolume, VolumeProbeError> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;

    let probe_path = nearest_existing_ancestor(destination)?;
    let metadata = std::fs::metadata(&probe_path).map_err(|error| {
        VolumeProbeError::new(format!(
            "inspect destination volume at {}: {error}",
            probe_path.display()
        ))
    })?;
    let path = std::ffi::CString::new(probe_path.as_os_str().as_bytes())
        .map_err(|_| VolumeProbeError::new("destination path contains an embedded NUL byte"))?;
    let mut stats = std::mem::MaybeUninit::<libc::statfs>::uninit();

    // SAFETY: `path` is NUL-terminated and `stats` points to writable,
    // correctly aligned storage. A successful statfs call initializes it.
    let result = unsafe { libc::statfs(path.as_ptr(), stats.as_mut_ptr()) };
    if result != 0 {
        return Err(VolumeProbeError::new(format!(
            "inspect filesystem for {}: {}",
            probe_path.display(),
            std::io::Error::last_os_error()
        )));
    }

    // SAFETY: statfs returned success above.
    let stats = unsafe { stats.assume_init() };
    let filesystem = filesystem_name(&stats);
    let read_only = is_read_only(&stats);

    Ok(DestinationVolume {
        requested_path: destination.to_path_buf(),
        probe_path,
        identity: VolumeIdentity {
            device_id: metadata.dev(),
            filesystem,
        },
        read_only,
    })
}

#[cfg(target_os = "macos")]
fn filesystem_name(stats: &libc::statfs) -> String {
    let bytes = stats
        .f_fstypename
        .iter()
        .copied()
        .take_while(|byte| *byte != 0)
        .map(|byte| byte as u8)
        .collect::<Vec<_>>();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(target_os = "macos")]
fn is_read_only(stats: &libc::statfs) -> bool {
    stats.f_flags & libc::MNT_RDONLY as u32 != 0
}

#[cfg(target_os = "linux")]
fn filesystem_name(stats: &libc::statfs) -> String {
    const BTRFS_SUPER_MAGIC: libc::c_long = 0x9123_683e;
    const XFS_SUPER_MAGIC: libc::c_long = 0x5846_5342;
    const OVERLAYFS_SUPER_MAGIC: libc::c_long = 0x794c_7630;

    match stats.f_type {
        BTRFS_SUPER_MAGIC => "btrfs".to_owned(),
        XFS_SUPER_MAGIC => "xfs".to_owned(),
        OVERLAYFS_SUPER_MAGIC => "overlayfs".to_owned(),
        other => format!("linux-magic-0x{other:x}"),
    }
}

#[cfg(target_os = "linux")]
fn is_read_only(stats: &libc::statfs) -> bool {
    stats.f_flags & libc::ST_RDONLY != 0
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
fn filesystem_name(_stats: &libc::statfs) -> String {
    "unknown-unix-filesystem".to_owned()
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
fn is_read_only(stats: &libc::statfs) -> bool {
    stats.f_flags & libc::ST_RDONLY != 0
}

#[cfg(not(unix))]
fn inspect_destination(destination: &Path) -> Result<DestinationVolume, VolumeProbeError> {
    Err(VolumeProbeError::new(format!(
        "volume identity probing is not implemented on {} for {}",
        std::env::consts::OS,
        destination.display()
    )))
}

#[cfg(target_os = "macos")]
fn platform_capabilities(
    volume: &Result<DestinationVolume, VolumeProbeError>,
) -> Vec<BackendCapability> {
    vec![match volume {
        Ok(volume) if volume.read_only => BackendCapability {
            kind: BackendKind::ApfsClone,
            status: CapabilityStatus::Unsupported,
            volume: Some(volume.clone()),
            explanation: "APFS clone creation requires a writable destination volume".to_owned(),
            requires_explicit_fallback: false,
        },
        Ok(volume) if volume.identity.filesystem.eq_ignore_ascii_case("apfs") => {
            BackendCapability {
                kind: BackendKind::ApfsClone,
                status: CapabilityStatus::Supported,
                volume: Some(volume.clone()),
                explanation: "destination is on a writable APFS volume with native clone support"
                    .to_owned(),
                requires_explicit_fallback: false,
            }
        }
        Ok(volume) => BackendCapability {
            kind: BackendKind::ApfsClone,
            status: CapabilityStatus::Unsupported,
            volume: Some(volume.clone()),
            explanation: format!(
                "destination filesystem is {}, not APFS",
                volume.identity.filesystem
            ),
            requires_explicit_fallback: false,
        },
        Err(error) => unavailable(BackendKind::ApfsClone, error),
    }]
}

#[cfg(target_os = "linux")]
fn platform_capabilities(
    volume: &Result<DestinationVolume, VolumeProbeError>,
) -> Vec<BackendCapability> {
    let reflink = match volume {
        Ok(volume) if volume.read_only => BackendCapability {
            kind: BackendKind::Reflink,
            status: CapabilityStatus::Unsupported,
            volume: Some(volume.clone()),
            explanation: "reflinks require a writable destination volume".to_owned(),
            requires_explicit_fallback: false,
        },
        Ok(volume) if volume.identity.filesystem == "btrfs" => BackendCapability {
            kind: BackendKind::Reflink,
            status: CapabilityStatus::Supported,
            volume: Some(volume.clone()),
            explanation: "destination is on Btrfs, which provides native reflinks".to_owned(),
            requires_explicit_fallback: false,
        },
        Ok(volume) if volume.identity.filesystem == "xfs" => BackendCapability {
            kind: BackendKind::Reflink,
            status: CapabilityStatus::Unavailable,
            volume: Some(volume.clone()),
            explanation:
                "XFS detected; its reflink feature cannot be confirmed without an active probe"
                    .to_owned(),
            requires_explicit_fallback: false,
        },
        Ok(volume) => BackendCapability {
            kind: BackendKind::Reflink,
            status: CapabilityStatus::Unsupported,
            volume: Some(volume.clone()),
            explanation: format!(
                "no read-only reflink proof is available for {}",
                volume.identity.filesystem
            ),
            requires_explicit_fallback: false,
        },
        Err(error) => unavailable(BackendKind::Reflink, error),
    };

    let overlay_available = std::fs::read_to_string("/proc/filesystems")
        .map(|filesystems| {
            filesystems
                .lines()
                .any(|line| line.split_whitespace().last() == Some("overlay"))
        })
        .ok();
    let overlay = match (volume, overlay_available) {
        (Ok(volume), Some(true)) if !volume.read_only => BackendCapability {
            kind: BackendKind::OverlayFs,
            status: CapabilityStatus::Unavailable,
            volume: Some(volume.clone()),
            explanation: "kernel OverlayFS is present; mount permission and upper-volume compatibility require an active probe"
                .to_owned(),
            requires_explicit_fallback: false,
        },
        (Ok(volume), Some(false)) => BackendCapability {
            kind: BackendKind::OverlayFs,
            status: CapabilityStatus::Unsupported,
            volume: Some(volume.clone()),
            explanation: "the running kernel does not advertise OverlayFS".to_owned(),
            requires_explicit_fallback: false,
        },
        (Ok(volume), _) if volume.read_only => BackendCapability {
            kind: BackendKind::OverlayFs,
            status: CapabilityStatus::Unsupported,
            volume: Some(volume.clone()),
            explanation: "OverlayFS needs writable upper and work directories".to_owned(),
            requires_explicit_fallback: false,
        },
        (Ok(volume), None) => BackendCapability {
            kind: BackendKind::OverlayFs,
            status: CapabilityStatus::Unavailable,
            volume: Some(volume.clone()),
            explanation: "could not inspect /proc/filesystems".to_owned(),
            requires_explicit_fallback: false,
        },
        (Err(error), _) => unavailable(BackendKind::OverlayFs, error),
    };

    vec![reflink, overlay]
}

#[cfg(target_os = "windows")]
fn platform_capabilities(
    volume: &Result<DestinationVolume, VolumeProbeError>,
) -> Vec<BackendCapability> {
    vec![match volume {
        Ok(volume) => BackendCapability {
            kind: BackendKind::RefsBlockClone,
            status: CapabilityStatus::Unavailable,
            volume: Some(volume.clone()),
            explanation: "ReFS block-clone probing is not implemented yet".to_owned(),
            requires_explicit_fallback: false,
        },
        Err(error) => unavailable(BackendKind::RefsBlockClone, error),
    }]
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn platform_capabilities(
    _volume: &Result<DestinationVolume, VolumeProbeError>,
) -> Vec<BackendCapability> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{BackendKind, CapabilityStatus, probe_backends};

    #[test]
    fn probes_the_volume_of_a_missing_destination_via_its_parent() {
        let directory = tempdir().expect("temporary directory");
        let destination = directory.path().join("missing").join("worktree");

        let capabilities = probe_backends(&destination);

        assert!(!capabilities.is_empty());
        assert!(capabilities.iter().all(|capability| {
            capability
                .volume
                .as_ref()
                .is_some_and(|volume| volume.requested_path == destination)
        }));
    }

    #[test]
    fn reports_an_explicit_full_copy_fallback() {
        let directory = tempdir().expect("temporary directory");

        let capabilities = probe_backends(directory.path());
        let fallback = capabilities
            .iter()
            .find(|capability| capability.kind == BackendKind::FullCopyFallback)
            .expect("fallback result");

        assert!(fallback.requires_explicit_fallback);
        assert_ne!(fallback.status, CapabilityStatus::Unavailable);
    }

    #[test]
    fn an_uninspectable_destination_is_unavailable() {
        let capabilities = probe_backends(Path::new("/dev/null/child"));

        assert!(
            capabilities
                .iter()
                .all(|capability| capability.status == CapabilityStatus::Unavailable)
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn apfs_probe_distinguishes_supported_and_unsupported_volumes() {
        use std::path::PathBuf;

        use super::{DestinationVolume, VolumeIdentity, platform_capabilities};

        let volume = |filesystem: &str| DestinationVolume {
            requested_path: PathBuf::from("/destination"),
            probe_path: PathBuf::from("/"),
            identity: VolumeIdentity {
                device_id: 1,
                filesystem: filesystem.to_owned(),
            },
            read_only: false,
        };

        let supported = platform_capabilities(&Ok(volume("apfs")));
        let unsupported = platform_capabilities(&Ok(volume("hfs")));

        assert_eq!(supported[0].status, CapabilityStatus::Supported);
        assert_eq!(unsupported[0].status, CapabilityStatus::Unsupported);
    }

    use std::path::Path;
}
