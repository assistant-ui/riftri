//! Experimental bounded leaf reads. The coordinator hashes the original byte
//! stream in sorted depth-first order; workers do not produce separate digests.
use super::*;
use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, SyncSender};

const MAX_WORKERS: usize = 4;
const MAX_PENDING: usize = 8;
const MAX_FILE_BYTES: u64 = 64 * 1024;
const MAX_ENTRY_BYTES: usize = 512 * 1024;

#[derive(Default)]
struct EntryBytes {
    bytes: Vec<u8>,
    overflow: bool,
}

impl sha2::digest::Update for EntryBytes {
    fn update(&mut self, bytes: &[u8]) {
        if self.overflow || bytes.len() > MAX_ENTRY_BYTES - self.bytes.len() {
            self.overflow = true;
        } else {
            let needed = self.bytes.len() + bytes.len();
            if needed > self.bytes.capacity() {
                let capacity = needed.max(self.bytes.capacity() * 2).min(MAX_ENTRY_BYTES);
                self.bytes.reserve_exact(capacity - self.bytes.len());
            }
            self.bytes.extend_from_slice(bytes);
        }
    }
}

type Encoded = io::Result<Option<Vec<u8>>>;

struct Job {
    path: PathBuf,
    name: OsString,
    result: SyncSender<Encoded>,
}

struct PendingLeaf {
    path: PathBuf,
    name: OsString,
    result: Receiver<Encoded>,
}

enum Pending {
    Header(Vec<u8>),
    Leaf(PendingLeaf),
}

struct Pipeline<'a> {
    digest: &'a mut Sha256,
    scratch: HashScratch,
    senders: Vec<SyncSender<Job>>,
    pending: VecDeque<Pending>,
    next: usize,
    failed: bool,
}

fn encode_leaf(path: &Path, name: &OsStr, scratch: &mut HashScratch) -> Encoded {
    let metadata = fs::symlink_metadata(path)?;
    // Large files keep the original streaming path. A directory replacing an
    // enumerated leaf also goes through the original complete traversal.
    if metadata.is_dir() || (metadata.is_file() && metadata.len() > MAX_FILE_BYTES) {
        return Ok(None);
    }
    let mut encoded = EntryBytes::default();
    hash_native(name, &mut encoded);
    hash_entry_with_metadata(
        path,
        metadata,
        &mut encoded,
        MarkerVersion::V2,
        SpecialEntries::Refuse,
        scratch,
    )?;
    // Large xattr sets cannot make the queue unbounded. Re-read them serially;
    // an overflow is never accepted as a truncated integrity observation.
    Ok((!encoded.overflow).then_some(encoded.bytes))
}

pub(super) fn hash(root: &Path, digest: &mut Sha256) -> io::Result<()> {
    let workers = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .min(MAX_WORKERS);
    if workers == 1 {
        return hash_entry(
            root,
            digest,
            MarkerVersion::V2,
            SpecialEntries::Refuse,
            &mut HashScratch::new(),
        );
    }
    hash_with_encoder(root, digest, workers, &encode_leaf)
}

fn hash_with_encoder(
    root: &Path,
    digest: &mut Sha256,
    workers: usize,
    encode: &(impl Fn(&Path, &OsStr, &mut HashScratch) -> Encoded + Sync),
) -> io::Result<()> {
    std::thread::scope(|scope| {
        let mut senders = Vec::with_capacity(workers);
        for _ in 0..workers {
            let (sender, receiver) = mpsc::sync_channel::<Job>(1);
            std::thread::Builder::new().spawn_scoped(scope, move || {
                let mut scratch = HashScratch::new();
                while let Ok(job) = receiver.recv() {
                    let result = encode(&job.path, &job.name, &mut scratch);
                    // Each job has a one-slot response queue, so dropping the
                    // owner on any error never strands a worker on a send.
                    let _ = job.result.send(result);
                }
            })?;
            senders.push(sender);
        }
        let mut pipeline = Pipeline {
            digest,
            scratch: HashScratch::new(),
            senders,
            pending: VecDeque::new(),
            next: 0,
            failed: false,
        };
        let walked = pipeline.walk(root, None);
        if pipeline.failed {
            return walked;
        }
        // A pending earlier leaf error takes precedence over a later walk
        // error. Scope joins every worker after dropping all queue owners.
        pipeline.flush().and(walked)
    })
}

impl Pipeline<'_> {
    fn walk(&mut self, path: &Path, name: Option<&OsStr>) -> io::Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_dir() {
            // A directory may have changed kind since enumeration. Keep the
            // serial fallback and ordered earlier errors, without following it.
            self.flush()?;
            if let Some(name) = name {
                hash_native(name, self.digest);
            }
            return hash_entry_with_metadata(
                path,
                metadata,
                self.digest,
                MarkerVersion::V2,
                SpecialEntries::Refuse,
                &mut self.scratch,
            );
        }
        let mut header = EntryBytes::default();
        if let Some(name) = name {
            hash_native(name, &mut header);
        }
        hash_v2_metadata(path, &metadata, &mut header, &mut self.scratch)?;
        sha2::digest::Update::update(&mut header, b"directory");
        let mut entries = fs::read_dir(path)?
            .map(|entry| entry.map(|entry| (entry.file_name(), entry.file_type())))
            .collect::<io::Result<Vec<_>>>()?;
        entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        sha2::digest::Update::update(&mut header, &(entries.len() as u64).to_le_bytes());
        if header.overflow {
            // A directory's xattrs have the same bound as leaf metadata. Never
            // hash a truncated header: drain and re-read it in original order.
            self.flush()?;
            if let Some(name) = name {
                hash_native(name, self.digest);
            }
            hash_v2_metadata(path, &metadata, self.digest, &mut self.scratch)?;
            self.digest.update(b"directory");
            self.digest.update((entries.len() as u64).to_le_bytes());
        } else {
            if self.pending.len() == MAX_PENDING {
                self.flush_one()?;
            }
            self.pending.push_back(Pending::Header(header.bytes));
        }
        let mut child = path.to_path_buf();
        for (name, kind) in entries {
            child.push(&name);
            if kind?.is_dir() {
                self.walk(&child, Some(&name))?;
            } else {
                self.enqueue(&child, name)?;
            }
            child.pop();
        }
        // Headers and leaves share the bounded ordered queue. The caller
        // drains it once at completion instead of stopping at every folder.
        Ok(())
    }

    fn enqueue(&mut self, path: &Path, name: OsString) -> io::Result<()> {
        if self.pending.len() == MAX_PENDING {
            self.flush_one()?;
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        self.senders[self.next]
            .send(Job {
                path: path.to_path_buf(),
                name: name.clone(),
                result: sender,
            })
            .map_err(|_| io::Error::other("integrity reader stopped"))?;
        self.next = (self.next + 1) % self.senders.len();
        self.pending.push_back(Pending::Leaf(PendingLeaf {
            path: path.to_path_buf(),
            name,
            result: receiver,
        }));
        Ok(())
    }

    fn flush_one(&mut self) -> io::Result<()> {
        let Some(pending) = self.pending.pop_front() else {
            return Ok(());
        };
        let pending = match pending {
            Pending::Header(bytes) => {
                self.digest.update(bytes);
                return Ok(());
            }
            Pending::Leaf(leaf) => leaf,
        };
        let result = (|| {
            let bytes = pending
                .result
                .recv()
                .map_err(|_| io::Error::other("integrity reader stopped"))??;
            match bytes {
                Some(bytes) => self.digest.update(bytes),
                None => {
                    hash_native(&pending.name, self.digest);
                    hash_entry(
                        &pending.path,
                        self.digest,
                        MarkerVersion::V2,
                        SpecialEntries::Refuse,
                        &mut self.scratch,
                    )?;
                }
            }
            Ok(())
        })();
        self.failed |= result.is_err();
        result
    }

    fn flush(&mut self) -> io::Result<()> {
        while !self.pending.is_empty() {
            self.flush_one()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readers_overlap_across_directory_boundaries() {
        use std::sync::{Condvar, Mutex};
        use std::time::Duration;
        let fixture = tempfile::tempdir().unwrap();
        for i in 0..4 {
            let directory = fixture.path().join(format!("directory-{i}"));
            fs::create_dir(&directory).unwrap();
            fs::write(directory.join("leaf"), [i]).unwrap();
        }
        let ready = Mutex::new(0);
        let changed = Condvar::new();
        let mut actual = Sha256::new();
        hash_with_encoder(fixture.path(), &mut actual, 4, &|path, name, scratch| {
            let mut started = ready.lock().unwrap();
            *started += 1;
            changed.notify_all();
            let (started, _) = changed
                .wait_timeout_while(started, Duration::from_secs(3), |count| *count < 4)
                .unwrap();
            if *started < 4 {
                return Err(io::Error::other(
                    "directory boundary drained before readers overlapped",
                ));
            }
            drop(started);
            encode_leaf(path, name, scratch)
        })
        .unwrap();
        let mut expected = Sha256::new();
        hash_entry(
            fixture.path(),
            &mut expected,
            MarkerVersion::V2,
            SpecialEntries::Refuse,
            &mut HashScratch::new(),
        )
        .unwrap();
        assert_eq!(actual.finalize(), expected.finalize());
    }

    #[test]
    fn queued_headers_preserve_serial_bytes_with_one_two_and_four_readers() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let fixture = tempfile::tempdir().unwrap();
        for i in 0..12 {
            let directory = fixture.path().join(format!("directory-{i:02}/nested"));
            fs::create_dir_all(directory.join("empty")).unwrap();
            fs::write(directory.join("small"), [i; 1024]).unwrap();
            fs::write(
                directory.join("large"),
                vec![i; MAX_FILE_BYTES as usize + 1],
            )
            .unwrap();
            symlink("small", directory.join("link-λ")).unwrap();
        }
        let special = fixture.path().join("directory-03");
        fs::set_permissions(&special, fs::Permissions::from_mode(0o1755)).unwrap();
        // Overflow a directory header, not a leaf. All metadata must still be
        // re-read and hashed in order, never silently dropped from the digest.
        for i in 0..3 {
            rustix::fs::setxattr(
                &special,
                format!("com.riftri.directory-{i}").as_str(),
                &vec![i; 192 * 1024],
                rustix::fs::XattrFlags::empty(),
            )
            .unwrap();
        }
        let mut expected = Sha256::new();
        hash_entry(
            fixture.path(),
            &mut expected,
            MarkerVersion::V2,
            SpecialEntries::Refuse,
            &mut HashScratch::new(),
        )
        .unwrap();
        let expected = expected.finalize();
        for workers in [1, 2, 4] {
            let mut actual = Sha256::new();
            hash_with_encoder(fixture.path(), &mut actual, workers, &encode_leaf).unwrap();
            assert_eq!(actual.finalize(), expected, "{workers} readers");
        }
    }

    #[test]
    fn errors_across_directories_remain_ordered_and_readers_settle() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let fixture = tempfile::tempdir().unwrap();
        for i in 0..12 {
            let directory = fixture.path().join(format!("{i:02}"));
            fs::create_dir(&directory).unwrap();
            fs::write(directory.join("leaf"), b"data").unwrap();
        }
        let started = AtomicUsize::new(0);
        let finished = AtomicUsize::new(0);
        let error = hash_with_encoder(
            fixture.path(),
            &mut Sha256::new(),
            4,
            &|path, name, scratch| {
                started.fetch_add(1, Ordering::SeqCst);
                let directory = path.parent().unwrap().file_name().unwrap();
                let result = if directory == "00" {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    Err(io::Error::other("first directory"))
                } else if directory == "01" {
                    Err(io::Error::other("later directory"))
                } else {
                    encode_leaf(path, name, scratch)
                };
                finished.fetch_add(1, Ordering::SeqCst);
                result
            },
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "first directory");
        assert!(started.load(Ordering::SeqCst) >= 2);
        assert_eq!(
            started.load(Ordering::SeqCst),
            finished.load(Ordering::SeqCst)
        );
    }

    #[test]
    fn encoded_bytes_are_strictly_bounded() {
        use sha2::digest::Update;
        let mut encoded = EntryBytes::default();
        encoded.update(&vec![1; MAX_ENTRY_BYTES]);
        assert!(!encoded.overflow);
        encoded.update(&[2]);
        assert!(encoded.overflow);
        encoded.update(&vec![3; MAX_ENTRY_BYTES]);
        assert_eq!(encoded.bytes.len(), MAX_ENTRY_BYTES);
        assert_eq!(encoded.bytes.capacity(), MAX_ENTRY_BYTES);
    }

    #[test]
    fn large_files_use_serial_fallback_and_missing_files_fail() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("large");
        fs::write(&path, vec![1; MAX_FILE_BYTES as usize + 1]).unwrap();
        assert!(
            encode_leaf(&path, OsStr::new("large"), &mut HashScratch::new())
                .unwrap()
                .is_none()
        );
        fs::remove_file(&path).unwrap();
        assert_eq!(
            encode_leaf(&path, OsStr::new("large"), &mut HashScratch::new())
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn oversized_metadata_falls_back_without_truncating_the_marker() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("file");
        fs::write(&path, b"payload").unwrap();
        for i in 0..3 {
            rustix::fs::setxattr(
                &path,
                format!("com.riftri.large-{i}").as_str(),
                &vec![i; 192 * 1024],
                rustix::fs::XattrFlags::empty(),
            )
            .unwrap();
        }
        assert!(
            encode_leaf(&path, OsStr::new("file"), &mut HashScratch::new())
                .unwrap()
                .is_none()
        );
        let mut expected = Sha256::new();
        hash_entry(
            fixture.path(),
            &mut expected,
            MarkerVersion::V2,
            SpecialEntries::Refuse,
            &mut HashScratch::new(),
        )
        .unwrap();
        let mut actual = Sha256::new();
        hash(fixture.path(), &mut actual).unwrap();
        assert_eq!(actual.finalize(), expected.finalize());
    }

    #[test]
    fn errors_keep_path_order_and_all_queued_workers_settle() {
        use std::sync::{
            Barrier,
            atomic::{AtomicUsize, Ordering},
        };
        let fixture = tempfile::tempdir().unwrap();
        for i in 0..12 {
            fs::write(fixture.path().join(format!("{i:03}")), b"payload").unwrap();
        }
        let barrier = Barrier::new(4);
        let started = AtomicUsize::new(0);
        let finished = AtomicUsize::new(0);
        let error = hash_with_encoder(
            fixture.path(),
            &mut Sha256::new(),
            4,
            &|path, name, scratch| {
                started.fetch_add(1, Ordering::SeqCst);
                if name < OsStr::new("004") {
                    barrier.wait();
                }
                let result = if name == "000" {
                    Err(io::Error::other("first leaf"))
                } else if name == "001" {
                    Err(io::Error::other("later leaf"))
                } else {
                    encode_leaf(path, name, scratch)
                };
                finished.fetch_add(1, Ordering::SeqCst);
                result
            },
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "first leaf");
        assert_eq!(started.load(Ordering::SeqCst), MAX_PENDING);
        assert_eq!(finished.load(Ordering::SeqCst), MAX_PENDING);
    }

    #[test]
    fn out_of_order_completion_preserves_serial_digest() {
        use std::sync::Barrier;
        let fixture = tempfile::tempdir().unwrap();
        for i in 0..8 {
            fs::write(fixture.path().join(format!("{i:03}")), vec![i; 1024]).unwrap();
        }
        let barrier = Barrier::new(4);
        let mut actual = Sha256::new();
        hash_with_encoder(fixture.path(), &mut actual, 4, &|path, name, scratch| {
            if name < OsStr::new("004") {
                barrier.wait();
            }
            if name == "000" {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            encode_leaf(path, name, scratch)
        })
        .unwrap();
        let mut expected = Sha256::new();
        hash_entry(
            fixture.path(),
            &mut expected,
            MarkerVersion::V2,
            SpecialEntries::Refuse,
            &mut HashScratch::new(),
        )
        .unwrap();
        assert_eq!(actual.finalize(), expected.finalize());
    }
}
