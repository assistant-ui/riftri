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
        for chunk in block.chunks_exact_mut(8) {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            chunk.copy_from_slice(&state.to_le_bytes());
        }
        file.write_all(&block).expect("write benchmark payload");
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
