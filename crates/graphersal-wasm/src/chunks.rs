//! A graph file read in chunks from JavaScript, so the importers stream it instead of taking it
//! as one byte array: the file is never copied into the module's memory as a whole (the linear
//! memory of a WebAssembly module never shrinks, so a whole-file copy would stay reserved for
//! the worker's life).
//!
//! The worker passes a [`ChunkSource`]: `{size, read(offset, length)}` over a `File` or `Blob`,
//! read synchronously with `FileReaderSync` (`playground/js/engine/worker.js`). A `File` the user
//! picked is a reference to the file on disk; reading it after the file changed on disk fails,
//! and that failure is the reader's error.

use std::io::{self, BufReader, Read, Seek, SeekFrom};

use wasm_bindgen::prelude::*;

/// The size of one chunk asked from JavaScript.
pub(crate) const CHUNK_BYTES: usize = 1 << 20;

#[wasm_bindgen]
extern "C" {
    /// A file readable in chunks: `size` in bytes and `read(offset, length)`, which answers the
    /// bytes from `offset` (fewer at the end) or throws a message string.
    pub type ChunkSource;

    /// The size of the file in bytes.
    #[wasm_bindgen(method, getter)]
    fn size(this: &ChunkSource) -> f64;

    /// The bytes `offset .. offset + length` (fewer at the end of the file).
    #[wasm_bindgen(method, catch)]
    fn read(this: &ChunkSource, offset: f64, length: u32) -> Result<Vec<u8>, JsValue>;
}

/// [`ChunkSource`] as a [`Read`] + [`Seek`] stream.
pub(crate) struct ChunkReader {
    source: ChunkSource,
    position: u64,
    size: u64,
}

impl ChunkReader {
    /// A reader at the start of `source`, buffered by one chunk.
    pub(crate) fn buffered(source: ChunkSource) -> BufReader<Self> {
        let size = source.size().max(0.0) as u64;
        BufReader::with_capacity(
            CHUNK_BYTES,
            Self {
                source,
                position: 0,
                size,
            },
        )
    }
}

impl Read for ChunkReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self.size.saturating_sub(self.position);
        let wanted = (buf.len() as u64).min(left).min(CHUNK_BYTES as u64) as usize;
        if wanted == 0 {
            return Ok(0);
        }
        let chunk = self
            .source
            .read(self.position as f64, wanted as u32)
            .map_err(|err| {
                let message = err.as_string().unwrap_or_else(|| {
                    "the file could not be read (it may have changed or been removed on disk \
                     since it was loaded)"
                        .to_string()
                });
                io::Error::other(message)
            })?;
        let n = chunk.len().min(buf.len());
        buf[..n].copy_from_slice(&chunk[..n]);
        self.position += n as u64;
        Ok(n)
    }
}

impl Seek for ChunkReader {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let target = match to {
            SeekFrom::Start(at) => Some(at),
            SeekFrom::End(delta) => self.size.checked_add_signed(delta),
            SeekFrom::Current(delta) => self.position.checked_add_signed(delta),
        };
        let Some(target) = target else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a seek before the start of the file",
            ));
        };
        self.position = target;
        Ok(target)
    }
}
