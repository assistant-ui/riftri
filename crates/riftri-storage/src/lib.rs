//! Destination-specific storage capability probing and native COW operations.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors from concrete native storage operations.
#[derive(Debug, Error)]
pub enum StorageError {
    #[error("{backend} is unavailable on this platform")]
    UnsupportedPlatform { backend: &'static str },

    #[error("clone source is not a directory: {0}")]
    InvalidSource(PathBuf),

    #[error("clone destination already exists: {0}")]
    DestinationExists(PathBuf),

    #[error("unsupported filesystem entry in immutable base: {0}")]
    UnsupportedEntry(PathBuf),

    #[error("{operation} {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("clone {source_path} to {destination}: {source}")]
    Clone {
        source_path: PathBuf,
        destination: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Native APFS clone operations used by the explicit macOS prototype.
pub struct ApfsCloner;

impl ApfsCloner {
    /// Clone a directory tree without permitting a byte-copy fallback.
    #[cfg(target_os = "macos")]
    pub fn clone_tree(source: &Path, destination: &Path) -> Result<(), StorageError> {
        apfs::clone_tree(source, destination)
    }

    #[cfg(not(target_os = "macos"))]
    pub fn clone_tree(_source: &Path, _destination: &Path) -> Result<(), StorageError> {
        Err(StorageError::UnsupportedPlatform {
            backend: "APFS cloning",
        })
    }

    /// Remove write permission from an immutable base tree.
    #[cfg(target_os = "macos")]
    pub fn make_tree_read_only(path: &Path) -> Result<(), StorageError> {
        apfs::make_tree_read_only(path)
    }

    #[cfg(not(target_os = "macos"))]
    pub fn make_tree_read_only(_path: &Path) -> Result<(), StorageError> {
        Err(StorageError::UnsupportedPlatform {
            backend: "APFS cloning",
        })
    }

    /// Restore owner write/search permission after cloning a read-only base.
    #[cfg(target_os = "macos")]
    pub fn make_tree_owner_writable(path: &Path) -> Result<(), StorageError> {
        apfs::make_tree_owner_writable(path)
    }

    #[cfg(not(target_os = "macos"))]
    pub fn make_tree_owner_writable(_path: &Path) -> Result<(), StorageError> {
        Err(StorageError::UnsupportedPlatform {
            backend: "APFS cloning",
        })
    }
}

#[cfg(target_os = "macos")]
mod apfs;

/// Linux `FICLONE` operations used by the native reflink backend.
pub struct ReflinkCloner;

impl ReflinkCloner {
    /// Actively verify reflink support using two unnamed files on the target
    /// volume. No probe artifact remains after this call or a process exit.
    #[cfg(target_os = "linux")]
    pub fn probe(destination: &Path) -> BackendCapability {
        let volume = match inspect_destination(destination) {
            Ok(volume) => volume,
            Err(error) => return unavailable(BackendKind::Reflink, &error),
        };
        if volume.read_only {
            return BackendCapability {
                kind: BackendKind::Reflink,
                status: CapabilityStatus::Unsupported,
                volume: Some(volume),
                explanation: "reflinks require a writable destination volume".to_owned(),
                requires_explicit_fallback: false,
            };
        }
        if !matches!(volume.identity.filesystem.as_str(), "btrfs" | "xfs") {
            let filesystem = volume.identity.filesystem.clone();
            return BackendCapability {
                kind: BackendKind::Reflink,
                status: CapabilityStatus::Unsupported,
                volume: Some(volume),
                explanation: format!(
                    "the Linux reflink backend currently supports Btrfs and reflink-enabled XFS, not {filesystem}"
                ),
                requires_explicit_fallback: false,
            };
        }

        match reflink::probe(&volume.probe_path) {
            Ok(()) => BackendCapability {
                kind: BackendKind::Reflink,
                status: CapabilityStatus::Supported,
                explanation: format!(
                    "active FICLONE probe succeeded on {}",
                    volume.identity.filesystem
                ),
                volume: Some(volume),
                requires_explicit_fallback: false,
            },
            Err(error) => {
                let status = match error.raw_os_error() {
                    Some(libc::EOPNOTSUPP | libc::ENOTTY | libc::EINVAL | libc::EXDEV) => {
                        CapabilityStatus::Unsupported
                    }
                    _ => CapabilityStatus::Unavailable,
                };
                BackendCapability {
                    kind: BackendKind::Reflink,
                    status,
                    explanation: format!(
                        "active FICLONE probe failed on {}: {error}",
                        volume.identity.filesystem
                    ),
                    volume: Some(volume),
                    requires_explicit_fallback: false,
                }
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub fn probe(destination: &Path) -> BackendCapability {
        BackendCapability {
            kind: BackendKind::Reflink,
            status: CapabilityStatus::Unsupported,
            volume: None,
            explanation: format!(
                "Linux FICLONE probing is unavailable for {}",
                destination.display()
            ),
            requires_explicit_fallback: false,
        }
    }

    /// Clone a directory tree without permitting a byte-copy fallback.
    #[cfg(target_os = "linux")]
    pub fn clone_tree(source: &Path, destination: &Path) -> Result<(), StorageError> {
        reflink::clone_tree(source, destination)
    }

    #[cfg(not(target_os = "linux"))]
    pub fn clone_tree(_source: &Path, _destination: &Path) -> Result<(), StorageError> {
        Err(StorageError::UnsupportedPlatform {
            backend: "Linux reflinking",
        })
    }

    #[cfg(target_os = "linux")]
    pub fn make_tree_read_only(path: &Path) -> Result<(), StorageError> {
        reflink::make_tree_read_only(path)
    }

    #[cfg(not(target_os = "linux"))]
    pub fn make_tree_read_only(_path: &Path) -> Result<(), StorageError> {
        Err(StorageError::UnsupportedPlatform {
            backend: "Linux reflinking",
        })
    }

    #[cfg(target_os = "linux")]
    pub fn make_tree_owner_writable(path: &Path) -> Result<(), StorageError> {
        reflink::make_tree_owner_writable(path)
    }

    #[cfg(not(target_os = "linux"))]
    pub fn make_tree_owner_writable(_path: &Path) -> Result<(), StorageError> {
        Err(StorageError::UnsupportedPlatform {
            backend: "Linux reflinking",
        })
    }
}

#[cfg(target_os = "linux")]
mod reflink;

/// Linux kernel OverlayFS mount operations.
pub struct OverlayFsMounter;

impl OverlayFsMounter {
    /// Actively verify mount permission, upper/work compatibility, copy-up,
    /// and private-write isolation against the destination volume.
    #[cfg(target_os = "linux")]
    pub fn probe(destination: &Path) -> BackendCapability {
        let volume = match inspect_destination(destination) {
            Ok(volume) => volume,
            Err(error) => return unavailable(BackendKind::OverlayFs, &error),
        };
        if volume.read_only {
            return BackendCapability {
                kind: BackendKind::OverlayFs,
                status: CapabilityStatus::Unsupported,
                volume: Some(volume),
                explanation: "OverlayFS needs writable upper and work directories".to_owned(),
                requires_explicit_fallback: false,
            };
        }

        match overlayfs::probe(&volume.probe_path) {
            Ok(()) => BackendCapability {
                kind: BackendKind::OverlayFs,
                status: CapabilityStatus::Supported,
                explanation: format!(
                    "active OverlayFS mount and private copy-up probe succeeded on {}",
                    volume.identity.filesystem
                ),
                volume: Some(volume),
                requires_explicit_fallback: false,
            },
            Err(error) => {
                let status = match error.raw_os_error() {
                    Some(libc::ENODEV | libc::EOPNOTSUPP | libc::EINVAL | libc::EXDEV) => {
                        CapabilityStatus::Unsupported
                    }
                    _ => CapabilityStatus::Unavailable,
                };
                BackendCapability {
                    kind: BackendKind::OverlayFs,
                    status,
                    explanation: format!(
                        "active OverlayFS probe failed on {} while trying to {}: {}",
                        volume.identity.filesystem, error.operation, error.source
                    ),
                    volume: Some(volume),
                    requires_explicit_fallback: false,
                }
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub fn probe(destination: &Path) -> BackendCapability {
        BackendCapability {
            kind: BackendKind::OverlayFs,
            status: CapabilityStatus::Unsupported,
            volume: None,
            explanation: format!(
                "Linux OverlayFS probing is unavailable for {}",
                destination.display()
            ),
            requires_explicit_fallback: false,
        }
    }
}

#[cfg(target_os = "linux")]
mod overlayfs;

/// Windows ReFS block-clone operations.
pub struct RefsBlockCloner;

impl RefsBlockCloner {
    /// Actively verify ReFS block cloning and private-write isolation on the
    /// destination volume.
    #[cfg(target_os = "windows")]
    pub fn probe(destination: &Path) -> BackendCapability {
        let volume = match inspect_destination(destination) {
            Ok(volume) => volume,
            Err(error) => return unavailable(BackendKind::RefsBlockClone, &error),
        };
        if volume.read_only {
            return BackendCapability {
                kind: BackendKind::RefsBlockClone,
                status: CapabilityStatus::Unsupported,
                volume: Some(volume),
                explanation: "ReFS block cloning requires a writable destination volume".to_owned(),
                requires_explicit_fallback: false,
            };
        }
        if !volume.identity.filesystem.eq_ignore_ascii_case("ReFS") {
            let filesystem = volume.identity.filesystem.clone();
            return BackendCapability {
                kind: BackendKind::RefsBlockClone,
                status: CapabilityStatus::Unsupported,
                volume: Some(volume),
                explanation: format!(
                    "the Windows block-clone backend requires ReFS, not {filesystem}"
                ),
                requires_explicit_fallback: false,
            };
        }

        match refs::probe(&volume.probe_path) {
            Ok(()) => BackendCapability {
                kind: BackendKind::RefsBlockClone,
                status: CapabilityStatus::Supported,
                explanation: "active FSCTL_DUPLICATE_EXTENTS_TO_FILE probe succeeded on ReFS"
                    .to_owned(),
                volume: Some(volume),
                requires_explicit_fallback: false,
            },
            Err(error) => {
                use windows_sys::Win32::Foundation::{
                    ERROR_INVALID_FUNCTION, ERROR_INVALID_PARAMETER, ERROR_NOT_SAME_DEVICE,
                    ERROR_NOT_SUPPORTED,
                };

                let status = match error.raw_os_error().map(|code| code as u32) {
                    Some(
                        ERROR_INVALID_FUNCTION
                        | ERROR_INVALID_PARAMETER
                        | ERROR_NOT_SAME_DEVICE
                        | ERROR_NOT_SUPPORTED,
                    ) => CapabilityStatus::Unsupported,
                    _ => CapabilityStatus::Unavailable,
                };
                BackendCapability {
                    kind: BackendKind::RefsBlockClone,
                    status,
                    explanation: format!(
                        "active FSCTL_DUPLICATE_EXTENTS_TO_FILE probe failed on ReFS: {error}"
                    ),
                    volume: Some(volume),
                    requires_explicit_fallback: false,
                }
            }
        }
    }

    #[cfg(not(target_os = "windows"))]
    pub fn probe(destination: &Path) -> BackendCapability {
        BackendCapability {
            kind: BackendKind::RefsBlockClone,
            status: CapabilityStatus::Unsupported,
            volume: None,
            explanation: format!(
                "Windows ReFS block-clone probing is unavailable for {}",
                destination.display()
            ),
            requires_explicit_fallback: false,
        }
    }

    /// Clone a directory tree with ReFS block cloning. Unaligned file tails
    /// are copied as required by the ReFS cluster-alignment contract; a failed
    /// block-clone request is never replaced with a full-file copy.
    #[cfg(target_os = "windows")]
    pub fn clone_tree(source: &Path, destination: &Path) -> Result<(), StorageError> {
        refs::clone_tree(source, destination)
    }

    #[cfg(not(target_os = "windows"))]
    pub fn clone_tree(_source: &Path, _destination: &Path) -> Result<(), StorageError> {
        Err(StorageError::UnsupportedPlatform {
            backend: "Windows ReFS block cloning",
        })
    }

    #[cfg(target_os = "windows")]
    pub fn make_tree_read_only(path: &Path) -> Result<(), StorageError> {
        refs::make_tree_read_only(path)
    }

    #[cfg(not(target_os = "windows"))]
    pub fn make_tree_read_only(_path: &Path) -> Result<(), StorageError> {
        Err(StorageError::UnsupportedPlatform {
            backend: "Windows ReFS block cloning",
        })
    }

    #[cfg(target_os = "windows")]
    pub fn make_tree_owner_writable(path: &Path) -> Result<(), StorageError> {
        refs::make_tree_owner_writable(path)
    }

    #[cfg(not(target_os = "windows"))]
    pub fn make_tree_owner_writable(_path: &Path) -> Result<(), StorageError> {
        Err(StorageError::UnsupportedPlatform {
            backend: "Windows ReFS block cloning",
        })
    }
}

#[cfg(target_os = "windows")]
mod refs;

/// A storage strategy Riftri may eventually use to materialize a worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackendKind {
    ApfsClone,
    Reflink,
    OverlayFs,
    RefsBlockClone,
    FullCopyFallback,
}

impl BackendKind {
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::ApfsClone => "APFS",
            Self::Reflink => "Linux reflink",
            Self::OverlayFs => "OverlayFS",
            Self::RefsBlockClone => "ReFS block clone",
            Self::FullCopyFallback => "full-copy fallback",
        }
    }
}

/// The result of probing one backend against a concrete destination volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CapabilityStatus {
    /// A conservative inspection or artifact-clean active probe established
    /// that the destination supplies the required primitive.
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

/// Capability half of the storage backend contract.
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
            Ok(metadata) if metadata.is_dir() => {
                return std::fs::canonicalize(candidate).map_err(|error| {
                    VolumeProbeError::new(format!(
                        "resolve destination ancestor {}: {error}",
                        candidate.display()
                    ))
                });
            }
            Ok(_) => {
                return Err(VolumeProbeError::new(format!(
                    "destination ancestor {} is not a directory",
                    candidate.display()
                )));
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
    let mut volume_stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `path` is NUL-terminated and `volume_stats` is valid writable
    // storage. A successful statvfs call initializes the value.
    let result = unsafe { libc::statvfs(path.as_ptr(), volume_stats.as_mut_ptr()) };
    if result != 0 {
        return Err(VolumeProbeError::new(format!(
            "inspect filesystem flags for {}: {}",
            probe_path.display(),
            std::io::Error::last_os_error()
        )));
    }
    // SAFETY: statvfs returned success above.
    let volume_stats = unsafe { volume_stats.assume_init() };
    let read_only = volume_stats.f_flag & libc::ST_RDONLY as libc::c_ulong != 0;

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

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
fn filesystem_name(_stats: &libc::statfs) -> String {
    "unknown-unix-filesystem".to_owned()
}

#[cfg(target_os = "windows")]
fn inspect_destination(destination: &Path) -> Result<DestinationVolume, VolumeProbeError> {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Storage::FileSystem::{GetVolumeInformationW, GetVolumePathNameW};
    use windows_sys::Win32::System::SystemServices::FILE_READ_ONLY_VOLUME;

    const WINDOWS_MAX_PATH: usize = 32_768;

    let probe_path = nearest_existing_ancestor(destination)?;
    let mut wide_path = probe_path.as_os_str().encode_wide().collect::<Vec<_>>();
    wide_path.push(0);
    let mut volume_path = vec![0_u16; WINDOWS_MAX_PATH];

    // SAFETY: the input is NUL-terminated and the output buffer is writable
    // for the length passed to Windows.
    let succeeded = unsafe {
        GetVolumePathNameW(
            wide_path.as_ptr(),
            volume_path.as_mut_ptr(),
            volume_path.len() as u32,
        )
    };
    if succeeded == 0 {
        return Err(VolumeProbeError::new(format!(
            "resolve volume for {}: {}",
            probe_path.display(),
            std::io::Error::last_os_error()
        )));
    }

    let mut serial = 0_u32;
    let mut flags = 0_u32;
    let mut filesystem = vec![0_u16; 256];
    // SAFETY: the root path is a NUL-terminated buffer produced by Windows;
    // optional output pointers are null and all provided outputs are writable.
    let succeeded = unsafe {
        GetVolumeInformationW(
            volume_path.as_ptr(),
            std::ptr::null_mut(),
            0,
            &mut serial,
            std::ptr::null_mut(),
            &mut flags,
            filesystem.as_mut_ptr(),
            filesystem.len() as u32,
        )
    };
    if succeeded == 0 {
        return Err(VolumeProbeError::new(format!(
            "inspect volume for {}: {}",
            probe_path.display(),
            std::io::Error::last_os_error()
        )));
    }

    let filesystem_length = filesystem
        .iter()
        .position(|character| *character == 0)
        .unwrap_or(filesystem.len());
    let filesystem = String::from_utf16_lossy(&filesystem[..filesystem_length]);

    Ok(DestinationVolume {
        requested_path: destination.to_path_buf(),
        probe_path,
        identity: VolumeIdentity {
            device_id: u64::from(serial),
            filesystem,
        },
        read_only: flags & FILE_READ_ONLY_VOLUME != 0,
    })
}

#[cfg(not(any(unix, target_os = "windows")))]
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
        (Ok(volume), _) if volume.read_only => BackendCapability {
            kind: BackendKind::OverlayFs,
            status: CapabilityStatus::Unsupported,
            volume: Some(volume.clone()),
            explanation: "OverlayFS needs writable upper and work directories".to_owned(),
            requires_explicit_fallback: false,
        },
        (Ok(volume), Some(true)) => BackendCapability {
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
        Ok(volume) if volume.read_only => BackendCapability {
            kind: BackendKind::RefsBlockClone,
            status: CapabilityStatus::Unsupported,
            volume: Some(volume.clone()),
            explanation: "ReFS block cloning requires a writable destination volume".to_owned(),
            requires_explicit_fallback: false,
        },
        Ok(volume) if volume.identity.filesystem.eq_ignore_ascii_case("ReFS") => {
            BackendCapability {
                kind: BackendKind::RefsBlockClone,
                status: CapabilityStatus::Unavailable,
                volume: Some(volume.clone()),
                explanation: "ReFS detected; block cloning requires an active private-write probe"
                    .to_owned(),
                requires_explicit_fallback: false,
            }
        }
        Ok(volume) => BackendCapability {
            kind: BackendKind::RefsBlockClone,
            status: CapabilityStatus::Unsupported,
            volume: Some(volume.clone()),
            explanation: format!(
                "the Windows block-clone backend requires ReFS, not {}",
                volume.identity.filesystem
            ),
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
        let directory = tempdir().expect("temporary directory");
        let file = directory.path().join("file");
        std::fs::write(&file, b"not a directory").expect("write fixture file");
        let capabilities = probe_backends(&file.join("child"));

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
}
