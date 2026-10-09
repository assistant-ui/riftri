use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;
use std::process::Command;

/// Reuse Riftri's real-Git discovery, including inherited activation and
/// stripped shim markers. `command -v git` can select the outer Riftri shim;
/// wrapping that shim as RIFTRI_REAL_GIT recursively invokes the test wrapper.
pub(crate) fn real_git() -> PathBuf {
    let output = Command::new(env!("CARGO_BIN_EXE_riftri"))
        .args(["exec", "--", "sh", "-c", "printf '%s' \"$RIFTRI_REAL_GIT\""])
        .output()
        .expect("locate real Git");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!output.stdout.is_empty(), "real Git path is empty");
    // printf adds no delimiter; neither trim native path bytes nor require UTF-8.
    PathBuf::from(OsString::from_vec(output.stdout))
}
