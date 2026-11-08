#!/usr/bin/env python3
"""Deterministic WAV sources for the FLAC tests (no external dependencies)."""
import math, struct, wave, random

def write(name, rate, bits, chans, frames):
    with wave.open(name, "wb") as w:
        w.setnchannels(chans)
        w.setsampwidth(bits // 8)
        w.setframerate(rate)
        lo, hi = -(1 << (bits - 1)), (1 << (bits - 1)) - 1
        out = bytearray()
        for fr in frames:
            for s in fr:
                s = max(lo, min(hi, int(s)))
                if bits == 8:
                    out += struct.pack("<B", s + 128)  # WAV 8-bit is unsigned
                elif bits == 16:
                    out += struct.pack("<h", s)
                elif bits == 24:
                    out += struct.pack("<i", s)[:3]
                else:
                    out += struct.pack("<i", s)
        w.writeframes(bytes(out))

rng = random.Random(1234)

def music(rate, secs, amp):
    """A sweep plus harmonics plus a little noise; the right channel is mostly the left."""
    n = int(rate * secs)
    out = []
    phase = 0.0
    for i in range(n):
        t = i / rate
        f = 110 * (2 ** (t * 2.5))
        phase += 2 * math.pi * f / rate
        l = 0.5 * math.sin(phase) + 0.2 * math.sin(3 * phase) + 0.1 * math.sin(7.1 * 2 * math.pi * 440 * t)
        r = 0.85 * l + 0.15 * math.sin(2 * math.pi * 660 * t)
        noise = rng.gauss(0, 0.004)
        out.append((amp * (l + noise), amp * (r + noise * 0.5)))
    return out

write("s16_stereo_44k.wav", 44100, 16, 2, music(44100, 1.5, 26000))
write("s24_stereo_96k.wav", 96000, 24, 2, music(96000, 0.5, 6_000_000))
write("s8_mono_22k.wav", 22050, 8, 1, [(l,) for l, _ in music(22050, 1.0, 100)])
mono = [(l,) for l, _ in music(48000, 1.0, 20000)]
write("s16_mono_48k.wav", 48000, 16, 1, mono)
# Near-silence with a click: constant subframes.
quiet = [(0, 0)] * 30000 + [(20000, -20000)] * 64 + [(0, 0)] * 14036
write("s16_silence_click.wav", 44100, 16, 2, quiet)
# Low 4 bits always zero: wasted bits.
write("s16_wasted_bits.wav", 44100, 16, 2, [(int(l) & ~15, int(r) & ~15) for l, r in music(44100, 0.6, 26000)])
# White noise: verbatim / escape partitions.
write("s16_noise.wav", 44100, 16, 2, [(rng.randint(-30000, 30000), rng.randint(-30000, 30000)) for _ in range(20000)])
