// The JavaScript side of the WebAssembly boundary.
//
// The Rust engine exports plain C functions. To hand it a file we:
//   1. ask it to allocate `len` bytes inside its linear memory,
//   2. copy the file there,
//   3. pass the (pointer, length) pair to an exported function.
// No bindings generator, no magic: ~30 lines you can read.

export async function loadEngine(url = "./pkg/galapagos_engine.wasm") {
  const { instance } = await WebAssembly.instantiateStreaming(fetch(url), {});
  const wasm = instance.exports;

  function upload(bytes) {
    const ptr = wasm.alloc(bytes.length);
    new Uint8Array(wasm.memory.buffer, ptr, bytes.length).set(bytes);
    return [ptr, bytes.length];
  }

  return {
    wasm,
    upload,
    worldsSectionCount(bytes) {
      return wasm.worlds_section_count(...upload(bytes));
    },
  };
}
