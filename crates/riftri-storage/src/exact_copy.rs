use std::io::{ErrorKind, Read, Write};

/// Copy exactly `expected` bytes from `reader` to `writer`.
///
/// `std::io::copy` over a `Read::take` adaptor reports success after a short
/// read, so a source that shrinks mid-copy (concurrent truncation, a stale
/// recorded size) would leave the destination incomplete while the copy claims
/// to have succeeded. This wrapper compares the copied count against the
/// expected length and surfaces any mismatch as an explicit error carrying
/// both counts.
pub(crate) fn copy_exact<R, W>(reader: &mut R, writer: &mut W, expected: u64) -> std::io::Result<()>
where
    R: Read + ?Sized,
    W: Write + ?Sized,
{
    let copied = std::io::copy(&mut reader.take(expected), writer)?;
    if copied != expected {
        return Err(std::io::Error::new(
            ErrorKind::UnexpectedEof,
            format!("source ended after {copied} of {expected} expected bytes"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::copy_exact;

    /// A reader over `contents` that ends early, after `limit` bytes, the way
    /// a file truncated behind a stale recorded size does.
    struct TruncatingReader {
        contents: Vec<u8>,
        limit: usize,
        position: usize,
    }

    impl Read for TruncatingReader {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let end = self.limit.min(self.contents.len());
            let count = end.saturating_sub(self.position).min(buffer.len());
            buffer[..count].copy_from_slice(&self.contents[self.position..self.position + count]);
            self.position += count;
            Ok(count)
        }
    }

    #[test]
    fn copies_the_expected_length_exactly_and_no_further() {
        let mut reader = TruncatingReader {
            contents: b"tail bytes plus trailing data".to_vec(),
            limit: 29,
            position: 0,
        };
        let mut written = Vec::new();

        copy_exact(&mut reader, &mut written, 10).expect("full-length copy succeeds");

        assert_eq!(written, b"tail bytes");
        assert_eq!(
            reader.position, 10,
            "copy must not read past the expected length"
        );
    }

    #[test]
    fn reports_expected_and_actual_counts_when_the_source_ends_early() {
        let mut reader = TruncatingReader {
            contents: b"tail bytes".to_vec(),
            limit: 4,
            position: 0,
        };
        let mut written = Vec::new();

        let error = copy_exact(&mut reader, &mut written, 10).expect_err("short read must fail");

        assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
        assert_eq!(
            error.to_string(),
            "source ended after 4 of 10 expected bytes"
        );
        assert_eq!(
            written, b"tail",
            "bytes read before truncation are still written"
        );
    }

    #[test]
    fn zero_length_copies_succeed_without_reading() {
        let mut reader = TruncatingReader {
            contents: Vec::new(),
            limit: 0,
            position: 0,
        };
        let mut written = Vec::new();

        copy_exact(&mut reader, &mut written, 0).expect("empty copy succeeds");

        assert!(written.is_empty());
    }
}
