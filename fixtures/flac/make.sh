#!/bin/sh
# Regenerates the FLAC fixtures: WAV sources (make_sources.py), then libFLAC 1.5 and ffmpeg encodes.
set -e
cd "$(dirname "$0")"
python3 make_sources.py
for src in s16_stereo_44k s24_stereo_96k s8_mono_22k s16_mono_48k s16_silence_click s16_wasted_bits s16_noise; do
  for lvl in 0 5 8; do
    flac -s -f --no-seektable -$lvl -o ${src}_flac$lvl.flac $src.wav
  done
done
# High-order LPC, RICE2-friendly 24-bit, exhaustive search outside the subset.
flac -s -f --lax -8 -e -l 32 -r 15 -o s24_stereo_96k_lax32.flac s24_stereo_96k.wav
# Metadata variety: seektable, Vorbis comments, padding.
flac -s -f -5 -S 10x -P 1024 -T "TITLE=visco test" -T "ARTIST=visco" -o s16_stereo_44k_metadata.flac s16_stereo_44k.wav
# Small fixed block size and a different encoder implementation (ffmpeg).
flac -s -f -5 -b 576 -o s16_stereo_44k_b576.flac s16_stereo_44k.wav
ffmpeg -loglevel error -y -i s16_stereo_44k.wav -c:a flac -compression_level 12 s16_stereo_44k_ffmpeg12.flac
ffmpeg -loglevel error -y -i s24_stereo_96k.wav -c:a flac -compression_level 8 s24_stereo_96k_ffmpeg8.flac
