use std::env;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::{OverlayFsLayout, OverlayFsMountIdentity, OverlayFsMountProfile, OverlayFsMounter};

pub(crate) fn probe(destination: &Path) -> Result<Option<()>, String> {
    let Some(helper) = configured_helper()? else {
        return Ok(None);
    };
    super::overlayfs::probe_current_namespace_with(
        destination,
        |layout| {
            invoke_mount(&helper, layout).map_err(|detail| crate::StorageError::OverlayFsHelper {
                operation: "mount a capability probe",
                path: layout.merged().to_path_buf(),
                detail,
            })
        },
        |layout, identity| {
            invoke_unmount(&helper, layout, identity).map_err(|detail| {
                crate::StorageError::OverlayFsHelper {
                    operation: "unmount a capability probe",
                    path: layout.merged().to_path_buf(),
                    detail,
                }
            })
        },
    )
    .map_err(|error| error.to_string())?;
    Ok(Some(()))
}

pub(crate) fn mount(layout: &OverlayFsLayout) -> Result<Option<OverlayFsMountIdentity>, String> {
    let Some(helper) = configured_helper()? else {
        return Ok(None);
    };
    invoke_mount(&helper, layout).map(Some)
}

pub(crate) fn unmount(
    layout: &OverlayFsLayout,
    identity: &OverlayFsMountIdentity,
) -> Result<Option<bool>, String> {
    let Some(helper) = configured_helper()? else {
        return Ok(None);
    };
    invoke_unmount(&helper, layout, identity).map(Some)
}

pub(crate) fn reset_work(
    layout_root: &Path,
    lower: &Path,
    merged: &Path,
) -> Result<Option<()>, String> {
    let Some(helper) = configured_helper()? else {
        return Ok(None);
    };
    invoke(
        &helper,
        [
            "reset-work".as_ref(),
            layout_root.as_os_str(),
            lower.as_os_str(),
            merged.as_os_str(),
        ],
    )?;
    Ok(Some(()))
}

fn invoke_mount(helper: &Path, layout: &OverlayFsLayout) -> Result<OverlayFsMountIdentity, String> {
    let output = invoke(
        helper,
        [
            "mount".as_ref(),
            layout.root().as_os_str(),
            layout.lower().as_os_str(),
            layout.merged().as_os_str(),
        ],
    )?;
    let identity: OverlayFsMountIdentity = serde_json::from_slice(&output)
        .map_err(|error| format!("helper returned an invalid mount identity: {error}"))?;
    if identity.profile != OverlayFsMountProfile::PrivilegedTrustedXattr {
        return Err("helper returned an unsafe OverlayFS mount profile".to_owned());
    }
    Ok(identity)
}

fn invoke_unmount(
    helper: &Path,
    layout: &OverlayFsLayout,
    identity: &OverlayFsMountIdentity,
) -> Result<bool, String> {
    let identity = serde_json::to_string(identity)
        .map_err(|error| format!("encode journaled mount identity: {error}"))?;
    let output = invoke(
        helper,
        [
            "unmount".as_ref(),
            layout.root().as_os_str(),
            layout.lower().as_os_str(),
            layout.merged().as_os_str(),
            identity.as_ref(),
        ],
    )?;
    match output.as_slice() {
        b"true\n" => Ok(true),
        b"false\n" => Ok(false),
        _ => Err("helper returned an invalid unmount result".to_owned()),
    }
}

fn configured_helper() -> Result<Option<PathBuf>, String> {
    let path = match env::var_os(OverlayFsMounter::HELPER_ENV) {
        Some(path) if path.is_empty() => return Ok(None),
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(OverlayFsMounter::DEFAULT_HELPER_PATH),
    };
    match fs::symlink_metadata(&path) {
        Ok(_) => validate_helper(&path).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if env::var_os(OverlayFsMounter::HELPER_ENV).is_some() {
                Err(format!("{} does not exist", path.display()))
            } else {
                Ok(None)
            }
        }
        Err(error) => Err(format!("inspect {}: {error}", path.display())),
    }
}

fn validate_helper(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!("helper path must be absolute: {}", path.display()));
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("inspect helper {}: {error}", path.display()))?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(format!("helper is not a regular file: {}", path.display()));
    }
    let mode = metadata.permissions().mode();
    if metadata.uid() != 0 || mode & 0o4000 == 0 || mode & 0o022 != 0 || mode & 0o111 == 0 {
        return Err(format!(
            "helper must be root-owned, set-user-ID, executable, and not group/other-writable: {}",
            path.display()
        ));
    }
    let mut ancestor = path.parent();
    while let Some(directory) = ancestor {
        let metadata = fs::symlink_metadata(directory)
            .map_err(|error| format!("inspect helper parent {}: {error}", directory.display()))?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.uid() != 0
            || metadata.permissions().mode() & 0o022 != 0
        {
            return Err(format!(
                "helper parent must be a root-owned, non-writable real directory: {}",
                directory.display()
            ));
        }
        ancestor = directory.parent();
    }
    Ok(path.to_path_buf())
}

fn invoke<I, S>(helper: &Path, arguments: I) -> Result<Vec<u8>, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let output = Command::new(helper)
        .args(arguments)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| format!("start {}: {error}", helper.display()))?;
    if output.status.success() {
        return Ok(output.stdout);
    }
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    Err(if detail.is_empty() {
        format!("{} exited with {}", helper.display(), output.status)
    } else {
        detail
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    use super::validate_helper;

    #[test]
    fn rejects_user_owned_setuid_lookalike() {
        let fixture = tempfile::tempdir().expect("fixture directory");
        let helper = fixture.path().join("riftri-overlayfs-helper");
        fs::write(&helper, b"not a trusted binary").expect("write fake helper");
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o4755))
            .expect("mark fake helper executable");

        let error = validate_helper(&helper).expect_err("user-owned helper must be rejected");
        assert!(error.contains("root-owned, set-user-ID"), "{error}");
    }

    #[test]
    fn rejects_relative_helper_path_before_execution() {
        let error = validate_helper(Path::new("riftri-overlayfs-helper"))
            .expect_err("relative helper must be rejected");
        assert!(error.contains("must be absolute"), "{error}");
    }
}
