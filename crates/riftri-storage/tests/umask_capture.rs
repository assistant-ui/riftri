//! The CLI captures the process umask once, as the first statement of `main`,
//! so storage does not spawn a shell to read it on every add and removal.
//!
//! POSIX reads the umask only by setting it, which is safe only while no
//! other thread can create a file. libtest always runs tests beside its own
//! main thread, so this target has no harness: `main` re-executes this binary
//! once per scenario, and each child starts single-threaded exactly as the
//! CLI does.

#[cfg(unix)]
const SCENARIO: &str = "RIFTRI_UMASK_CAPTURE_SCENARIO";

#[cfg(unix)]
fn main() {
    match std::env::var(SCENARIO) {
        Ok(scenario) => scenario::run(&scenario),
        Err(_) => {
            // Masks a user plausibly runs under, plus both extremes of the
            // group and other bits. 0o077 and 0o002 differ in every write bit
            // the storage layer restores.
            let mut scenarios = ["000", "002", "007", "022", "027", "077"]
                .map(|mask| format!("capture:{mask}"))
                .to_vec();
            scenarios.push("threaded".to_owned());
            for scenario in scenarios {
                let output = std::process::Command::new(std::env::current_exe().unwrap())
                    .env(SCENARIO, &scenario)
                    .output()
                    .expect("run umask capture scenario");
                assert!(
                    output.status.success(),
                    "{scenario}:\n{}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                println!("umask capture scenario {scenario} ... ok");
            }
        }
    }
}

#[cfg(not(unix))]
fn main() {
    // There is no process umask to capture.
    assert_eq!(riftri_storage::capture_process_umask(), None);
}

#[cfg(unix)]
mod scenario {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    pub fn run(scenario: &str) {
        if let Some(mask) = scenario.strip_prefix("capture:") {
            capture(u32::from_str_radix(mask, 8).expect("octal mask"));
        } else if scenario == "threaded" {
            threaded();
        } else {
            panic!("unknown scenario {scenario}");
        }
    }

    /// Set the mask this process starts with. Only this child changes it,
    /// before any other thread exists.
    fn set_umask(mask: u32) {
        // SAFETY: umask has no memory-safety preconditions; this process is
        // single-threaded here, so no file is created under a stale mask.
        unsafe {
            libc::umask(mask as libc::mode_t);
        }
    }

    /// The mode a new file gets, which is how Git's checkout sees the mask.
    fn created_file_mode(directory: &Path) -> u32 {
        let probe = directory.join("probe");
        fs::write(&probe, b"").expect("create probe");
        let mode = fs::metadata(&probe)
            .expect("probe metadata")
            .permissions()
            .mode()
            & 0o777;
        fs::remove_file(&probe).expect("remove probe");
        mode
    }

    /// The mask a child shell reports, which is what storage asked before.
    fn shell_umask() -> u32 {
        let output = std::process::Command::new("/bin/sh")
            .args(["-c", "umask"])
            .env_clear()
            .output()
            .expect("ask a shell for the umask");
        assert!(output.status.success());
        u32::from_str_radix(String::from_utf8(output.stdout).unwrap().trim(), 8).unwrap()
    }

    fn capture(mask: u32) {
        set_umask(mask);
        let captured = riftri_storage::capture_process_umask();
        let scratch = tempfile::tempdir().expect("scratch directory");
        // Capturing restores the mask it read.
        assert_eq!(created_file_mode(scratch.path()), 0o666 & !mask);
        assert_eq!(shell_umask(), mask);
        if !cfg!(target_os = "macos") {
            // Elsewhere the mask is read without changing it, from procfs or
            // the shell, so nothing is captured.
            assert_eq!(captured, None);
            return;
        }
        assert_eq!(captured, Some(mask));
        // A second call returns the first capture.
        assert_eq!(riftri_storage::capture_process_umask(), Some(mask));
        #[cfg(target_os = "macos")]
        clone_uses_the_captured_mask(scratch.path(), mask);
    }

    /// Storage restores write bits from the captured mask, not from a shell.
    /// Riftri never changes its own mask; changing it here only proves which
    /// value the clone used, since the shell would now report the new one.
    #[cfg(target_os = "macos")]
    fn clone_uses_the_captured_mask(scratch: &Path, mask: u32) {
        let other = if mask == 0o077 { 0o000 } else { 0o077 };
        let base = scratch.join("base");
        fs::create_dir(&base).expect("create base");
        fs::write(base.join("file"), b"tracked\n").expect("write base file");
        fs::set_permissions(base.join("file"), fs::Permissions::from_mode(0o444)).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o555)).unwrap();
        set_umask(other);
        assert_eq!(shell_umask(), other);
        let view = scratch.join("view");
        let cloned = riftri_storage::ApfsCloner::clone_tree_owner_writable(&base, &view);
        set_umask(mask);
        fs::set_permissions(&base, fs::Permissions::from_mode(0o755)).unwrap();
        cloned.expect("clone base");
        let mode = fs::metadata(view.join("file"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o444 | (0o222 & !mask), "file mode under {mask:03o}");
        let mode = fs::metadata(&view).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode,
            0o555 | 0o700 | (0o222 & !mask),
            "directory mode under {mask:03o}"
        );
    }

    /// With a second thread alive, nothing is captured and the mask is never
    /// touched, so a file that thread creates keeps the real mask.
    fn threaded() {
        let mask = 0o027;
        set_umask(mask);
        let (release, parked) = std::sync::mpsc::channel::<()>();
        let worker = std::thread::spawn(move || parked.recv().ok());
        assert_eq!(riftri_storage::capture_process_umask(), None);
        let scratch = tempfile::tempdir().expect("scratch directory");
        assert_eq!(created_file_mode(scratch.path()), 0o666 & !mask);
        release.send(()).unwrap();
        worker.join().unwrap();
    }
}
