#!/bin/sh
# Regenerates the JPEG fixtures from small.ppm / large.ppm with libjpeg-turbo's cjpeg and ImageMagick.
set -e
cd "$(dirname "$0")"
printf '0;\n1;\n2;\n' > /tmp/visco-seq-scans.txt
for src in small large; do
  cjpeg -quality 85 -sample 1x1 -baseline $src.ppm > ${src}_base_444.jpg
  cjpeg -quality 85 -sample 2x1 -baseline $src.ppm > ${src}_base_422.jpg
  cjpeg -quality 85 -sample 2x2 -baseline $src.ppm > ${src}_base_420.jpg
  cjpeg -quality 85 -sample 1x2 -baseline $src.ppm > ${src}_base_440.jpg
  cjpeg -quality 85 -sample 4x1 -baseline $src.ppm > ${src}_base_411.jpg
  cjpeg -quality 85 -grayscale -baseline $src.ppm > ${src}_base_gray.jpg
  cjpeg -quality 60 -sample 2x2 -restart 2B -baseline $src.ppm > ${src}_base_rst.jpg
  cjpeg -quality 85 -optimize -baseline $src.ppm > ${src}_base_opt.jpg
  cjpeg -quality 85 -sample 2x2 -scans /tmp/visco-seq-scans.txt $src.ppm > ${src}_seq_noninterleaved.jpg
  cjpeg -quality 85 -sample 2x2 -progressive $src.ppm > ${src}_prog_420.jpg
  cjpeg -quality 85 -sample 1x1 -progressive $src.ppm > ${src}_prog_444.jpg
  cjpeg -quality 85 -grayscale -progressive $src.ppm > ${src}_prog_gray.jpg
  cjpeg -quality 50 -sample 2x2 -progressive -restart 1 $src.ppm > ${src}_prog_rst.jpg
  magick $src.ppm -quality 80 -interlace JPEG ${src}_prog_magick.jpg
done

# Reference decodes from libjpeg-turbo (islow IDCT, fancy upsampling). zune-jpeg mis-decodes the
# non-interleaved sequential fixture, so libjpeg is the reference.
for f in *.jpg; do
  djpeg -pnm "$f" > "${f%.jpg}.ref.pnm"
done
