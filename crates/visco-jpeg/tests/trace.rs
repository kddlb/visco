mod common;
use common::*;
use std::sync::Arc;
use visco_core::{Codec, Params, RgbImage, StageView};
use visco_jpeg::JpegCodec;

fn check(trace: &visco_core::Trace, name: &str) {
    let blocks = trace.blocks.as_ref().unwrap();
    let n = blocks.block_count();
    assert!(n > 0);
    for st in &trace.stages {
        if let StageView::Planes(pvs) = &st.view {
            for pv in pvs {
                let len = match &pv.plane.kind {
                    visco_core::PlaneKind::Gray(d) => d.len(),
                    visco_core::PlaneKind::Rgb(d) => d.len() / 3,
                    visco_core::PlaneKind::Scalar { data, .. } => data.len(),
                };
                assert_eq!(len, pv.plane.width * pv.plane.height, "{name}/{}", st.id);
                if let Some(g) = &pv.grid {
                    assert_eq!(g.cells.len(), g.cols * g.rows);
                    assert!(g.cells.iter().all(|&c| c < n), "{name}/{}: bad cell", st.id);
                    assert!(g.cols * g.block_w >= pv.plane.width, "{name}/{}", st.id);
                }
            }
        }
    }
    for id in [0, n / 2, n - 1] {
        let info = blocks.block_info(id, &trace.bitstream);
        assert!(!info.grids.is_empty());
        assert!(info.grids.iter().all(|g| g.values.len() == 64));
        for &s in &info.spans {
            assert_eq!(trace.bitstream.spans[s].block as usize, id);
        }
    }
}

#[test]
fn decode_traces_for_all_fixtures() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/jpeg");
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "jpg") {
            let bytes: Arc<[u8]> = std::fs::read(&p).unwrap().into();
            let t = JpegCodec.decode(bytes).unwrap();
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            assert_eq!(t.stages.iter().any(|s| s.id == "scans"), name.contains("prog"), "{name}");
            check(&t, &name);
        }
    }
}

#[test]
fn encode_traces() {
    let (w, h) = (50, 30);
    let img = RgbImage { width: w, height: h, data: synth(w, h).into() };
    let schema = JpegCodec.params_schema();
    for gray in [false, true] {
        for sub in 0..3 {
            let mut p = Params::defaults(&schema);
            p.set("grayscale", visco_core::ParamValue::Bool(gray));
            p.set("subsampling", visco_core::ParamValue::Choice(sub));
            let t = JpegCodec.encode(&img, &p).unwrap();
            check(&t, "encode");
            assert!(t.metrics.iter().any(|(k, _)| k == "PSNR"));
        }
    }
}
