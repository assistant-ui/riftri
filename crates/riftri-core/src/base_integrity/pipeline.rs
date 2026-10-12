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

#[derive(Debug)]
enum EncodedLeaf {
    Buffered(Vec<u8>),
    Stream {
        header: Vec<u8>,
        file: File,
        length: u64,
    },
}

type Encoded = io::Result<Option<EncodedLeaf>>;

enum Chunk {
    Complete(Vec<u8>),
    Bytes(Vec<u8>),
    End,
    Serial,
}

struct Job {
    path: PathBuf,
    name: OsString,
    result: SyncSender<io::Result<Chunk>>,
}

struct Pending {
    path: PathBuf,
    name: OsString,
    result: Receiver<io::Result<Chunk>>,
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
    // A directory replacing an enumerated leaf still uses the original walk.
    if metadata.is_dir() {
        return Ok(None);
    }
    let mut encoded = EntryBytes::default();
    hash_native(name, &mut encoded);
    if metadata.is_file() && metadata.len() > MAX_FILE_BYTES {
        hash_v2_metadata(path, &metadata, &mut encoded, scratch)?;
        sha2::digest::Update::update(&mut encoded, b"file");
        sha2::digest::Update::update(&mut encoded, &metadata.len().to_le_bytes());
        if encoded.overflow {
            return Ok(None);
        }
        return Ok(Some(EncodedLeaf::Stream {
            header: encoded.bytes,
            file: open_regular(path)?,
            length: metadata.len(),
        }));
    }
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
    Ok((!encoded.overflow).then_some(EncodedLeaf::Buffered(encoded.bytes)))
}

fn stream_reader(
    mut reader: impl Read,
    expected_length: u64,
    buffer: &mut [u8],
    mut emit: impl FnMut(&[u8]) -> io::Result<()>,
) -> io::Result<()> {
    let mut length = 0;
    loop {
        let count = reader.read(buffer)?;
        if count == 0 {
            break;
        }
        length += count as u64;
        // A dropped consumer stops reading at this chunk, rather than draining
        // a potentially huge file while the scoped owner waits for its worker.
        emit(&buffer[..count])?;
    }
    if length != expected_length {
        return Err(io::Error::other(ChangedWhileReading));
    }
    Ok(())
}

fn send_encoded(
    encoded: Encoded,
    sender: &SyncSender<io::Result<Chunk>>,
    scratch: &mut HashScratch,
) {
    let (header, file, length) = match encoded {
        Ok(Some(EncodedLeaf::Stream {
            header,
            file,
            length,
        })) => (header, file, length),
        other => {
            let message = other.map(|entry| match entry {
                Some(EncodedLeaf::Buffered(bytes)) => Chunk::Complete(bytes),
                None => Chunk::Serial,
                Some(EncodedLeaf::Stream { .. }) => unreachable!(),
            });
            let _ = sender.send(message);
            return;
        }
    };
    if sender.send(Ok(Chunk::Bytes(header))).is_err() {
        return;
    }
    let result = stream_reader(file, length, &mut scratch.file_bytes, |bytes| {
        sender
            .send(Ok(Chunk::Bytes(bytes.to_vec())))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "integrity consumer stopped"))
    });
    let _ = sender.send(result.map(|()| Chunk::End));
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
                    // Large files use the same one-slot queue as small leaves.
                    // Each payload is at most 64 KiB; an entry header is at most
                    // 512 KiB. Dropping pending receivers releases blocked sends.
                    send_encoded(result, &job.result, &mut scratch);
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
        let walked = pipeline.walk(root);
        if pipeline.failed {
            return walked;
        }
        // A pending earlier leaf error takes precedence over a later walk
        // error. Scope joins every worker after dropping all queue owners.
        pipeline.flush().and(walked)
    })
}

impl Pipeline<'_> {
    fn walk(&mut self, path: &Path) -> io::Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_dir() {
            return hash_entry_with_metadata(
                path,
                metadata,
                self.digest,
                MarkerVersion::V2,
                SpecialEntries::Refuse,
                &mut self.scratch,
            );
        }
        hash_v2_metadata(path, &metadata, self.digest, &mut self.scratch)?;
        self.digest.update(b"directory");
        let mut entries = fs::read_dir(path)?
            .map(|entry| entry.map(|entry| (entry.file_name(), entry.file_type())))
            .collect::<io::Result<Vec<_>>>()?;
        entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        self.digest.update((entries.len() as u64).to_le_bytes());
        let mut child = path.to_path_buf();
        for (name, kind) in entries {
            child.push(&name);
            if kind?.is_dir() {
                self.flush()?;
                hash_native(&name, self.digest);
                self.walk(&child)?;
            } else {
                self.enqueue(&child, name)?;
            }
            child.pop();
        }
        self.flush()
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
        self.pending.push_back(Pending {
            path: path.to_path_buf(),
            name,
            result: receiver,
        });
        Ok(())
    }

    fn flush_one(&mut self) -> io::Result<()> {
        let Some(pending) = self.pending.pop_front() else {
            return Ok(());
        };
        let result = (|| loop {
            let chunk = pending
                .result
                .recv()
                .map_err(|_| io::Error::other("integrity reader stopped"))??;
            match chunk {
                Chunk::Complete(bytes) => {
                    self.digest.update(bytes);
                    break Ok(());
                }
                Chunk::Bytes(bytes) => self.digest.update(bytes),
                Chunk::End => break Ok(()),
                Chunk::Serial => {
                    hash_native(&pending.name, self.digest);
                    hash_entry(
                        &pending.path,
                        self.digest,
                        MarkerVersion::V2,
                        SpecialEntries::Refuse,
                        &mut self.scratch,
                    )?;
                    break Ok(());
                }
            }
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
    fn large_regular_files_are_assigned_to_a_worker() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("large");
        fs::write(&path, vec![7; MAX_FILE_BYTES as usize * 3 + 17]).unwrap();
        assert!(
            encode_leaf(&path, OsStr::new("large"), &mut HashScratch::new())
                .unwrap()
                .is_some(),
            "large regular files must not fall back to coordinator reads"
        );
    }

    #[test]
    fn streamed_chunks_are_bounded_and_match_the_serial_entry() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("large");
        fs::write(&path, vec![7; MAX_FILE_BYTES as usize * 5 + 17]).unwrap();
        let name = OsStr::new("large");
        let mut expected = Sha256::new();
        hash_native(name, &mut expected);
        hash_entry(
            &path,
            &mut expected,
            MarkerVersion::V2,
            SpecialEntries::Refuse,
            &mut HashScratch::new(),
        )
        .unwrap();
        let mut actual = Sha256::new();
        let mut chunks = 0;
        std::thread::scope(|scope| {
            let (sender, receiver) = mpsc::sync_channel(1);
            let path = &path;
            scope.spawn(move || {
                let mut scratch = HashScratch::new();
                let encoded = encode_leaf(path, name, &mut scratch);
                send_encoded(encoded, &sender, &mut scratch);
            });
            loop {
                match receiver.recv().unwrap().unwrap() {
                    Chunk::Bytes(bytes) => {
                        assert!(bytes.len() <= MAX_FILE_BYTES as usize);
                        chunks += 1;
                        actual.update(bytes);
                    }
                    Chunk::End => break,
                    _ => panic!("large input did not stream"),
                }
            }
        });
        assert!(chunks >= 7, "header plus at least six content chunks");
        assert_eq!(actual.finalize(), expected.finalize());
    }

    #[test]
    fn stream_length_changes_and_read_errors_fail_closed() {
        for expected_length in [2, 4] {
            let error =
                stream_reader(&b"abc"[..], expected_length, &mut [0; 2], |_| Ok(())).unwrap_err();
            assert!(error.get_ref().unwrap().is::<ChangedWhileReading>());
        }
        struct FailedReader;
        impl Read for FailedReader {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            }
        }
        assert_eq!(
            stream_reader(FailedReader, 1, &mut [0; 2], |_| Ok(()))
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn consumer_failure_stops_before_another_read() {
        struct CountingReader<'a>(&'a std::cell::Cell<usize>);
        impl Read for CountingReader<'_> {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                self.0.set(self.0.get() + 1);
                bytes.fill(1);
                Ok(bytes.len())
            }
        }
        let reads = std::cell::Cell::new(0);
        let error = stream_reader(CountingReader(&reads), 100, &mut [0; 16], |_| {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(reads.get(), 1);
    }

    #[test]
    fn dropping_a_stream_receiver_releases_its_producer() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("large");
        fs::write(&path, vec![1; MAX_FILE_BYTES as usize * 8]).unwrap();
        std::thread::scope(|scope| {
            let (sender, receiver) = mpsc::sync_channel(1);
            let worker = scope.spawn(move || {
                let mut scratch = HashScratch::new();
                let encoded = encode_leaf(&path, OsStr::new("large"), &mut scratch);
                send_encoded(encoded, &sender, &mut scratch);
            });
            assert!(matches!(receiver.recv().unwrap().unwrap(), Chunk::Bytes(_)));
            drop(receiver);
            worker.join().unwrap();
        });
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
    fn large_files_stream_and_missing_files_fail() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("large");
        fs::write(&path, vec![1; MAX_FILE_BYTES as usize + 1]).unwrap();
        assert!(matches!(
            encode_leaf(&path, OsStr::new("large"), &mut HashScratch::new()).unwrap(),
            Some(EncodedLeaf::Stream { .. })
        ));
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
            fs::write(
                fixture.path().join(format!("{i:03}")),
                vec![1; MAX_FILE_BYTES as usize * 3],
            )
            .unwrap();
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
