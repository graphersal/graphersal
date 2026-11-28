// A File or Blob read in chunks by the engine module (graphersal-wasm's `ChunkSource`): an
// import takes the file slice by slice, so neither the page nor the module's memory ever holds
// it whole (a WebAssembly module's memory never shrinks). Used by the worker; pure apart from
// the injected reader, so Node tests run it with a fake.

/** Whether `data` is a File or Blob (posted to a worker by reference: its bytes are not copied). */
export function isBlob(data) {
  return typeof Blob !== 'undefined' && data instanceof Blob;
}

/**
 * The module's `ChunkSource` over a File or Blob: `size` and `read(offset, length)`, which reads
 * that slice synchronously (`FileReaderSync`, workers only). A File the user picked is a
 * reference to the file on disk; reading it after the file changed or was removed throws, and
 * the thrown message (a string, which the module turns into the import's error) says so.
 * @param {Blob} blob
 * @param {new () => {readAsArrayBuffer(blob: Blob): ArrayBuffer}} [Reader]
 */
export function chunkSource(blob, Reader = globalThis.FileReaderSync) {
  return {
    size: blob.size,
    read(offset, length) {
      try {
        return new Uint8Array(new Reader().readAsArrayBuffer(blob.slice(offset, offset + length)));
      } catch (err) {
        const name = blob.name ? `"${blob.name}"` : 'the file';
        throw `${name} could not be read: it may have changed or been removed on disk since it was loaded (${err && err.message ? err.message : String(err)})`;
      }
    },
  };
}
