#![cfg(any(
    target_os = "macos",
    all(
        feature = "native-cow-integration",
        any(target_os = "linux", target_os = "windows")
    )
))]

use std::env;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use riftri_core::{
    AddWorktreeRequest, RemoveWorktreeRequest, WorktreeMode, add_worktree, remove_worktree,
    storage_accounting,
};
use riftri_storage::BackendKind;
use serde::Serialize;

mod support;
use support::writable_tempdir as tempdir;

const LOGICAL_PAYLOAD_BYTES: usize = 32 * 1024 * 1024;
const PRIVATE_WRITE_BYTES: usize = 4 * 1024 * 1024;
/// A monorepo shape: several sibling packages, only one of which a task needs.
const MONOREPO_PACKAGES: usize = 8;
const MONOREPO_FILES_PER_PACKAGE: usize = 8;
const MONOREPO_FILE_BYTES: usize = 512 * 1024;

#[derive(Serialize)]
struct SparseMonorepoBenchmark {
    schema_version: u8,
    operating_system: &'static str,
    architecture: &'static str,
    backend: BackendKind,
    packages: usize,
    logical_tree_bytes: u64,
    cone_packages: usize,
    logical_cone_bytes: u64,
    full_cold_add_microseconds: u64,
    full_cold_volume_growth_bytes: u64,
    sparse_cold_add_microseconds: u64,
    sparse_cold_volume_growth_bytes: u64,
    sparse_cached_add_microseconds: u64,
    sparse_cached_volume_growth_bytes: u64,
    full_view_allocated_bytes: Option<u64>,
    sparse_view_allocated_bytes: Option<u64>,
}

#[derive(Serialize)]
struct NativeCowBenchmark {
    schema_version: u8,
    operating_system: &'static str,
    architecture: &'static str,
    backend: BackendKind,
    logical_payload_bytes: u64,
    cold_add_microseconds: u64,
    cached_add_microseconds: u64,
    cold_volume_growth_bytes: u64,
    cached_volume_growth_bytes: u64,
    private_write_bytes: u64,
    private_write_volume_growth_bytes: u64,
    cached_view_allocated_before_write_bytes: Option<u64>,
    cached_view_allocated_after_write_bytes: Option<u64>,
}

fn git(path: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(path)
        .output()
        .expect("run Git fixture command");
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn write_payload(path: &Path) {
    let mut file = File::create(path).expect("create benchmark payload");
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut block = [0_u8; 64 * 1024];
    for _ in 0..(LOGICAL_PAYLOAD_BYTES / block.len()) {
        for chunk in block.as_chunks_mut::<8>().0 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *chunk = state.to_le_bytes();
        }
        file.write_all(&block).expect("write benchmark payload");
    }
    file.sync_all().expect("sync benchmark payload");
}

/// Write `bytes` of incompressible pseudo-random data, so a filesystem that
/// compresses or deduplicates cannot flatter the measurement.
fn write_sized_payload(path: &Path, bytes: usize) {
    let mut file = File::create(path).expect("create benchmark payload");
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut block = [0_u8; 64 * 1024];
    let mut written = 0;
    while written < bytes {
        for chunk in block.as_chunks_mut::<8>().0 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *chunk = state.to_le_bytes();
        }
        let take = block.len().min(bytes - written);
        file.write_all(&block[..take])
            .expect("write benchmark payload");
        written += take;
    }
    file.sync_all().expect("sync benchmark payload");
}

fn elapsed_microseconds(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

fn view_allocated_bytes(state: &Path, destination: &Path, backend: BackendKind) -> Option<u64> {
    if backend == BackendKind::OverlayFs {
        return None;
    }
    let accounting = storage_accounting(state).expect("account native COW view");
    Some(
        accounting
            .views
            .iter()
            .find(|view| view.destination == destination)
            .expect("benchmark view accounting")
            .allocated_bytes,
    )
}

#[test]
#[ignore = "repeatable native COW timing and allocation benchmark; run on a supported quiet volume"]
fn reports_cold_cached_and_private_write_costs() {
    let fixture = tempdir().expect("benchmark fixture directory");
    let repository = fixture.path().join("repository");
    let state = fixture.path().join("state");
    let cold_view = fixture.path().join("cold-view");
    let cached_view = fixture.path().join("cached-view");
    fs::create_dir(&repository).expect("create benchmark repository");
    git(&repository, &["init", "--quiet"]);
    git(&repository, &["config", "user.name", "Riftri Benchmark"]);
    git(
        &repository,
        &["config", "user.email", "riftri@example.invalid"],
    );
    git(&repository, &["config", "core.autocrlf", "false"]);
    write_payload(&repository.join("payload.bin"));
    git(&repository, &["add", "--", "payload.bin"]);
    git(
        &repository,
        &["commit", "--quiet", "-m", "benchmark payload"],
    );

    let before_cold = fs2::available_space(fixture.path()).expect("space before cold add");
    let cold_started = Instant::now();
    let cold_result = add_worktree(AddWorktreeRequest {
        repository: repository.clone(),
        destination: cold_view.clone(),
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::NewBranch(OsString::from("benchmark/cold")),
        state_dir: Some(state.clone()),
        sparse_directories: Vec::new(),
    })
    .expect("create cold Riftri worktree");
    let cold_duration = cold_started.elapsed();
    let after_cold = fs2::available_space(fixture.path()).expect("space after cold add");

    let cached_started = Instant::now();
    let cached_result = add_worktree(AddWorktreeRequest {
        repository: repository.clone(),
        destination: cached_view.clone(),
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::NewBranch(OsString::from("benchmark/cached")),
        state_dir: Some(state.clone()),
        sparse_directories: Vec::new(),
    })
    .expect("create cached Riftri worktree");
    let cached_duration = cached_started.elapsed();
    let after_cached = fs2::available_space(fixture.path()).expect("space after cached add");

    assert!(!cold_result.reused_base);
    assert!(cached_result.reused_base);
    assert_eq!(cold_result.backend, cached_result.backend);
    assert!(git(&cold_view, &["status", "--porcelain=v1"]).is_empty());
    assert!(git(&cached_view, &["status", "--porcelain=v1"]).is_empty());

    let cached_allocated_before =
        view_allocated_bytes(&state, &cached_result.destination, cached_result.backend);

    let before_private_write =
        fs2::available_space(fixture.path()).expect("space before private write");
    let mut cached_payload = OpenOptions::new()
        .write(true)
        .open(cached_view.join("payload.bin"))
        .expect("open cached payload");
    cached_payload
        .seek(SeekFrom::Start(
            (LOGICAL_PAYLOAD_BYTES - PRIVATE_WRITE_BYTES) as u64,
        ))
        .expect("seek cached payload");
    cached_payload
        .write_all(&vec![0xa5; PRIVATE_WRITE_BYTES])
        .expect("write private payload range");
    cached_payload.sync_all().expect("sync private payload");
    drop(cached_payload);
    let after_private_write =
        fs2::available_space(fixture.path()).expect("space after private write");
    assert_eq!(
        fs::metadata(repository.join("payload.bin"))
            .expect("source payload metadata")
            .len(),
        LOGICAL_PAYLOAD_BYTES as u64
    );
    assert!(git(&cold_view, &["status", "--porcelain=v1"]).is_empty());
    assert_eq!(
        git(&cached_view, &["status", "--porcelain=v1"]).trim_end(),
        " M payload.bin"
    );

    let cached_allocated_after =
        view_allocated_bytes(&state, &cached_result.destination, cached_result.backend);
    let report = NativeCowBenchmark {
        schema_version: 1,
        operating_system: std::env::consts::OS,
        architecture: std::env::consts::ARCH,
        backend: cached_result.backend,
        logical_payload_bytes: LOGICAL_PAYLOAD_BYTES as u64,
        cold_add_microseconds: elapsed_microseconds(cold_duration),
        cached_add_microseconds: elapsed_microseconds(cached_duration),
        cold_volume_growth_bytes: before_cold.saturating_sub(after_cold),
        cached_volume_growth_bytes: after_cold.saturating_sub(after_cached),
        private_write_bytes: PRIVATE_WRITE_BYTES as u64,
        private_write_volume_growth_bytes: before_private_write.saturating_sub(after_private_write),
        cached_view_allocated_before_write_bytes: cached_allocated_before,
        cached_view_allocated_after_write_bytes: cached_allocated_after,
    };
    let compact = serde_json::to_string(&report).expect("serialize benchmark report");
    println!("RIFTRI_BENCHMARK {compact}");
    if let Some(output) = env::var_os("RIFTRI_BENCHMARK_OUTPUT") {
        let pretty = serde_json::to_vec_pretty(&report).expect("format benchmark report");
        fs::write(output, pretty).expect("write benchmark report");
    }

    assert!(
        report.cached_volume_growth_bytes < report.logical_payload_bytes / 4,
        "cached view allocated too much data: {compact}"
    );

    fs::copy(
        repository.join("payload.bin"),
        cached_view.join("payload.bin"),
    )
    .expect("restore cached payload");
    assert!(git(&cached_view, &["status", "--porcelain=v1"]).is_empty());
    remove_worktree(RemoveWorktreeRequest {
        repository: repository.clone(),
        destination: cached_view,
        state_dir: Some(state.clone()),
    })
    .expect("remove cached benchmark view");
    remove_worktree(RemoveWorktreeRequest {
        repository,
        destination: cold_view,
        state_dir: Some(state),
    })
    .expect("remove cold benchmark view");
}

/// Milestone 6 asks for allocation and creation costs on a representative
/// monorepo, which is the case sparse profiles exist for: a tree of sibling
/// packages where a task needs one of them.
///
/// The full and sparse adds each start from their own state directory, so both
/// are genuinely cold and neither can reuse the other's base — the comparison
/// is base construction against base construction, which is where a sparse
/// profile actually saves. The third add repeats the sparse cone against the
/// warm state directory to show the cached path.
#[test]
#[ignore = "repeatable sparse monorepo allocation benchmark; run on a supported quiet volume"]
fn reports_sparse_cone_costs_on_a_monorepo_shape() {
    let fixture = tempdir().expect("benchmark fixture directory");
    let repository = fixture.path().join("repository");
    fs::create_dir(&repository).expect("create benchmark repository");
    git(&repository, &["init", "--quiet"]);
    git(&repository, &["config", "user.name", "Riftri Benchmark"]);
    git(
        &repository,
        &["config", "user.email", "riftri@example.invalid"],
    );
    git(&repository, &["config", "core.autocrlf", "false"]);

    let mut package_names = Vec::new();
    for package in 0..MONOREPO_PACKAGES {
        let name = format!("packages/pkg-{package:02}");
        let directory = repository.join(&name);
        fs::create_dir_all(&directory).expect("create benchmark package");
        for file in 0..MONOREPO_FILES_PER_PACKAGE {
            write_sized_payload(
                &directory.join(format!("blob-{file:02}.bin")),
                MONOREPO_FILE_BYTES,
            );
        }
        package_names.push(name);
    }
    git(&repository, &["add", "-A"]);
    git(
        &repository,
        &["commit", "--quiet", "-m", "monorepo payload"],
    );

    let package_bytes = (MONOREPO_FILES_PER_PACKAGE * MONOREPO_FILE_BYTES) as u64;
    let cone = vec![package_names[0].clone()];

    // Full checkout, cold.
    let full_state = fixture.path().join("state-full");
    let full_view = fixture.path().join("full-view");
    let before_full = fs2::available_space(fixture.path()).expect("space before full add");
    let full_started = Instant::now();
    let full_result = add_worktree(AddWorktreeRequest {
        repository: repository.clone(),
        destination: full_view.clone(),
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::NewBranch(OsString::from("benchmark/full")),
        state_dir: Some(full_state.clone()),
        sparse_directories: Vec::new(),
    })
    .expect("create full monorepo worktree");
    let full_duration = full_started.elapsed();
    let after_full = fs2::available_space(fixture.path()).expect("space after full add");

    // One package, cold, from a state directory that shares nothing.
    let sparse_state = fixture.path().join("state-sparse");
    let sparse_view = fixture.path().join("sparse-view");
    let before_sparse = fs2::available_space(fixture.path()).expect("space before sparse add");
    let sparse_started = Instant::now();
    let sparse_result = add_worktree(AddWorktreeRequest {
        repository: repository.clone(),
        destination: sparse_view.clone(),
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::NewBranch(OsString::from("benchmark/sparse")),
        state_dir: Some(sparse_state.clone()),
        sparse_directories: cone.clone(),
    })
    .expect("create sparse monorepo worktree");
    let sparse_duration = sparse_started.elapsed();
    let after_sparse = fs2::available_space(fixture.path()).expect("space after sparse add");

    // The same cone again, now that its base exists.
    let cached_view = fixture.path().join("sparse-cached-view");
    let before_cached = fs2::available_space(fixture.path()).expect("space before cached add");
    let cached_started = Instant::now();
    let cached_result = add_worktree(AddWorktreeRequest {
        repository: repository.clone(),
        destination: cached_view.clone(),
        revision: OsString::from("HEAD"),
        mode: WorktreeMode::NewBranch(OsString::from("benchmark/sparse-cached")),
        state_dir: Some(sparse_state.clone()),
        sparse_directories: cone.clone(),
    })
    .expect("create cached sparse monorepo worktree");
    let cached_duration = cached_started.elapsed();
    let after_cached = fs2::available_space(fixture.path()).expect("space after cached add");

    assert!(!full_result.reused_base);
    assert!(!sparse_result.reused_base);
    assert!(cached_result.reused_base);
    assert!(git(&full_view, &["status", "--porcelain=v1"]).is_empty());
    assert!(git(&sparse_view, &["status", "--porcelain=v1"]).is_empty());
    assert!(git(&cached_view, &["status", "--porcelain=v1"]).is_empty());
    // The sparse views hold the requested package and nothing else.
    assert!(sparse_view.join(&package_names[0]).is_dir());
    assert!(!sparse_view.join(&package_names[1]).exists());
    assert!(full_view.join(&package_names[1]).is_dir());

    let report = SparseMonorepoBenchmark {
        schema_version: 1,
        operating_system: env::consts::OS,
        architecture: env::consts::ARCH,
        backend: full_result.backend,
        packages: MONOREPO_PACKAGES,
        logical_tree_bytes: package_bytes * MONOREPO_PACKAGES as u64,
        cone_packages: cone.len(),
        logical_cone_bytes: package_bytes * cone.len() as u64,
        full_cold_add_microseconds: elapsed_microseconds(full_duration),
        full_cold_volume_growth_bytes: before_full.saturating_sub(after_full),
        sparse_cold_add_microseconds: elapsed_microseconds(sparse_duration),
        sparse_cold_volume_growth_bytes: before_sparse.saturating_sub(after_sparse),
        sparse_cached_add_microseconds: elapsed_microseconds(cached_duration),
        sparse_cached_volume_growth_bytes: before_cached.saturating_sub(after_cached),
        full_view_allocated_bytes: view_allocated_bytes(
            &full_state,
            &full_result.destination,
            full_result.backend,
        ),
        sparse_view_allocated_bytes: view_allocated_bytes(
            &sparse_state,
            &sparse_result.destination,
            sparse_result.backend,
        ),
    };
    let compact = serde_json::to_string(&report).expect("serialize sparse benchmark report");
    println!("RIFTRI_SPARSE_BENCHMARK {compact}");
    if let Some(output) = env::var_os("RIFTRI_SPARSE_BENCHMARK_OUTPUT") {
        let pretty = serde_json::to_vec_pretty(&report).expect("format sparse benchmark report");
        fs::write(output, pretty).expect("write sparse benchmark report");
    }

    // The claim worth holding to: materializing one package of eight costs a
    // fraction of materializing the tree. Half is a deliberately loose bound —
    // the point is that the saving is structural, not that a quiet volume
    // reproduces an exact ratio.
    assert!(
        report.sparse_cold_volume_growth_bytes * 2 < report.full_cold_volume_growth_bytes,
        "sparse base did not cost materially less than the full base: {compact}"
    );
    // Reusing a cone's base costs far less again than building it.
    assert!(
        report.sparse_cached_volume_growth_bytes < report.sparse_cold_volume_growth_bytes,
        "cached sparse view cost no less than building its base: {compact}"
    );

    for (state, destination) in [
        (&full_state, full_view),
        (&sparse_state, sparse_view),
        (&sparse_state, cached_view),
    ] {
        remove_worktree(RemoveWorktreeRequest {
            repository: repository.clone(),
            destination,
            state_dir: Some(state.clone()),
        })
        .expect("remove benchmark view");
    }
}
