# visco

A modular codec visualizer written in Rust with [GPUI](https://gpui.rs). Open a file and step through
how it is encoded or decoded, one stage at a time. You can click any block to see its real
intermediate data, and see which bits in the file encode it.

JPEG is the first codec, with a JPEG encoder and decoder written from scratch so that every stage
shows data the codec actually produced. FLAC is next.

## Features

- **Stage pipeline.**
  - Encoding: color conversion, chroma subsampling, DCT, quantization tables, quantized coefficients,
    entropy coding (bits spent per block), Huffman tables, and the decoded result with an error map
    and PSNR.
  - Decoding: marker segments, tables, entropy decoding, dequantization, inverse DCT, upsampling,
    and the output image.
- **Block inspector.** Click an 8×8 block to see its samples, DCT coefficients, quantization table,
  quantized values, zigzag sequence, dequantized values, reconstruction and error. It also lists
  every coded symbol with its exact bits.
- **Live parameters.** Quality, chroma subsampling, restart interval and grayscale re-encode in the
  background as you drag. The selected block keeps its place, so its detail updates live.
- **Bitstream view.** A hex dump with marker segments highlighted. Hovering a byte shows its bits and
  the symbol they encode (DC difference, AC run/size, EOB, ZRL, refinement bits). Clicking selects
  the block that owns that symbol, and selecting a block highlights its bytes.
- **Progressive scans.** A scrubber steps through a progressive JPEG scan by scan, showing the image
  after each pass.
- **Viewer.** Zoom with ⌘-scroll or pinch and drag to pan. Pixels stay crisp at high zoom and the
  block grid stays aligned.

## JPEG support

| | Supported |
|---|---|
| Encoder | Baseline sequential Huffman; 4:4:4, 4:2:2, 4:2:0; grayscale; restart markers; IJG quality scaling with the Annex K tables |
| Decoder | Baseline, extended sequential and progressive Huffman; any sampling factors (tested: 4:4:4, 4:2:2, 4:2:0, 4:4:0, 4:1:1); interleaved and non-interleaved scans; restart markers; grayscale, YCbCr, Adobe RGB/CMYK/YCCK |
| Not supported | Arithmetic coding, lossless, hierarchical, 12-bit samples, DNL |

Accuracy is tested against libjpeg-turbo:

- The decoder is within 3 levels of `djpeg` (53–68 dB PSNR) on every fixture.
- The encoder is within 0.06 dB of `cjpeg` at the same settings.
- Decoding our own encoder's output reproduces every coefficient and bit span exactly.

## Building

Requires a recent stable Rust (edition 2024). On macOS you also need Xcode, because GPUI compiles
Metal shaders.

```sh
cargo run --release -p visco-app                  # open a file from the app (⌘O or drag and drop)
cargo run --release -p visco-app -- photo.jpg     # or pass one on the command line
```

Opening a JPEG shows the decoding pipeline, and you can also re-encode it. Opening a PNG, BMP or
other image shows the encoding pipeline, and the decode view then decodes visco's own output.

## Layout

```
crates/
  visco-core/   codec-agnostic trace model: stages, planes, block grids, bit spans, parameters
  visco-jpeg/   the JPEG codec and its stage tracing (no GUI dependencies)
  visco-app/    the GPUI application (gpui-kit)
fixtures/jpeg/  test images, the script that makes them, and libjpeg reference decodes
```

A codec implements `visco_core::Codec`: it encodes or decodes into a `Trace` of generic stages, plus
an annotated bitstream and per-block detail. The app draws any `Trace`, so adding a codec does not
require changing the UI.

## Testing

```sh
cargo test --workspace
```

The fixtures and their reference decodes are committed, so the tests need no external tools.
`fixtures/jpeg/make.sh` regenerates them with libjpeg-turbo (`cjpeg`, `djpeg`) and ImageMagick.

## License

[Apache-2.0](LICENSE)
