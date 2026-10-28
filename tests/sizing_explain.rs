//! The probe's answer is `tools/bench/sizing.py`'s `explain()`, to the digit. The goldens in
//! `tests/probe/explain-*.json` are what `sizing.explain()` answers for a platform as `bench.py probe` builds
//! it; regenerate them after a change to `sizing.py` with:
//!
//! ```sh
//! python3 - <<'PY'
//! import sys, json; sys.path.insert(0, "tools/bench"); import sizing
//! for name, cpus, s1, c, d in [("4cpu", 4, 2.542, 1.183, None), ("2cpu-disk", 2, 1.53, 1.1, 219.3),
//!                              ("1cpu-slow-disk", 1, 3.0, 1.0, 40.0), ("8cpu", 8, 2.0, 1.25, 120.0),
//!                              ("2cpu-past-target", 2, 9.0, 1.2, None)]:
//!     k = sizing.load(None, s1=s1, contention=c, disk_mbps=d)
//!     k["hw"].update(platform="golden", measured="sizing.explain", cpus=cpus)
//!     with open(f"tests/probe/explain-{name}.json", "w") as f:
//!         json.dump(sizing.explain(k, cpus), f, indent=1, ensure_ascii=False); f.write("\n")
//! PY
//! ```
//!
//! The embedded workload ([`REPORT`]) is checked against `tools/bench/coefficients/amd64-report.json` the way
//! `sizing.py` derives it, so a new coefficients file cannot leave the service's constants behind unnoticed.

use std::path::Path;

use docx_to_pdf::num::ToF64;
use docx_to_pdf::sizing::{HARD_MEMORY_MIB, Platform, REPORT, explain, power_of_two};
use serde_json::Value;

fn read(path: impl AsRef<Path>) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

/// Equal, numbers within a relative 1e-12 (JSON integers and floats alike), everything else exactly.
fn assert_same(ours: &Value, theirs: &Value, at: &str) {
    match (ours, theirs) {
        (Value::Number(a), Value::Number(b)) => {
            let (a, b) = (a.as_f64().unwrap(), b.as_f64().unwrap());
            assert!(
                (a - b).abs() <= 1e-12 * a.abs().max(b.abs()),
                "{at}: {a} != {b}"
            );
        }
        (Value::Object(a), Value::Object(b)) => {
            let mut keys: Vec<_> = a.keys().collect();
            let mut theirs_keys: Vec<_> = b.keys().collect();
            keys.sort();
            theirs_keys.sort();
            assert_eq!(keys, theirs_keys, "{at}: keys");
            for key in a.keys() {
                assert_same(&a[key], &b[key], &format!("{at}.{key}"));
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            assert_eq!(a.len(), b.len(), "{at}: length");
            for (i, (x, y)) in a.iter().zip(b).enumerate() {
                assert_same(x, y, &format!("{at}[{i}]"));
            }
        }
        _ => assert_eq!(ours, theirs, "{at}"),
    }
}

#[test]
fn the_answer_is_sizing_explain_to_the_digit() {
    for (name, cpus, s1, contention, disk) in [
        ("4cpu", 4, 2.542, 1.183, None),
        ("2cpu-disk", 2, 1.53, 1.1, Some(219.3)),
        ("1cpu-slow-disk", 1, 3.0, 1.0, Some(40.0)),
        ("8cpu", 8, 2.0, 1.25, Some(120.0)),
        ("2cpu-past-target", 2, 9.0, 1.2, None),
    ] {
        let platform = Platform {
            name: "golden".into(),
            measured: Some("sizing.explain".into()),
            cpus_probed: Some(cpus),
            s1,
            contention,
            disk_mbps: disk,
        };
        let ours = explain(&REPORT, &platform, cpus, HARD_MEMORY_MIB);
        let golden = read(format!("tests/probe/explain-{name}.json"));
        assert_same(&ours, &golden, name);
    }
}

#[test]
#[allow(
    clippy::many_single_char_names,
    reason = "the formula's own names (x, y, w, h; r, g, b; the DCT's indices)"
)]
fn the_embedded_workload_is_the_reference_coefficients() {
    let k = read("tools/bench/coefficients/amd64-report.json");
    let w = &k["workload"];
    assert_eq!(w["name"], REPORT.name);
    let documents: Vec<&str> = w["documents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d.as_str().unwrap())
        .collect();
    assert_eq!(documents, REPORT.documents);
    let number = |v: &Value| v.as_f64().unwrap();
    assert_eq!(
        number(&w["largest_document_mib"]),
        REPORT.largest_document_mib
    );
    assert_eq!(number(&w["largest_pdf_mib"]), REPORT.largest_pdf_mib);
    assert_eq!(
        number(&w["max_document_mib"]),
        REPORT.max_document_mib.to_f64()
    );
    let m = &k["memory"];
    assert_eq!(
        power_of_two(number(&m["service_mib"])),
        REPORT.memory_service_mib
    );
    assert_eq!(
        power_of_two(number(&m["per_instance_mib"])),
        REPORT.memory_per_instance_mib
    );
    assert_eq!(number(&m["headroom"]), REPORT.memory_headroom);
    assert_eq!(
        number(&m["instance_max_mib"]),
        REPORT.instance_max_mib.to_f64()
    );
    let d = &k["disk"];
    // serde_json parses a float to within an ulp or two (no `float_roundtrip`).
    let bytes = number(&d["bytes_per_conversion"]);
    assert!((bytes - REPORT.bytes_per_conversion).abs() <= 1e-12 * bytes);
    assert_eq!(number(&d["utilisation"]), REPORT.disk_share);
    let t = &k["service_time"];
    let shape: Vec<f64> = t["shape"].as_array().unwrap().iter().map(number).collect();
    assert_eq!(shape, REPORT.shape);
    assert_eq!(number(&t["cv2"]), REPORT.cv2);
}
