mod common;
use common::*;
use std::sync::Arc;
use visco_core::{Codec, Media, Params, Pcm, StageView};
use visco_flac::FlacCodec;

fn check(t: &visco_core::Trace, name: &str) {
    let blocks = t.blocks.as_ref().unwrap();
    let n = blocks.block_count();
    assert!(n > 0, "{name}");
    for st in &t.stages {
        if let StageView::Signals(sv) = &st.view {
            for lane in &sv.lanes {
                for s in &lane.series {
                    assert_eq!(s.data.len() as u64, sv.len, "{name}/{}: {}", st.id, s.label);
                }
                if let Some(b) = &lane.blocks {
                    assert!(b.ranges.iter().all(|(r, id)| *id < n && r.end <= sv.len), "{name}/{}", st.id);
                    assert!(b.ranges.windows(2).all(|w| w[0].0.end <= w[1].0.start));
                    if let Some(c) = &b.categories {
                        assert_eq!(c.len(), b.ranges.len());
                        assert!(c.iter().all(|&i| (i as usize) < b.legend.len()));
                    }
                    if let Some(h) = &b.heat {
                        assert_eq!(h.len(), b.ranges.len());
                    }
                }
            }
        }
    }
    for id in [0, n / 2, n - 1] {
        let info = blocks.block_info(id, &t.bitstream);
        assert!(!info.plots.is_empty(), "{name}: block {id}");
        assert!(!info.spans.is_empty(), "{name}: block {id} has no spans");
        for p in &info.plots {
            assert!(p.series.iter().all(|s| !s.data.is_empty()));
        }
    }
    assert!(matches!(t.output, Some(Media::Audio(_))));
}

#[test]
fn decode_traces() {
    for (name, bytes) in fixtures("flac") {
        let t = FlacCodec.decode(Arc::from(bytes)).unwrap_or_else(|e| panic!("{name}: {e}"));
        check(&t, &name);
    }
}

#[test]
fn encode_traces() {
    for (name, bytes) in fixtures("wav") {
        let (rate, bits, ch, samples) = read_wav(&bytes);
        let pcm = Pcm { sample_rate: rate, bits_per_sample: bits, channels: ch, samples: samples.clone().into() };
        let t = FlacCodec.encode(&Media::Audio(pcm), &Params::defaults(&FlacCodec.params_schema())).unwrap();
        check(&t, &name);
        assert!(t.metrics.iter().any(|(k, v)| k == "lossless check" && v.contains('✓')), "{name}");
        // Encoder traces explain their choices.
        let info = t.blocks.as_ref().unwrap().block_info(0, &t.bitstream);
        assert!(info.tables.iter().any(|t| t.title.starts_with("Predictors tried")), "{name}");
    }
}
