use std::ffi::{CString, OsString};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use thiserror::Error;

use crate::{
    OverlayFsLayout, OverlayFsMountContext, OverlayFsMountIdentity, OverlayFsMountProfile,
    OverlayFsMountState, OverlayFsRecoveryState, StorageError,
};

const PROBE_CONTENTS: &[u8; 4] = b"base";
const PRIVATE_CONTENTS: &[u8; 4] = b"view";
static PROBE_NONCE: AtomicU64 = AtomicU64::new(0);

const ROOTLESS_OVERLAY_OPTIONS: &str = "userxattr,index=off,metacopy=off,redirect_dir=nofollow";
const PRIVILEGED_OVERLAY_OPTIONS: &str = "index=off,metacopy=on";
const RECOVERY_MARKER_PREFIX: &str = ".riftri-overlayfs-recovery-";

#[derive(Debug, Error)]
#[error("{operation}: {source}")]
pub(crate) struct ProbeFailure {
    pub(crate) operation: &'static str,
    #[source]
    pub(crate) source: std::io::Error,
}

impl ProbeFailure {
    fn new(operation: &'static str, source: std::io::Error) -> Self {
        Self { operation, source }
    }

    pub(crate) fn raw_os_error(&self) -> Option<i32> {
        self.source.raw_os_error()
    }
}

pub(crate) fn probe_permission_denied(error: &ProbeFailure) -> bool {
    matches!(error.raw_os_error(), Some(libc::EACCES | libc::EPERM))
}

pub(crate) fn storage_permission_denied(error: &StorageError) -> bool {
    matches!(
        error,
        StorageError::Io { source, .. }
            if matches!(source.raw_os_error(), Some(libc::EACCES | libc::EPERM))
    )
}

pub(crate) fn validate_helper_layout_owner(
    layout: &OverlayFsLayout,
    requester_uid: u32,
) -> Result<(), StorageError> {
    for (name, path) in [
        ("layout root", &layout.root),
        ("immutable lower", &layout.lower),
        ("private upper", &layout.upper),
        ("private work", &layout.work),
        ("merged destination", &layout.merged),
    ] {
        validate_helper_owned_directory(path, requester_uid, name)?;
    }
    Ok(())
}

pub(crate) fn reset_helper_work_directory(
    layout: &OverlayFsLayout,
    requester_uid: u32,
    requester_gid: u32,
) -> Result<(), StorageError> {
    validate_helper_layout_owner(layout, requester_uid)?;
    let entry = current_mount_entry(&layout.merged)?;
    if entry.mount_point == layout.merged {
        return Err(mount_conflict(
            &layout.merged,
            "cannot reset the private work directory while its view is mounted",
        ));
    }
    fs::remove_dir_all(&layout.work).map_err(|source| {
        storage_io(
            "reset helper-owned OverlayFS work directory",
            &layout.work,
            source,
        )
    })?;
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    builder.create(&layout.work).map_err(|source| {
        storage_io(
            "recreate helper-owned OverlayFS work directory",
            &layout.work,
            source,
        )
    })?;
    let directory = File::open(&layout.work).map_err(|source| {
        storage_io(
            "open recreated OverlayFS work directory",
            &layout.work,
            source,
        )
    })?;
    // SAFETY: the descriptor names the newly created journal-owned work
    // directory and the IDs are the real caller credentials captured before
    // any helper operation.
    if unsafe { libc::fchown(directory.as_raw_fd(), requester_uid, requester_gid) } != 0 {
        return Err(storage_io(
            "restore OverlayFS work directory ownership",
            &layout.work,
            std::io::Error::last_os_error(),
        ));
    }
    directory
        .sync_all()
        .map_err(|source| storage_io("sync OverlayFS work directory", &layout.work, source))?;
    sync_directory(&layout.root)
}

fn validate_helper_owned_directory(
    path: &Path,
    requester_uid: u32,
    name: &str,
) -> Result<(), StorageError> {
    let canonical = canonical_real_directory(path, name)?;
    if canonical != path {
        return Err(invalid_layout(
            path,
            format!("{name} resolves through a different path"),
        ));
    }
    let metadata = canonical
        .metadata()
        .map_err(|source| storage_io("inspect helper-owned directory", &canonical, source))?;
    if metadata.uid() != requester_uid {
        return Err(invalid_layout(
            &canonical,
            format!(
                "{name} is owned by uid {}, not requesting uid {requester_uid}",
                metadata.uid()
            ),
        ));
    }
    Ok(())
}

fn validate_helper_owned_descriptor(
    directory: &File,
    requester_uid: u32,
    name: &str,
    error_path: &Path,
) -> Result<(), StorageError> {
    let metadata = directory
        .metadata()
        .map_err(|source| storage_io("inspect helper-owned descriptor", error_path, source))?;
    if metadata.uid() != requester_uid {
        return Err(invalid_layout(
            error_path,
            format!(
                "{name} descriptor is owned by uid {}, not requesting uid {requester_uid}",
                metadata.uid()
            ),
        ));
    }
    Ok(())
}

struct ProbeDirectory {
    path: PathBuf,
}

impl ProbeDirectory {
    fn create(parent: &Path) -> Result<Self, ProbeFailure> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| ProbeFailure::new("read system clock", std::io::Error::other(error)))?
            .as_nanos();
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        for _ in 0..128 {
            let nonce = PROBE_NONCE.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!(
                ".riftri-overlay-probe-{}-{timestamp}-{nonce}",
                std::process::id()
            ));
            match builder.create(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(ProbeFailure::new("create probe directory", error));
                }
            }
        }
        Err(ProbeFailure::new(
            "create probe directory",
            std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "could not allocate a unique probe path",
            ),
        ))
    }

    fn close(mut self) -> Result<(), ProbeFailure> {
        fs::remove_dir_all(&self.path)
            .map_err(|error| ProbeFailure::new("remove probe directory", error))?;
        self.path = PathBuf::new();
        Ok(())
    }

    fn abandon(mut self) {
        self.path = PathBuf::new();
    }
}

impl Drop for ProbeDirectory {
    fn drop(&mut self) {
        if !self.path.as_os_str().is_empty() {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ChildResult {
    stage: i32,
    error: i32,
}

struct ProbeContext<'a> {
    root: &'a CString,
    merged: &'a CString,
    merged_payload: &'a CString,
    upper_payload: &'a CString,
    options: &'a CString,
    lower_payload_fd: RawFd,
}

pub(crate) fn probe(directory: &Path) -> Result<(), ProbeFailure> {
    let root = ProbeDirectory::create(directory)?;
    let lower = root.path.join("lower");
    let upper = root.path.join("upper");
    let work = root.path.join("work");
    let merged = root.path.join("merged");
    for path in [&lower, &upper, &work, &merged] {
        fs::create_dir(path).map_err(|error| ProbeFailure::new("create probe layer", error))?;
    }
    let lower_payload = lower.join("payload");
    let mut payload = File::create(&lower_payload)
        .map_err(|error| ProbeFailure::new("create lower probe file", error))?;
    payload
        .write_all(PROBE_CONTENTS)
        .and_then(|()| payload.sync_all())
        .map_err(|error| ProbeFailure::new("write lower probe file", error))?;
    drop(payload);
    fs::set_permissions(&lower_payload, fs::Permissions::from_mode(0o400))
        .map_err(|error| ProbeFailure::new("protect lower probe file", error))?;

    let lower_payload_file = File::open(&lower_payload)
        .map_err(|error| ProbeFailure::new("open lower probe file", error))?;
    let root_path = path_c_string(&root.path)?;
    let merged_payload = CString::new("merged/payload").expect("controlled path has no NUL");
    let upper_payload = CString::new("upper/payload").expect("controlled path has no NUL");
    let merged = CString::new("merged").expect("controlled path has no NUL");
    let options = CString::new(
        "lowerdir=lower,upperdir=upper,workdir=work,userxattr,index=off,metacopy=off,redirect_dir=nofollow",
    )
    .expect("controlled OverlayFS mount options contain no NUL");

    let result = run_probe_child(&ProbeContext {
        root: &root_path,
        merged: &merged,
        merged_payload: &merged_payload,
        upper_payload: &upper_payload,
        options: &options,
        lower_payload_fd: lower_payload_file.as_raw_fd(),
    });
    drop(lower_payload_file);
    let cleanup = root.close();
    result.and(cleanup)
}

pub(crate) fn probe_current_namespace(directory: &Path) -> Result<(), ProbeFailure> {
    probe_current_namespace_with(directory, mount, unmount)
}

pub(crate) fn probe_current_namespace_with<Mount, Unmount>(
    directory: &Path,
    mount_view: Mount,
    unmount_view: Unmount,
) -> Result<(), ProbeFailure>
where
    Mount: FnOnce(&OverlayFsLayout) -> Result<OverlayFsMountIdentity, StorageError>,
    Unmount: FnOnce(&OverlayFsLayout, &OverlayFsMountIdentity) -> Result<bool, StorageError>,
{
    let root = ProbeDirectory::create(directory)?;
    let lower = root.path.join("lower");
    let layout_root = root.path.join("layout");
    let merged = root.path.join("merged");
    fs::create_dir(&lower).map_err(|error| ProbeFailure::new("create probe lower", error))?;
    fs::create_dir(&merged).map_err(|error| ProbeFailure::new("create probe mountpoint", error))?;
    let lower_payload = lower.join("payload");
    let mut payload = File::create(&lower_payload)
        .map_err(|error| ProbeFailure::new("create lower probe file", error))?;
    payload
        .write_all(PROBE_CONTENTS)
        .and_then(|()| payload.sync_all())
        .map_err(|error| ProbeFailure::new("write lower probe file", error))?;
    drop(payload);
    fs::set_permissions(&lower_payload, fs::Permissions::from_mode(0o400))
        .map_err(|error| ProbeFailure::new("protect lower probe file", error))?;

    let layout = prepare(&layout_root, &lower, &merged)
        .map_err(|error| storage_probe_failure("prepare caller-visible probe", error))?;
    let identity = mount_view(&layout)
        .map_err(|error| storage_probe_failure("mount caller-visible probe", error))?;

    let verification = (|| {
        let contents = fs::read(merged.join("payload"))
            .map_err(|error| ProbeFailure::new("read lower file through probe view", error))?;
        if contents != PROBE_CONTENTS {
            return Err(ProbeFailure::new(
                "read lower file through probe view",
                std::io::Error::other("probe view returned different lower bytes"),
            ));
        }
        if identity.profile == OverlayFsMountProfile::PrivilegedTrustedXattr {
            crate::reflink::make_tree_owner_writable(&merged)
                .map_err(|error| storage_probe_failure("prepare writable probe view", error))?;
        }
        fs::write(merged.join("payload"), PRIVATE_CONTENTS)
            .map_err(|error| ProbeFailure::new("copy up private probe write", error))?;
        if fs::read(&lower_payload)
            .map_err(|error| ProbeFailure::new("verify immutable lower probe file", error))?
            != PROBE_CONTENTS
        {
            return Err(ProbeFailure::new(
                "verify immutable lower probe file",
                std::io::Error::other("copy-up changed the lower file"),
            ));
        }
        if fs::read(layout.upper.join("payload"))
            .map_err(|error| ProbeFailure::new("verify private upper probe file", error))?
            != PRIVATE_CONTENTS
        {
            return Err(ProbeFailure::new(
                "verify private upper probe file",
                std::io::Error::other("private write did not reach the upper layer"),
            ));
        }
        Ok(())
    })();

    if let Err(error) = unmount_view(&layout, &identity) {
        root.abandon();
        return Err(storage_probe_failure("unmount caller-visible probe", error));
    }
    remove_private_layers(&layout, &identity)
        .map_err(|error| storage_probe_failure("remove caller-visible probe layers", error))?;
    let cleanup = root.close();
    verification.and(cleanup)
}

pub(crate) fn prepare(
    layout_root: &Path,
    lower: &Path,
    merged: &Path,
) -> Result<OverlayFsLayout, StorageError> {
    for (name, path) in [
        ("layout root", layout_root),
        ("immutable lower", lower),
        ("merged destination", merged),
    ] {
        if !path.is_absolute() {
            return Err(invalid_layout(
                path,
                format!("{name} path must be absolute"),
            ));
        }
    }
    let lower = canonical_real_directory(lower, "immutable lower")?;
    let merged = canonical_real_directory(merged, "merged destination")?;
    require_empty_directory(&merged, "merged destination")?;

    let parent = layout_root
        .parent()
        .ok_or_else(|| invalid_layout(layout_root, "layout root must have a parent directory"))?;
    fs::create_dir_all(parent)
        .map_err(|source| storage_io("create OverlayFS layout parent", parent, source))?;
    let parent = canonical_real_directory(parent, "layout parent")?;
    let file_name = layout_root
        .file_name()
        .ok_or_else(|| invalid_layout(layout_root, "layout root must name a child directory"))?;
    let normalized_root = parent.join(file_name);
    if normalized_root != layout_root {
        return Err(invalid_layout(
            layout_root,
            format!(
                "layout root resolves through a different path: {}",
                normalized_root.display()
            ),
        ));
    }
    if lower.starts_with(&normalized_root)
        || merged.starts_with(&normalized_root)
        || normalized_root.starts_with(&lower)
        || normalized_root.starts_with(&merged)
    {
        return Err(invalid_layout(
            &normalized_root,
            "layout root, lower, and merged paths must not contain one another",
        ));
    }

    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    builder
        .create(&normalized_root)
        .map_err(|source| storage_io("create OverlayFS layout root", &normalized_root, source))?;
    let cleanup = PreparedLayoutGuard {
        root: normalized_root.clone(),
        armed: true,
    };
    for path in [normalized_root.join("upper"), normalized_root.join("work")] {
        builder
            .create(&path)
            .map_err(|source| storage_io("create OverlayFS private layer", &path, source))?;
    }
    sync_directory(&normalized_root)?;
    sync_directory(&parent)?;

    let layout = load(&normalized_root, &lower, &merged)?;
    cleanup.disarm();
    Ok(layout)
}

pub(crate) fn load(
    layout_root: &Path,
    lower: &Path,
    merged: &Path,
) -> Result<OverlayFsLayout, StorageError> {
    let root = canonical_real_directory(layout_root, "layout root")?;
    if root != layout_root {
        return Err(invalid_layout(
            layout_root,
            format!(
                "layout root resolves through a different path: {}",
                root.display()
            ),
        ));
    }
    let lower = canonical_real_directory(lower, "immutable lower")?;
    let merged = canonical_real_directory(merged, "merged destination")?;
    let upper = canonical_real_directory(&root.join("upper"), "private upper")?;
    let work = canonical_real_directory(&root.join("work"), "private work")?;
    require_exact_layout_entries(&root)?;

    if upper
        .metadata()
        .map_err(|source| storage_io("inspect OverlayFS private upper", &upper, source))?
        .dev()
        != work
            .metadata()
            .map_err(|source| storage_io("inspect OverlayFS private work", &work, source))?
            .dev()
    {
        return Err(invalid_layout(
            &root,
            "private upper and work directories are on different filesystems",
        ));
    }

    Ok(OverlayFsLayout {
        root,
        lower,
        upper,
        work,
        merged,
    })
}

pub(crate) fn load_for_remount(
    layout_root: &Path,
    lower: &Path,
    merged: &Path,
) -> Result<OverlayFsLayout, StorageError> {
    let root = canonical_real_directory(layout_root, "layout root")?;
    if root != layout_root {
        return Err(invalid_layout(
            layout_root,
            format!(
                "layout root resolves through a different path: {}",
                root.display()
            ),
        ));
    }
    let lower = canonical_real_directory(lower, "immutable lower")?;
    let merged = canonical_real_directory(merged, "merged destination")?;
    let upper = canonical_real_directory(&root.join("upper"), "private upper")?;
    let work = root.join("work");
    let entry = current_mount_entry(&merged)?;
    if entry.mount_point == merged {
        return load(&root, &lower, &merged);
    }

    let mut entries = fs::read_dir(&root)
        .map_err(|source| storage_io("read OverlayFS layout root", &root, source))?
        .map(|entry| {
            entry
                .map(|entry| entry.file_name())
                .map_err(|source| storage_io("read OverlayFS layout entry", &root, source))
        })
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort_unstable();
    if entries != [OsString::from("upper")]
        && entries != [OsString::from("upper"), OsString::from("work")]
    {
        return Err(invalid_layout(
            &root,
            "remount layout root must contain only the journal-owned upper and optional work directory",
        ));
    }

    match fs::symlink_metadata(&work) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            if metadata.dev()
                != upper
                    .metadata()
                    .map_err(|source| {
                        storage_io("inspect OverlayFS private upper", &upper, source)
                    })?
                    .dev()
            {
                return Err(invalid_layout(
                    &root,
                    "private upper and work directories are on different filesystems",
                ));
            }
            fs::remove_dir_all(&work).map_err(|source| {
                storage_io("reset OverlayFS remount work directory", &work, source)
            })?;
        }
        Ok(_) => {
            return Err(invalid_layout(
                &work,
                "private work must be a real directory",
            ));
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(storage_io(
                "inspect OverlayFS remount work directory",
                &work,
                source,
            ));
        }
    }
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    builder
        .create(&work)
        .map_err(|source| storage_io("recreate OverlayFS remount work directory", &work, source))?;
    sync_directory(&root)?;
    load(&root, &lower, &merged)
}

pub(crate) fn current_mount_context(
    profile: OverlayFsMountProfile,
) -> Result<OverlayFsMountContext, StorageError> {
    let boot_id = current_boot_id()?;
    let (mount_namespace_device, mount_namespace_inode) = current_mount_namespace()?;
    Ok(OverlayFsMountContext {
        profile,
        boot_id,
        mount_namespace_device,
        mount_namespace_inode,
    })
}

pub(crate) fn mount(layout: &OverlayFsLayout) -> Result<OverlayFsMountIdentity, StorageError> {
    mount_with_profile(layout, OverlayFsMountProfile::RootlessUserXattr)
}

pub(crate) fn mount_with_profile(
    layout: &OverlayFsLayout,
    profile: OverlayFsMountProfile,
) -> Result<OverlayFsMountIdentity, StorageError> {
    mount_for_owner(layout, None, profile)
}

pub(crate) fn mount_for_owner(
    layout: &OverlayFsLayout,
    requester_uid: Option<u32>,
    profile: OverlayFsMountProfile,
) -> Result<OverlayFsMountIdentity, StorageError> {
    if requester_uid.is_some() && profile != OverlayFsMountProfile::PrivilegedTrustedXattr {
        return Err(invalid_layout(
            &layout.merged,
            "the elevated helper requires trusted OverlayFS metadata",
        ));
    }
    validate_layout(layout)?;
    require_empty_directory(&layout.merged, "merged destination")?;
    require_empty_directory(&layout.work, "private work")?;
    let context = current_mount_context(profile)?;
    let merged = open_path_directory(&layout.merged, "open OverlayFS merged destination")?;
    let target_path = PathBuf::from(format!("/proc/self/fd/{}/.", merged.as_raw_fd()));
    let current = current_mount_entry(&target_path)?;
    if current.mount_point == layout.merged {
        return Err(mount_conflict(
            &layout.merged,
            format!(
                "destination is already mount {} ({})",
                current.mount_id, current.filesystem_type
            ),
        ));
    }

    let lower = open_path_directory(&layout.lower, "open OverlayFS immutable lower")?;
    let upper = open_path_directory(&layout.upper, "open OverlayFS private upper")?;
    let work = open_path_directory(&layout.work, "open OverlayFS private work")?;
    if let Some(requester_uid) = requester_uid {
        for (name, directory) in [
            ("immutable lower", &lower),
            ("private upper", &upper),
            ("private work", &work),
            ("merged destination", &merged),
        ] {
            validate_helper_owned_descriptor(directory, requester_uid, name, &layout.merged)?;
        }
    }
    let profile_options = match profile {
        OverlayFsMountProfile::RootlessUserXattr => ROOTLESS_OVERLAY_OPTIONS,
        OverlayFsMountProfile::PrivilegedTrustedXattr => PRIVILEGED_OVERLAY_OPTIONS,
    };
    let options = CString::new(format!(
        "lowerdir=/proc/self/fd/{},upperdir=/proc/self/fd/{},workdir=/proc/self/fd/{},{profile_options}",
        lower.as_raw_fd(),
        upper.as_raw_fd(),
        work.as_raw_fd(),
    ))
    .expect("controlled OverlayFS options contain no NUL");
    let mount_target =
        storage_path_c_string(&target_path, "encode OverlayFS mountpoint descriptor")?;
    let cleanup_target = storage_path_c_string(&layout.merged, "encode OverlayFS mountpoint")?;
    const OVERLAY: &[u8] = b"overlay\0";
    // SAFETY: all strings are NUL-terminated, the directory descriptors stay
    // open for option resolution, and the exact target was validated above.
    if unsafe {
        libc::mount(
            OVERLAY.as_ptr().cast(),
            mount_target.as_ptr(),
            OVERLAY.as_ptr().cast(),
            (libc::MS_NODEV | libc::MS_NOSUID) as libc::c_ulong,
            options.as_ptr().cast(),
        )
    } != 0
    {
        return Err(storage_io(
            "mount durable OverlayFS view",
            &layout.merged,
            std::io::Error::last_os_error(),
        ));
    }
    let cleanup = MountedViewGuard {
        target: cleanup_target,
        _target_directory: merged,
        armed: true,
    };

    let entry = current_mount_entry(&layout.merged)?;
    if entry.mount_point != layout.merged || entry.filesystem_type != "overlay" {
        return Err(mount_conflict(
            &layout.merged,
            "kernel did not expose the newly created OverlayFS mount at the requested path",
        ));
    }
    cleanup.disarm();
    Ok(OverlayFsMountIdentity {
        profile,
        boot_id: context.boot_id,
        mount_namespace_device: context.mount_namespace_device,
        mount_namespace_inode: context.mount_namespace_inode,
        mount_id: entry.mount_id,
    })
}

pub(crate) fn make_view_owner_writable(
    layout: &OverlayFsLayout,
    identity: &OverlayFsMountIdentity,
) -> Result<(), StorageError> {
    if identity.profile != OverlayFsMountProfile::PrivilegedTrustedXattr {
        return Err(invalid_layout(
            &layout.merged,
            "metadata-only permission restoration requires the privileged trusted-xattr profile",
        ));
    }
    if mount_state(layout, identity)? != OverlayFsMountState::Active {
        return Err(mount_conflict(
            &layout.merged,
            "cannot prepare checkout permissions without the exact journaled mount",
        ));
    }
    crate::reflink::make_tree_owner_writable(&layout.merged)?;
    if mount_state(layout, identity)? != OverlayFsMountState::Active {
        return Err(mount_conflict(
            &layout.merged,
            "journaled mount identity changed while preparing checkout permissions",
        ));
    }
    Ok(())
}

pub(crate) fn arm_recovery(layout: &OverlayFsLayout, token: &str) -> Result<(), StorageError> {
    validate_layout(layout)?;
    let marker = recovery_marker_path(layout, token)?;
    let contents = recovery_marker_contents(token);
    let mut options = File::options();
    options
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    match options.open(&marker) {
        Ok(mut file) => {
            file.write_all(&contents)
                .and_then(|()| file.sync_all())
                .map_err(|source| storage_io("write OverlayFS recovery marker", &marker, source))?;
            sync_directory(&layout.upper)
        }
        Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {
            require_recovery_marker(&marker, token)
        }
        Err(source) => Err(storage_io(
            "create OverlayFS recovery marker",
            &marker,
            source,
        )),
    }
}

pub(crate) fn recover_mount(
    layout: &OverlayFsLayout,
    context: &OverlayFsMountContext,
    token: &str,
) -> Result<OverlayFsRecoveryState, StorageError> {
    validate_layout(layout)?;
    let marker = recovery_marker_path(layout, token)?;
    let current_context = current_mount_context(context.profile)?;
    let entry = current_mount_entry(&layout.merged)?;
    if current_context.boot_id != context.boot_id {
        if entry.mount_point == layout.merged {
            return Ok(OverlayFsRecoveryState::Foreign);
        }
        return match require_recovery_marker(&marker, token) {
            Ok(()) => Ok(OverlayFsRecoveryState::Prepared),
            Err(StorageError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                Ok(OverlayFsRecoveryState::Absent)
            }
            Err(error) => Err(error),
        };
    }
    if current_context.mount_namespace_device != context.mount_namespace_device
        || current_context.mount_namespace_inode != context.mount_namespace_inode
    {
        return Ok(OverlayFsRecoveryState::DifferentNamespace);
    }

    if entry.mount_point != layout.merged {
        return match require_recovery_marker(&marker, token) {
            Ok(()) => Ok(OverlayFsRecoveryState::Prepared),
            Err(StorageError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                Ok(OverlayFsRecoveryState::Absent)
            }
            Err(error) => Err(error),
        };
    }
    if entry.filesystem_type != "overlay" {
        return Ok(OverlayFsRecoveryState::Foreign);
    }
    if require_recovery_marker(&marker, token).is_err()
        || require_recovery_marker(&layout.merged.join(recovery_marker_name(token)?), token)
            .is_err()
    {
        return Ok(OverlayFsRecoveryState::Foreign);
    }

    Ok(OverlayFsRecoveryState::Mounted(OverlayFsMountIdentity {
        profile: context.profile,
        boot_id: current_context.boot_id,
        mount_namespace_device: current_context.mount_namespace_device,
        mount_namespace_inode: current_context.mount_namespace_inode,
        mount_id: entry.mount_id,
    }))
}

pub(crate) fn clear_recovery(layout: &OverlayFsLayout, token: &str) -> Result<(), StorageError> {
    validate_layout(layout)?;
    let marker = recovery_marker_path(layout, token)?;
    match require_recovery_marker(&marker, token) {
        Ok(()) => {}
        Err(StorageError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(());
        }
        Err(error) => return Err(error),
    }
    match fs::remove_file(&marker) {
        Ok(()) => {}
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(storage_io(
                "remove OverlayFS recovery marker",
                &marker,
                source,
            ));
        }
    }
    sync_directory(&layout.upper)
}

pub(crate) fn remove_unmounted_private_layers(
    layout: &OverlayFsLayout,
    context: &OverlayFsMountContext,
) -> Result<(), StorageError> {
    validate_layout(layout)?;
    let current_context = current_mount_context(context.profile)?;
    if current_context.boot_id == context.boot_id && current_context != *context {
        return Err(mount_conflict(
            &layout.merged,
            "prepared layers belong to a different mount namespace in the current boot",
        ));
    }
    let entry = current_mount_entry(&layout.merged)?;
    if entry.mount_point == layout.merged {
        return Err(mount_conflict(
            &layout.merged,
            format!(
                "destination is still mount {} ({})",
                entry.mount_id, entry.filesystem_type
            ),
        ));
    }
    require_exact_layout_entries(&layout.root)?;
    fs::remove_dir_all(&layout.root).map_err(|source| {
        storage_io("remove OverlayFS private layer root", &layout.root, source)
    })?;
    if let Some(parent) = layout.root.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

pub(crate) fn mount_state(
    layout: &OverlayFsLayout,
    identity: &OverlayFsMountIdentity,
) -> Result<OverlayFsMountState, StorageError> {
    validate_layout(layout)?;
    mount_state_at(&layout.merged, &layout.merged, identity)
}

fn mount_state_at(
    lookup_path: &Path,
    expected_mountpoint: &Path,
    identity: &OverlayFsMountIdentity,
) -> Result<OverlayFsMountState, StorageError> {
    let current_boot = current_boot_id()?;
    let entry = current_mount_entry(lookup_path)?;
    if current_boot != identity.boot_id {
        return Ok(if entry.mount_point == expected_mountpoint {
            OverlayFsMountState::Foreign
        } else {
            OverlayFsMountState::Absent
        });
    }
    let (namespace_device, namespace_inode) = current_mount_namespace()?;
    if namespace_device != identity.mount_namespace_device
        || namespace_inode != identity.mount_namespace_inode
    {
        return Ok(OverlayFsMountState::DifferentNamespace);
    }
    if entry.mount_point != expected_mountpoint {
        return Ok(OverlayFsMountState::Absent);
    }
    if entry.mount_id == identity.mount_id && entry.filesystem_type == "overlay" {
        Ok(OverlayFsMountState::Active)
    } else {
        Ok(OverlayFsMountState::Foreign)
    }
}

pub(crate) fn unmount(
    layout: &OverlayFsLayout,
    identity: &OverlayFsMountIdentity,
) -> Result<bool, StorageError> {
    unmount_for_owner(layout, identity, None)
}

pub(crate) fn unmount_for_owner(
    layout: &OverlayFsLayout,
    identity: &OverlayFsMountIdentity,
    requester_uid: Option<u32>,
) -> Result<bool, StorageError> {
    if requester_uid.is_none() {
        return unmount_direct(layout, identity);
    }
    validate_layout(layout)?;
    let merged = open_path_directory(&layout.merged, "open OverlayFS merged destination")?;
    if let Some(requester_uid) = requester_uid {
        validate_helper_owned_descriptor(
            &merged,
            requester_uid,
            "merged destination",
            &layout.merged,
        )?;
    }
    let target_path = PathBuf::from(format!("/proc/self/fd/{}/.", merged.as_raw_fd()));
    match mount_state_at(&target_path, &layout.merged, identity)? {
        OverlayFsMountState::Absent => return Ok(false),
        OverlayFsMountState::Active => {}
        OverlayFsMountState::DifferentNamespace => {
            return Err(mount_conflict(
                &layout.merged,
                "journaled mount belongs to a different mount namespace",
            ));
        }
        OverlayFsMountState::Foreign => {
            return Err(mount_conflict(
                &layout.merged,
                "the current mount does not match the journaled boot, namespace, mount ID, and filesystem type",
            ));
        }
    }

    if mount_state(layout, identity)? != OverlayFsMountState::Active {
        return Err(mount_conflict(
            &layout.merged,
            "journaled mount identity changed while preparing to unmount",
        ));
    }
    drop(merged);
    let target = storage_path_c_string(&layout.merged, "encode OverlayFS mountpoint")?;
    // SAFETY: mount_state proved that this exact target is the journaled
    // OverlayFS mount, and UMOUNT_NOFOLLOW rejects a replaced final symlink.
    if unsafe { libc::umount2(target.as_ptr(), libc::UMOUNT_NOFOLLOW) } != 0 {
        return Err(storage_io(
            "unmount durable OverlayFS view",
            &layout.merged,
            std::io::Error::last_os_error(),
        ));
    }
    if mount_state(layout, identity)? != OverlayFsMountState::Absent {
        return Err(mount_conflict(
            &layout.merged,
            "journaled mount still appears active after unmount",
        ));
    }
    Ok(true)
}

fn unmount_direct(
    layout: &OverlayFsLayout,
    identity: &OverlayFsMountIdentity,
) -> Result<bool, StorageError> {
    match mount_state(layout, identity)? {
        OverlayFsMountState::Absent => return Ok(false),
        OverlayFsMountState::Active => {}
        OverlayFsMountState::DifferentNamespace => {
            return Err(mount_conflict(
                &layout.merged,
                "journaled mount belongs to a different mount namespace",
            ));
        }
        OverlayFsMountState::Foreign => {
            return Err(mount_conflict(
                &layout.merged,
                "the current mount does not match the journaled boot, namespace, mount ID, and filesystem type",
            ));
        }
    }

    let target = storage_path_c_string(&layout.merged, "encode OverlayFS mountpoint")?;
    // SAFETY: mount_state proved that this exact target is the journaled
    // OverlayFS mount, and UMOUNT_NOFOLLOW rejects a replaced final symlink.
    if unsafe { libc::umount2(target.as_ptr(), libc::UMOUNT_NOFOLLOW) } != 0 {
        return Err(storage_io(
            "unmount durable OverlayFS view",
            &layout.merged,
            std::io::Error::last_os_error(),
        ));
    }
    if mount_state(layout, identity)? != OverlayFsMountState::Absent {
        return Err(mount_conflict(
            &layout.merged,
            "journaled mount still appears active after unmount",
        ));
    }
    Ok(true)
}

pub(crate) fn remove_private_layers(
    layout: &OverlayFsLayout,
    identity: &OverlayFsMountIdentity,
) -> Result<(), StorageError> {
    validate_layout(layout)?;
    match mount_state(layout, identity)? {
        OverlayFsMountState::Absent => {}
        OverlayFsMountState::Active => {
            return Err(mount_conflict(
                &layout.merged,
                "journaled mount is still active",
            ));
        }
        OverlayFsMountState::DifferentNamespace => {
            return Err(mount_conflict(
                &layout.merged,
                "journaled mount may still be active in a different mount namespace",
            ));
        }
        OverlayFsMountState::Foreign => {
            return Err(mount_conflict(
                &layout.merged,
                "a foreign mount occupies the journaled destination",
            ));
        }
    }
    require_exact_layout_entries(&layout.root)?;
    fs::remove_dir_all(&layout.root).map_err(|source| {
        storage_io("remove OverlayFS private layer root", &layout.root, source)
    })?;
    if let Some(parent) = layout.root.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

struct PreparedLayoutGuard {
    root: PathBuf,
    armed: bool,
}

impl PreparedLayoutGuard {
    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for PreparedLayoutGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

struct MountedViewGuard {
    target: CString,
    _target_directory: File,
    armed: bool,
}

impl MountedViewGuard {
    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for MountedViewGuard {
    fn drop(&mut self) {
        if self.armed {
            // SAFETY: the guard is armed only after this process successfully
            // mounted the exact target and before the identity is returned.
            let _ = unsafe { libc::umount2(self.target.as_ptr(), libc::UMOUNT_NOFOLLOW) };
        }
    }
}

#[derive(Debug)]
struct MountEntry {
    mount_id: u64,
    mount_point: PathBuf,
    filesystem_type: String,
}

fn validate_layout(layout: &OverlayFsLayout) -> Result<(), StorageError> {
    let loaded = load(&layout.root, &layout.lower, &layout.merged)?;
    if loaded != *layout {
        return Err(invalid_layout(
            &layout.root,
            "layout paths no longer resolve to their prepared locations",
        ));
    }
    Ok(())
}

fn recovery_marker_name(token: &str) -> Result<OsString, StorageError> {
    validate_recovery_token(token)?;
    Ok(OsString::from(format!("{RECOVERY_MARKER_PREFIX}{token}")))
}

fn recovery_marker_path(layout: &OverlayFsLayout, token: &str) -> Result<PathBuf, StorageError> {
    Ok(layout.upper.join(recovery_marker_name(token)?))
}

fn recovery_marker_contents(token: &str) -> Vec<u8> {
    format!("riftri-overlayfs-recovery-v1\n{token}\n").into_bytes()
}

fn validate_recovery_token(token: &str) -> Result<(), StorageError> {
    if token.len() == 64 && token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Ok(());
    }
    Err(invalid_layout(
        Path::new("<overlayfs-recovery-token>"),
        "recovery token must be exactly 32 bytes encoded as hexadecimal",
    ))
}

fn require_recovery_marker(path: &Path, token: &str) -> Result<(), StorageError> {
    let expected = recovery_marker_contents(token);
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| storage_io("inspect OverlayFS recovery marker", path, source))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(invalid_layout(
            path,
            "recovery marker must be a regular file",
        ));
    }
    if metadata.len() != expected.len() as u64 {
        return Err(invalid_layout(
            path,
            "recovery marker contains unexpected bytes",
        ));
    }
    let mut file = File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|source| storage_io("open OverlayFS recovery marker", path, source))?;
    let mut contents = Vec::with_capacity(expected.len());
    file.read_to_end(&mut contents)
        .map_err(|source| storage_io("read OverlayFS recovery marker", path, source))?;
    if contents != expected {
        return Err(invalid_layout(
            path,
            "recovery marker contains unexpected bytes",
        ));
    }
    Ok(())
}

fn canonical_real_directory(path: &Path, name: &str) -> Result<PathBuf, StorageError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| storage_io("inspect OverlayFS directory", path, source))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(invalid_layout(
            path,
            format!("{name} must be a real directory"),
        ));
    }
    fs::canonicalize(path).map_err(|source| storage_io("resolve OverlayFS directory", path, source))
}

fn require_empty_directory(path: &Path, name: &str) -> Result<(), StorageError> {
    let mut entries = fs::read_dir(path)
        .map_err(|source| storage_io("read OverlayFS directory", path, source))?;
    if entries.next().is_some() {
        return Err(invalid_layout(path, format!("{name} must be empty")));
    }
    Ok(())
}

fn require_exact_layout_entries(root: &Path) -> Result<(), StorageError> {
    let mut entries = fs::read_dir(root)
        .map_err(|source| storage_io("read OverlayFS layout root", root, source))?
        .map(|entry| {
            entry
                .map(|entry| entry.file_name())
                .map_err(|source| storage_io("read OverlayFS layout entry", root, source))
        })
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort_unstable();
    if entries != [OsString::from("upper"), OsString::from("work")] {
        return Err(invalid_layout(
            root,
            "layout root must contain only the journal-owned upper and work directories",
        ));
    }
    Ok(())
}

fn open_path_directory(path: &Path, operation: &'static str) -> Result<File, StorageError> {
    File::options()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|source| storage_io(operation, path, source))
}

fn current_mount_entry(path: &Path) -> Result<MountEntry, StorageError> {
    let directory =
        File::open(path).map_err(|source| storage_io("open OverlayFS mountpoint", path, source))?;
    let fdinfo_path = PathBuf::from(format!("/proc/self/fdinfo/{}", directory.as_raw_fd()));
    let fdinfo = fs::read(&fdinfo_path)
        .map_err(|source| storage_io("read mountpoint file-descriptor identity", path, source))?;
    let mount_id = fdinfo
        .split(|byte| *byte == b'\n')
        .find_map(|line| line.strip_prefix(b"mnt_id:\t"))
        .ok_or_else(|| invalid_layout(path, "kernel did not report a mount ID for the path"))?;
    let mount_id = std::str::from_utf8(mount_id)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| invalid_layout(path, "kernel reported an invalid mount ID"))?;

    let mountinfo = fs::read("/proc/self/mountinfo").map_err(|source| {
        storage_io(
            "read current mount namespace inventory",
            Path::new("/proc/self/mountinfo"),
            source,
        )
    })?;
    for line in mountinfo.split(|byte| *byte == b'\n') {
        let fields = line
            .split(|byte| byte.is_ascii_whitespace())
            .filter(|field| !field.is_empty())
            .collect::<Vec<_>>();
        if fields.len() < 7 {
            continue;
        }
        let Some(line_mount_id) = std::str::from_utf8(fields[0])
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
        else {
            continue;
        };
        if line_mount_id != mount_id {
            continue;
        }
        let separator = fields
            .iter()
            .position(|field| *field == b"-")
            .ok_or_else(|| invalid_layout(path, "kernel mount inventory entry has no separator"))?;
        if separator + 2 >= fields.len() {
            return Err(invalid_layout(
                path,
                "kernel mount inventory entry is incomplete",
            ));
        }
        let mount_point = PathBuf::from(OsString::from_vec(decode_mount_field(fields[4])?));
        let filesystem_type = std::str::from_utf8(fields[separator + 1])
            .map_err(|_| invalid_layout(path, "kernel reported a non-UTF-8 filesystem type"))?
            .to_owned();
        return Ok(MountEntry {
            mount_id,
            mount_point,
            filesystem_type,
        });
    }
    Err(invalid_layout(
        path,
        format!("mount ID {mount_id} was absent from the current namespace inventory"),
    ))
}

fn decode_mount_field(field: &[u8]) -> Result<Vec<u8>, StorageError> {
    let mut decoded = Vec::with_capacity(field.len());
    let mut index = 0;
    while index < field.len() {
        if field[index] != b'\\' {
            decoded.push(field[index]);
            index += 1;
            continue;
        }
        if index + 3 >= field.len()
            || !(b'0'..=b'7').contains(&field[index + 1])
            || !(b'0'..=b'7').contains(&field[index + 2])
            || !(b'0'..=b'7').contains(&field[index + 3])
        {
            return Err(invalid_layout(
                Path::new("/proc/self/mountinfo"),
                "kernel mount inventory contains an invalid path escape",
            ));
        }
        decoded.push(
            (field[index + 1] - b'0') * 64
                + (field[index + 2] - b'0') * 8
                + (field[index + 3] - b'0'),
        );
        index += 4;
    }
    Ok(decoded)
}

fn current_mount_namespace() -> Result<(u64, u64), StorageError> {
    let path = Path::new("/proc/self/ns/mnt");
    let metadata = fs::metadata(path)
        .map_err(|source| storage_io("inspect current mount namespace", path, source))?;
    Ok((metadata.dev(), metadata.ino()))
}

fn current_boot_id() -> Result<String, StorageError> {
    let path = Path::new("/proc/sys/kernel/random/boot_id");
    let boot_id = fs::read_to_string(path)
        .map_err(|source| storage_io("read Linux boot identity", path, source))?;
    let boot_id = boot_id.trim();
    if boot_id.is_empty()
        || !boot_id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
    {
        return Err(invalid_layout(
            path,
            "kernel reported an invalid boot identity",
        ));
    }
    Ok(boot_id.to_owned())
}

fn sync_directory(path: &Path) -> Result<(), StorageError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| storage_io("sync OverlayFS directory", path, source))
}

fn storage_path_c_string(path: &Path, operation: &'static str) -> Result<CString, StorageError> {
    CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        storage_io(
            operation,
            path,
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "path contains an embedded NUL byte",
            ),
        )
    })
}

fn storage_io(operation: &'static str, path: &Path, source: std::io::Error) -> StorageError {
    StorageError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

fn storage_probe_failure(operation: &'static str, error: StorageError) -> ProbeFailure {
    let source = match error {
        StorageError::Io { source, .. } => source,
        other => std::io::Error::other(other.to_string()),
    };
    ProbeFailure::new(operation, source)
}

fn invalid_layout(path: &Path, detail: impl Into<String>) -> StorageError {
    StorageError::InvalidOverlayFsLayout {
        path: path.to_path_buf(),
        detail: detail.into(),
    }
}

fn mount_conflict(path: &Path, detail: impl Into<String>) -> StorageError {
    StorageError::OverlayFsMountConflict {
        path: path.to_path_buf(),
        detail: detail.into(),
    }
}

fn path_c_string(path: &Path) -> Result<CString, ProbeFailure> {
    CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        ProbeFailure::new(
            "encode probe path",
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "probe path contains an embedded NUL byte",
            ),
        )
    })
}

fn run_probe_child(context: &ProbeContext<'_>) -> Result<(), ProbeFailure> {
    let mut pipe = [-1; 2];
    // SAFETY: `pipe` points to two writable file-descriptor slots.
    if unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(ProbeFailure::new(
            "create probe result pipe",
            std::io::Error::last_os_error(),
        ));
    }
    // Capture the expected parent before `fork`: resolving it in the child
    // would miss a parent exit that happened immediately after the fork.
    // SAFETY: `getpid` has no preconditions.
    let parent = unsafe { libc::getpid() };
    // SAFETY: no synchronized Rust state is accessed in the child. The child
    // uses pre-built buffers and direct libc operations before `_exit`.
    let child = unsafe { libc::fork() };
    if child == -1 {
        // SAFETY: both descriptors were returned by `pipe2` above.
        unsafe {
            libc::close(pipe[0]);
            libc::close(pipe[1]);
        }
        return Err(ProbeFailure::new(
            "fork isolated probe",
            std::io::Error::last_os_error(),
        ));
    }
    if child == 0 {
        // SAFETY: this branch runs in the fork child and never returns into
        // Rust. All referenced buffers were fully built before `fork`.
        unsafe {
            libc::close(pipe[0]);
            child_probe(pipe[1], parent, context);
        }
    }

    // SAFETY: the parent does not write to the child-result pipe.
    unsafe {
        libc::close(pipe[1]);
    }
    // SAFETY: ownership of the open read descriptor transfers to `File`.
    let mut reader = unsafe { File::from_raw_fd(pipe[0]) };
    let mut bytes = [0_u8; std::mem::size_of::<ChildResult>()];
    let read_result = reader.read_exact(&mut bytes);
    drop(reader);
    let wait_result = wait_for_child(child);

    if let Err(error) = read_result {
        let _ = wait_result;
        return Err(ProbeFailure::new("read isolated probe result", error));
    }
    let result = ChildResult {
        stage: i32::from_ne_bytes(bytes[..4].try_into().expect("four-byte stage")),
        error: i32::from_ne_bytes(bytes[4..].try_into().expect("four-byte error")),
    };
    let status =
        wait_result.map_err(|error| ProbeFailure::new("wait for isolated probe", error))?;
    if result.stage == 0 && libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0 {
        return Ok(());
    }

    let operation = match result.stage {
        1 => "isolate probe mount namespace",
        2 => "make probe mount propagation private",
        3 => "enter probe directory",
        4 => "mount probe OverlayFS view",
        5 => "read lower file through probe view",
        6 => "copy up private probe write",
        7 => "sync private probe write",
        8 => "verify immutable lower probe file",
        9 => "verify private upper probe file",
        10 => "unmount probe OverlayFS view",
        _ => "run isolated OverlayFS probe",
    };
    Err(ProbeFailure::new(
        operation,
        if result.error == 0 {
            std::io::Error::other("probe child terminated unexpectedly")
        } else {
            std::io::Error::from_raw_os_error(result.error)
        },
    ))
}

fn wait_for_child(child: libc::pid_t) -> std::io::Result<i32> {
    let mut status = 0;
    loop {
        // SAFETY: `status` is writable and `child` is the direct child PID.
        let waited = unsafe { libc::waitpid(child, &mut status, 0) };
        if waited == child {
            return Ok(status);
        }
        if waited == -1 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
    }
}

unsafe fn child_probe(result_fd: RawFd, parent: libc::pid_t, context: &ProbeContext<'_>) -> ! {
    const OVERLAY: &[u8] = b"overlay\0";
    const ROOT: &[u8] = b"/\0";

    // SAFETY: `prctl` receives the documented scalar arguments for
    // PR_SET_PDEATHSIG. The parent check closes the race before this call.
    if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) } != 0 {
        // SAFETY: `result_fd` is the inherited pipe write descriptor.
        unsafe { child_fail(result_fd, 1, last_errno()) };
    }
    // SAFETY: `getppid` has no preconditions. A changed parent means the
    // expected parent exited before the death signal was installed.
    if unsafe { libc::getppid() } != parent {
        unsafe { child_fail(result_fd, 1, libc::ESRCH) };
    }
    // SAFETY: the child owns no Rust synchronization state and creates only a
    // private mount namespace.
    if unsafe { libc::unshare(libc::CLONE_NEWNS) } != 0 {
        unsafe { child_fail(result_fd, 1, last_errno()) };
    }
    // SAFETY: the NUL-terminated root path is valid; null source, type, and
    // data are required for a recursive propagation change.
    if unsafe {
        libc::mount(
            std::ptr::null(),
            ROOT.as_ptr().cast(),
            std::ptr::null(),
            (libc::MS_REC | libc::MS_PRIVATE) as libc::c_ulong,
            std::ptr::null(),
        )
    } != 0
    {
        unsafe { child_fail(result_fd, 2, last_errno()) };
    }
    // SAFETY: the path was encoded before `fork` and remains valid. Resolving
    // it after `unshare` ensures the working directory belongs to the new
    // mount namespace. Fixed relative layer names then keep caller paths out
    // of the comma- and colon-delimited mount option language.
    if unsafe { libc::chdir(context.root.as_ptr()) } != 0 {
        unsafe { child_fail(result_fd, 3, last_errno()) };
    }
    // SAFETY: all strings are NUL-terminated and live until the child exits.
    if unsafe {
        libc::mount(
            OVERLAY.as_ptr().cast(),
            context.merged.as_ptr(),
            OVERLAY.as_ptr().cast(),
            0,
            context.options.as_ptr().cast(),
        )
    } != 0
    {
        unsafe { child_fail(result_fd, 4, last_errno()) };
    }

    let mut contents = [0_u8; 4];
    // SAFETY: the path is NUL-terminated and the flags require no mode.
    let merged_fd = unsafe {
        libc::open(
            context.merged_payload.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC,
        )
    };
    if merged_fd == -1 {
        unsafe { child_fail(result_fd, 5, last_errno()) };
    }
    let read = unsafe { libc::pread(merged_fd, contents.as_mut_ptr().cast(), contents.len(), 0) };
    if read != contents.len() as isize {
        unsafe {
            child_fail(
                result_fd,
                5,
                if read == -1 { last_errno() } else { libc::EIO },
            )
        };
    }
    if contents != *PROBE_CONTENTS {
        unsafe { child_fail(result_fd, 5, libc::EIO) };
    }
    // SAFETY: `merged_fd` was returned by `open` above.
    unsafe { libc::close(merged_fd) };

    // SAFETY: the path is NUL-terminated and the flags require no mode.
    let merged_fd = unsafe {
        libc::open(
            context.merged_payload.as_ptr(),
            libc::O_WRONLY | libc::O_CLOEXEC,
        )
    };
    if merged_fd == -1 {
        unsafe { child_fail(result_fd, 6, last_errno()) };
    }
    let written = unsafe {
        libc::pwrite(
            merged_fd,
            PRIVATE_CONTENTS.as_ptr().cast(),
            PRIVATE_CONTENTS.len(),
            0,
        )
    };
    if written != PRIVATE_CONTENTS.len() as isize {
        unsafe {
            child_fail(
                result_fd,
                6,
                if written == -1 {
                    last_errno()
                } else {
                    libc::EIO
                },
            )
        };
    }
    // SAFETY: `merged_fd` is an open writable regular file descriptor.
    if unsafe { libc::fsync(merged_fd) } != 0 {
        unsafe { child_fail(result_fd, 7, last_errno()) };
    }
    unsafe { libc::close(merged_fd) };

    contents.fill(0);
    // SAFETY: both file descriptor and output buffer are valid.
    let read = unsafe {
        libc::pread(
            context.lower_payload_fd,
            contents.as_mut_ptr().cast(),
            contents.len(),
            0,
        )
    };
    if read != contents.len() as isize {
        unsafe {
            child_fail(
                result_fd,
                8,
                if read == -1 { last_errno() } else { libc::EIO },
            )
        };
    }
    if contents != *PROBE_CONTENTS {
        unsafe { child_fail(result_fd, 8, libc::EIO) };
    }

    contents.fill(0);
    // SAFETY: the path is NUL-terminated and the flags require no mode.
    let upper_fd = unsafe {
        libc::open(
            context.upper_payload.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC,
        )
    };
    if upper_fd == -1 {
        unsafe { child_fail(result_fd, 9, last_errno()) };
    }
    let read = unsafe { libc::pread(upper_fd, contents.as_mut_ptr().cast(), contents.len(), 0) };
    if read != contents.len() as isize {
        unsafe {
            child_fail(
                result_fd,
                9,
                if read == -1 { last_errno() } else { libc::EIO },
            )
        };
    }
    if contents != *PRIVATE_CONTENTS {
        unsafe { child_fail(result_fd, 9, libc::EIO) };
    }
    unsafe { libc::close(upper_fd) };

    // SAFETY: the target is the exact mount created above.
    if unsafe { libc::umount2(context.merged.as_ptr(), 0) } != 0 {
        unsafe { child_fail(result_fd, 10, last_errno()) };
    }
    unsafe { child_report(result_fd, ChildResult { stage: 0, error: 0 }) };
    unsafe { libc::_exit(0) };
}

unsafe fn child_fail(result_fd: RawFd, stage: i32, error: i32) -> ! {
    unsafe { child_report(result_fd, ChildResult { stage, error }) };
    unsafe { libc::_exit(1) };
}

unsafe fn child_report(result_fd: RawFd, result: ChildResult) {
    let mut bytes = [0_u8; std::mem::size_of::<ChildResult>()];
    bytes[..4].copy_from_slice(&result.stage.to_ne_bytes());
    bytes[4..].copy_from_slice(&result.error.to_ne_bytes());
    // SAFETY: the buffer is initialized and the fixed-size message is below
    // PIPE_BUF, so a successful write is atomic.
    let _ = unsafe { libc::write(result_fd, bytes.as_ptr().cast(), bytes.len()) };
}

fn last_errno() -> i32 {
    // SAFETY: Linux exposes the calling thread's errno through this pointer.
    unsafe { *libc::__errno_location() }
}
