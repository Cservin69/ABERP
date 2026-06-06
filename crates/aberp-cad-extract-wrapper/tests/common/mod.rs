//! Shared test helpers.
//!
//! Each `tests/*.rs` is its own compilation unit, so this module
//! gets `#[allow(dead_code)]` — helpers used only by one of the
//! files would otherwise warn from the unused-import lint in the
//! others.

#![allow(dead_code)]

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Path to the Python interpreter the test suite uses. Pulled from
/// `ABERP_TEST_PYTHON` if set, else `python3` on PATH.
///
/// The CI lane sets the env var to point at the venv created by
/// `pip install -e python/aberp-cad-extract`. Locally a developer can
/// either source the venv (`source .venv-cad-extract/bin/activate`) so
/// `python3` resolves there, or export `ABERP_TEST_PYTHON=<path>`.
pub fn test_python_bin() -> PathBuf {
    if let Ok(p) = std::env::var("ABERP_TEST_PYTHON") {
        return PathBuf::from(p);
    }
    PathBuf::from("python3")
}

/// Write a 20 mm cube as a binary STL to `path`. Matches the
/// fixture geometry exercised by the Python-side CLI test
/// (`test_cli_emits_valid_feature_graph_json`) — 20×20×20 axis-
/// aligned cube centered on the origin, so the wrapper's smoke
/// test asserts the same bounding box [20, 20, 20].
///
/// Binary STL layout (Wikipedia: STL format):
///   80-byte header (any content — convention is "" padded with NUL)
///   uint32 little-endian triangle count
///   per triangle:
///     3 × float32 LE  normal (x,y,z) — we leave (0,0,0); STL viewers
///                     don't require valid normals for solid models
///     9 × float32 LE  three vertices (x,y,z each)
///     uint16 LE       attribute byte count (0)
pub fn write_cube_stl(path: &Path, side_mm: f32) -> std::io::Result<()> {
    let h = side_mm / 2.0;
    // Eight cube corners.
    let v = [
        [-h, -h, -h],
        [h, -h, -h],
        [h, h, -h],
        [-h, h, -h],
        [-h, -h, h],
        [h, -h, h],
        [h, h, h],
        [-h, h, h],
    ];
    // 12 triangles (2 per face). Winding doesn't affect the
    // signed-tetrahedra volume's absolute value (the extractor
    // takes `abs`), so we don't bother enforcing outward normals.
    let tris: [[usize; 3]; 12] = [
        [0, 3, 1],
        [1, 3, 2], // bottom (-z)
        [4, 5, 7],
        [5, 6, 7], // top (+z)
        [0, 1, 5],
        [0, 5, 4], // front (-y)
        [2, 3, 7],
        [2, 7, 6], // back (+y)
        [1, 2, 6],
        [1, 6, 5], // right (+x)
        [0, 4, 7],
        [0, 7, 3], // left (-x)
    ];

    let mut f = File::create(path)?;
    f.write_all(&[0u8; 80])?;
    f.write_all(&(tris.len() as u32).to_le_bytes())?;
    for t in tris.iter() {
        // zero normal
        for _ in 0..3 {
            f.write_all(&0f32.to_le_bytes())?;
        }
        for vi in t {
            for coord in &v[*vi] {
                f.write_all(&coord.to_le_bytes())?;
            }
        }
        f.write_all(&0u16.to_le_bytes())?;
    }
    Ok(())
}
