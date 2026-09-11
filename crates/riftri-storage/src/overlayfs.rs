use std::ffi::CString;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use thiserror::Error;

const PROBE_CONTENTS: &[u8; 4] = b"base";
const PRIVATE_CONTENTS: &[u8; 4] = b"view";
static PROBE_NONCE: AtomicU64 = AtomicU64::new(0);

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
