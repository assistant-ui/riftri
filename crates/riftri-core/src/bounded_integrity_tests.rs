use super::{ChangedWhileReading, hash_file_bytes, hash_reader_bytes};
use sha2::{Digest, Sha256};
use std::io::{self, Cursor, Read};

struct CountedReader {
    bytes: Cursor<Vec<u8>>,
    read: usize,
}

impl Read for CountedReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = self.bytes.read(buffer)?;
        self.read += count;
        Ok(count)
    }
}

#[test]
fn oversized_integrity_inputs_stop_after_one_extra_byte() {
    for expected in [0, 1, 65535, 65536, 65537] {
        let mut reader = CountedReader {
            bytes: Cursor::new(vec![42; 4 * 1024 * 1024]),
            read: 0,
        };
        let error = hash_reader_bytes(&mut reader, expected, &mut Sha256::new(), &mut [0; 65536])
            .unwrap_err();
        assert!(
            error
                .get_ref()
                .is_some_and(|error| error.is::<ChangedWhileReading>())
        );
        assert_eq!(reader.read as u64, expected + 1);
    }
}

#[test]
fn bounded_integrity_hashes_all_valid_bytes_and_rejects_truncation() {
    for length in [0, 1, 63, 64, 65, 65535, 65536, 65537] {
        let data = vec![42; length];
        for buffer_size in [1, 17, 65536] {
            let mut digest = Sha256::new();
            hash_reader_bytes(
                &mut Cursor::new(&data),
                length as u64,
                &mut digest,
                &mut vec![0; buffer_size],
            )
            .unwrap();
            assert_eq!(digest.finalize(), Sha256::digest(&data));
        }
        let error = hash_reader_bytes(
            &mut Cursor::new(&data),
            length as u64 + 1,
            &mut Sha256::new(),
            &mut [0; 65536],
        )
        .unwrap_err();
        assert!(
            error
                .get_ref()
                .is_some_and(|error| error.is::<ChangedWhileReading>())
        );
    }
    // An expected length at u64::MAX must not wrap the read budget to zero.
    let mut reader = CountedReader {
        bytes: Cursor::new(vec![42; 1]),
        read: 0,
    };
    assert!(hash_reader_bytes(&mut reader, u64::MAX, &mut Sha256::new(), &mut [0; 16]).is_err());
    assert_eq!(reader.read, 1);
}

#[test]
fn integrity_growth_after_metadata_is_rejected_before_hashing_excess() {
    let fixture = tempfile::tempdir().unwrap();
    let path = fixture.path().join("growing");
    std::fs::write(&path, b"x").unwrap();
    let expected = std::fs::metadata(&path).unwrap().len();
    std::fs::write(&path, vec![42; 4 * 1024 * 1024]).unwrap();
    let mut digest = Sha256::new();
    assert!(hash_file_bytes(&path, expected, &mut digest, &mut [0; 65536]).is_err());
    assert_eq!(digest.finalize(), Sha256::digest([]));
}
