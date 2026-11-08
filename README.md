# visco

A modular codec visualizer written in Rust with [GPUI](https://gpui.rs). Open a file and step through
how it is encoded or decoded, one stage at a time. You can click any block to see its real
intermediate data, and see which bits in the file encode it.

There are two codecs, JPEG and FLAC. Both encoders and both decoders are written from scratch, so
every stage shows data the codec actually produced, not a reconstruction.

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

### FLAC

- **Stage pipeline.**
  - Encoding: source waveform with spectrogram, framing, stereo decorrelation (the winning mode
    per frame), prediction overlaid on the signal, residual with bits per sample, statistics, and
    the decoded result with an MD5 check.
  - Decoding: metadata blocks, frames, coded channels, prediction, residual, output, and
    statistics.
- **Subframe inspector.** Click a subframe to see its signal, prediction and residual, with Rice
  partition boundaries marked. It also shows the quantized LPC coefficients, every Rice partition's
  parameter and cost, the predictors and stereo modes the encoder tried with their bit costs, and
  the exact bits in the hex view.
- **Live parameters.** Block size, maximum LPC order, coefficient precision, Rice partition order,
  stereo mode, LPC window, exhaustive search and escape coding.
- **Playback.** Play the input, the decoded output, or the current stage's signal (for example the
  residual, normalized to its peak) over the visible range. A playhead follows along.
- **Waveform viewer.** A shared time axis that stays fast on long files and shows individual
  samples at high zoom. Zoom with ⌘-scroll or pinch, drag to pan, and ⌥-scroll for vertical gain.

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

## FLAC support

| | Supported |
|---|---|
| Encoder | Constant, verbatim, fixed (orders 0–4) and LPC (orders 1–32) subframes; windowed autocorrelation with Levinson-Durbin, including libFLAC's tukey, partial/punchout tukey and `subdivide_tukey` windows; automatic or fixed coefficient precision; estimated or exhaustive order search; Rice partition orders 0–8 with RICE/RICE2; optional escape partitions; wasted-bits detection; exhaustive L/R, L/S, S/R and M/S stereo; 4–32-bit samples, 1–8 channels; presets matching `flac -0` … `-8` |
| Decoder | The full format (RFC 9639): every subframe type, wasted bits, RICE/RICE2 and escape partitions, every channel assignment, fixed and variable block sizes, 4–32-bit samples, all metadata block types (parsed or summarized), ID3v2 prefixes, CRC-8/CRC-16 and MD5 verification, and resynchronization after corrupt frames |
| Not supported | Ogg FLAC |

Lossless means the tests are bit-exact:

- The decoder reproduces every fixture exactly. Its output matches claxon and the source WAV, and
  its MD5 matches STREAMINFO. The fixtures come from libFLAC 1.5 at several settings and from
  ffmpeg's encoder.
- The encoder's output passes `flac -t` and decodes exactly with both our decoder and claxon.
- At the same preset, the encoder's output is within 0.32% of libFLAC's size, and often
  byte-for-byte the same.

## Building

Requires a recent stable Rust (edition 2024). On macOS you also need Xcode, because GPUI compiles
Metal shaders.

```sh
cargo run --release -p visco-app                  # open a file from the app (⌘O or drag and drop)
cargo run --release -p visco-app -- photo.jpg     # or pass one on the command line
```

The codec is picked from the file:

- **JPEG or FLAC:** opens the decoding pipeline. You can also re-encode the file.
- **PNG, BMP or another image:** opens the JPEG encoding pipeline.
- **WAV:** opens the FLAC encoding pipeline.

For images and WAVs, the decode view decodes visco's own output.

## Layout

```
crates/
  visco-core/   codec-agnostic trace model: stages, planes, block grids, bit spans, parameters
  visco-jpeg/   the JPEG codec and its stage tracing (no GUI dependencies)
  visco-flac/   the FLAC codec and its stage tracing (no GUI dependencies)
  visco-app/    the GPUI application (gpui-kit, rodio for playback)
fixtures/jpeg/  test images, the script that makes them, and libjpeg reference decodes
fixtures/flac/  WAV sources (generated by a script) and libFLAC/ffmpeg encodes of them
```

A codec implements `visco_core::Codec`. It encodes or decodes into a `Trace`, which holds generic
stages (image planes with block grids, or signal lanes with block strips), an annotated bitstream
and per-block detail. The app draws any `Trace`, so adding a codec does not require changing the
UI.

## Testing

```sh
cargo test --workspace
```

The fixtures and their reference decodes are committed, so the tests need no external tools.

- `fixtures/jpeg/make.sh` regenerates the JPEG fixtures with libjpeg-turbo (`cjpeg`, `djpeg`) and
  ImageMagick.
- `fixtures/flac/make.sh` regenerates the FLAC fixtures with Python, libFLAC (`flac`) and ffmpeg.

The comparisons against the reference tools are ignored tests. With the tools installed, run them
with:

```sh
cargo test -p visco-jpeg --test compare_cjpeg -- --ignored --nocapture
cargo test -p visco-flac --test encoder compare_with_libflac -- --ignored --nocapture
```

## License

[Apache-2.0](LICENSE)
