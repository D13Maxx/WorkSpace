//! GGUF validation against corrupt and truncated files (master spec s13 C3).
//!
//! The requirement is "errors, zero panics". A model file is something the
//! user downloaded, and the realistic hostile input is not a crafted attack
//! but an interrupted download — so these are mostly truncations, plus a few
//! deliberately implausible headers.
//!
//! Files are generated rather than committed: a corpus of binaries in the
//! repository is opaque, and this way the shapes being tested are readable.

// Spec s14 permits unwrap/expect in tests.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use candle_core::quantized::{QTensor, gguf_file};
use candle_core::{Device, Tensor};
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use ws_engine::{EngineError, gguf};

/// Write a tiny but structurally valid GGUF: a llama-flavoured header and one
/// small tensor. Everything else in this file is a corruption of this.
fn write_valid(path: &Path, architecture: &str) {
    let device = Device::Cpu;
    // Q8_0 works on a 32-element block boundary.
    let data = Tensor::from_vec(vec![0.25f32; 64], (2, 32), &device).unwrap();
    let q = QTensor::quantize(&data, candle_core::quantized::GgmlDType::Q8_0).unwrap();

    let arch = gguf_file::Value::String(architecture.to_owned());
    let block_count = gguf_file::Value::U32(2);
    let embedding = gguf_file::Value::U32(64);
    let heads = gguf_file::Value::U32(4);
    let kv_heads = gguf_file::Value::U32(2);
    let ctx = gguf_file::Value::U32(2048);

    let key = |s: &str| format!("{architecture}.{s}");
    let metadata: Vec<(&str, &gguf_file::Value)> = vec![
        ("general.architecture", &arch),
        (Box::leak(key("block_count").into_boxed_str()), &block_count),
        (
            Box::leak(key("embedding_length").into_boxed_str()),
            &embedding,
        ),
        (
            Box::leak(key("attention.head_count").into_boxed_str()),
            &heads,
        ),
        (
            Box::leak(key("attention.head_count_kv").into_boxed_str()),
            &kv_heads,
        ),
        (Box::leak(key("context_length").into_boxed_str()), &ctx),
    ];

    let mut file = File::create(path).unwrap();
    gguf_file::write(&mut file, &metadata, &[("token_embd.weight", &q)]).unwrap();
}

struct Fixtures {
    dir: tempfile::TempDir,
}

impl Fixtures {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn valid(&self, name: &str) -> PathBuf {
        let p = self.path(name);
        write_valid(&p, "llama");
        p
    }

    /// A copy of a valid file, cut to `fraction` of its length.
    fn truncated(&self, name: &str, fraction: f64) -> PathBuf {
        let src = self.valid(&format!("{name}-src"));
        let bytes = std::fs::read(&src).unwrap();
        let keep = ((bytes.len() as f64) * fraction) as usize;
        let p = self.path(name);
        File::create(&p).unwrap().write_all(&bytes[..keep]).unwrap();
        p
    }

    /// A copy of a valid file with `len` bytes overwritten at `offset`.
    fn corrupted(&self, name: &str, offset: usize, len: usize, byte: u8) -> PathBuf {
        let src = self.valid(&format!("{name}-src"));
        let mut bytes = std::fs::read(&src).unwrap();
        for i in offset..(offset + len).min(bytes.len()) {
            bytes[i] = byte;
        }
        let p = self.path(name);
        File::create(&p).unwrap().write_all(&bytes).unwrap();
        p
    }

    fn raw(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let p = self.path(name);
        File::create(&p).unwrap().write_all(bytes).unwrap();
        p
    }
}

#[test]
fn a_valid_file_is_read_and_its_header_reported() {
    let f = Fixtures::new();
    let info = gguf::inspect(f.valid("good.gguf")).unwrap();

    assert_eq!(info.architecture, "llama");
    assert_eq!(info.tensor_count, 1);
    assert_eq!(info.block_count, Some(2));
    assert_eq!(info.embedding_length, Some(64));
    assert_eq!(info.head_count, Some(4));
    assert_eq!(info.head_count_kv, Some(2));
    assert_eq!(info.context_length, Some(2048));
    assert_eq!(info.head_dim(), Some(16));
    assert!(
        info.tensor_bytes > 0,
        "tensor size is needed for the memory check"
    );
    assert!(
        info.quant_summary.contains("Q8"),
        "got {}",
        info.quant_summary
    );
    assert!(!info.has_chat_template, "this fixture embeds none");
}

#[test]
fn a_truncated_file_never_panics() {
    // The realistic case: an interrupted download. Not every cut point has to
    // fail -- GGUF pads its tensor data, so clipping a few trailing bytes can
    // leave every tensor still in bounds and the file genuinely readable. What
    // must hold at every cut point is that nothing panics.
    let f = Fixtures::new();
    for (i, fraction) in [0.0, 0.05, 0.25, 0.5, 0.6, 0.75, 0.9, 0.99]
        .iter()
        .enumerate()
    {
        let path = f.truncated(&format!("cut{i}.gguf"), *fraction);
        let _ = gguf::inspect(&path);
    }
}

#[test]
fn a_truncation_that_removes_tensor_data_is_caught() {
    // Cut past the padding, into bytes a tensor actually claims.
    let f = Fixtures::new();
    let src = f.valid("short-src.gguf");
    let bytes = std::fs::read(&src).unwrap();
    let info = gguf::inspect(&src).unwrap();

    let keep = bytes
        .len()
        .saturating_sub(usize::try_from(info.tensor_bytes).unwrap() + 4);
    assert!(keep > 0, "the fixture must be larger than its tensor data");
    let path = f.raw("short.gguf", &bytes[..keep]);

    let err = gguf::inspect(&path).unwrap_err();
    assert!(
        matches!(
            err,
            EngineError::TensorOutOfBounds { .. } | EngineError::NotGguf { .. }
        ),
        "expected a bounds or header error, got {err:?}"
    );
}

#[test]
fn a_file_that_is_not_gguf_at_all_is_rejected() {
    let f = Fixtures::new();
    for (name, bytes) in [
        ("empty.gguf", &b""[..]),
        ("text.gguf", &b"this is not a model, it is a note"[..]),
        (
            "wrong-magic.gguf",
            &b"XXXX\x02\x00\x00\x00padding-padding"[..],
        ),
        ("zeros.gguf", &[0u8; 512][..]),
        ("ones.gguf", &[0xFFu8; 512][..]),
    ] {
        let path = f.raw(name, bytes);
        let err = gguf::inspect(&path).unwrap_err();
        assert!(
            matches!(err, EngineError::NotGguf { .. } | EngineError::Io { .. }),
            "{name} produced {err:?}"
        );
    }
}

#[test]
fn a_corrupted_header_errors_rather_than_panicking() {
    // Sweep corruption across the header region. Some of these will happen to
    // land on padding and still parse; what matters is that none panics.
    let f = Fixtures::new();
    for offset in (0..200).step_by(8) {
        for byte in [0x00u8, 0xFF, 0xAA] {
            let path = f.corrupted(&format!("c{offset}-{byte}.gguf"), offset, 8, byte);
            // The assertion is the absence of a panic; either outcome is fine.
            let _ = gguf::inspect(&path);
        }
    }
}

#[test]
fn a_tensor_table_describing_absent_data_is_rejected_by_the_bounds_check() {
    // Drives the bounds arithmetic directly: keep the whole header, remove
    // enough of the payload that the tensor table describes data that is not
    // there. This is the check that stops a crafted shape becoming an
    // allocation (master spec s8.3).
    let f = Fixtures::new();
    let src = f.valid("table-src.gguf");
    let bytes = std::fs::read(&src).unwrap();
    let info = gguf::inspect(&src).unwrap();

    let keep = bytes
        .len()
        .saturating_sub(usize::try_from(info.tensor_bytes).unwrap() / 2);
    let path = f.raw("table.gguf", &bytes[..keep]);

    match gguf::inspect(&path) {
        Err(EngineError::TensorOutOfBounds {
            start,
            end,
            file_size,
            ..
        }) => {
            assert!(
                end > file_size,
                "the error should say why: {start}..{end} of {file_size}"
            );
        }
        Err(other) => panic!("expected a bounds error, got {other:?}"),
        Ok(info) => panic!("a file missing tensor data was accepted: {info:?}"),
    }
}

#[test]
fn a_missing_file_is_an_io_error_with_the_path_in_it() {
    let f = Fixtures::new();
    let err = gguf::inspect(f.path("does-not-exist.gguf")).unwrap_err();
    assert!(matches!(err, EngineError::Io { .. }));
    assert!(
        err.to_string().contains("does-not-exist"),
        "the message should name the file: {err}"
    );
}

#[test]
fn an_unsupported_architecture_is_named_in_the_error() {
    // Master spec s8.3: M1 supports llama only, and says so rather than
    // failing obscurely later.
    let f = Fixtures::new();
    let p = f.path("mamba.gguf");
    write_valid(&p, "mamba");

    let info = gguf::inspect(&p).unwrap();
    assert_eq!(info.architecture, "mamba");

    let err = gguf::require_supported(&info).unwrap_err();
    assert!(matches!(err, EngineError::UnsupportedArchitecture(..)));
    assert!(
        err.to_string().contains("mamba"),
        "the message should name what was found: {err}"
    );
}

#[test]
fn a_supported_architecture_passes_the_gate() {
    let f = Fixtures::new();
    let info = gguf::inspect(f.valid("llama.gguf")).unwrap();
    gguf::require_supported(&info).unwrap();
}

#[test]
fn hashing_a_file_is_stable_and_content_addressed() {
    let f = Fixtures::new();
    let a = f.valid("a.gguf");
    let b = f.valid("b.gguf");

    let ha = gguf::file_sha256(&a).unwrap();
    assert_eq!(ha.len(), 64, "hex-encoded SHA-256");
    assert_eq!(ha, gguf::file_sha256(&a).unwrap(), "stable across reads");
    assert_eq!(
        ha,
        gguf::file_sha256(&b).unwrap(),
        "identical content hashes identically"
    );

    let different = f.raw("different.gguf", b"other bytes entirely");
    assert_ne!(ha, gguf::file_sha256(&different).unwrap());
}

// ---- RoPE scaling detection (master spec s8.4) -------------------------
//
// The forward pass in this build applies no frequency scaling. A file that
// needs it still loads and still produces fluent text on short prompts, then
// degrades as the context grows -- the exact failure s8.4 calls out as
// invisible. So it has to be detected from the file rather than discovered
// from the output.

/// Llama 3.1 and 3.2: llama.cpp precomputes the factors into a tensor.
#[test]
fn a_file_carrying_rope_frequency_factors_is_detected() {
    let f = Fixtures::new();
    let device = Device::Cpu;
    let data = Tensor::from_vec(vec![0.25f32; 32], (1, 32), &device).unwrap();
    let weights = QTensor::quantize(&data, candle_core::quantized::GgmlDType::Q8_0).unwrap();
    // The converter writes these as F32.
    let factors = Tensor::from_vec(vec![1.0f32; 32], (32,), &device).unwrap();
    let freqs = QTensor::quantize(&factors, candle_core::quantized::GgmlDType::F32).unwrap();

    let arch = gguf_file::Value::String("llama".to_owned());
    let metadata: Vec<(&str, &gguf_file::Value)> = vec![("general.architecture", &arch)];

    let path = f.path("llama32-ish.gguf");
    let mut file = File::create(&path).unwrap();
    gguf_file::write(
        &mut file,
        &metadata,
        &[
            ("token_embd.weight", &weights),
            ("rope_freqs.weight", &freqs),
        ],
    )
    .unwrap();
    drop(file);

    let info = gguf::inspect(&path).unwrap();
    assert_eq!(
        info.rope_scaling,
        Some(gguf::RopeScaling::FrequencyFactors),
        "a rope_freqs.weight tensor means the model expects scaling"
    );
    assert!(
        info.rope_scaling.unwrap().to_string().contains("Llama 3"),
        "the message should name what this is, for someone who has not read s8.4"
    );
}

/// Linear and YARN scaling are declared in metadata instead.
#[test]
fn a_declared_scaling_type_is_detected_with_its_factor() {
    let f = Fixtures::new();
    let device = Device::Cpu;
    let data = Tensor::from_vec(vec![0.25f32; 32], (1, 32), &device).unwrap();
    let q = QTensor::quantize(&data, candle_core::quantized::GgmlDType::Q8_0).unwrap();

    let arch = gguf_file::Value::String("llama".to_owned());
    let kind = gguf_file::Value::String("yarn".to_owned());
    let factor = gguf_file::Value::F32(4.0);
    let metadata: Vec<(&str, &gguf_file::Value)> = vec![
        ("general.architecture", &arch),
        ("llama.rope.scaling.type", &kind),
        ("llama.rope.scaling.factor", &factor),
    ];

    let path = f.path("yarn.gguf");
    let mut file = File::create(&path).unwrap();
    gguf_file::write(&mut file, &metadata, &[("token_embd.weight", &q)]).unwrap();
    drop(file);

    let info = gguf::inspect(&path).unwrap();
    let Some(gguf::RopeScaling::Declared { kind, factor }) = info.rope_scaling else {
        panic!(
            "expected a declared scaling type, got {:?}",
            info.rope_scaling
        );
    };
    assert_eq!(kind, "yarn");
    assert_eq!(factor, Some(4.0), "the factor says how wrong it would be");
}

#[test]
fn a_scaling_type_of_none_is_not_reported_as_scaling() {
    // "none" is the honest absence of scaling. Reporting it would put a
    // warning on every well-behaved model and train the user to ignore it.
    let f = Fixtures::new();
    let device = Device::Cpu;
    let data = Tensor::from_vec(vec![0.25f32; 32], (1, 32), &device).unwrap();
    let q = QTensor::quantize(&data, candle_core::quantized::GgmlDType::Q8_0).unwrap();

    let arch = gguf_file::Value::String("llama".to_owned());
    for declared in ["none", "", "NONE"] {
        let kind = gguf_file::Value::String(declared.to_owned());
        let metadata: Vec<(&str, &gguf_file::Value)> = vec![
            ("general.architecture", &arch),
            ("llama.rope.scaling.type", &kind),
        ];
        let path = f.path(&format!("none-{}.gguf", declared.len()));
        let mut file = File::create(&path).unwrap();
        gguf_file::write(&mut file, &metadata, &[("token_embd.weight", &q)]).unwrap();
        drop(file);

        assert_eq!(
            gguf::inspect(&path).unwrap().rope_scaling,
            None,
            "{declared:?} must not raise a warning"
        );
    }
}

#[test]
fn an_ordinary_model_reports_no_rope_scaling() {
    let f = Fixtures::new();
    let info = gguf::inspect(f.valid("plain.gguf")).unwrap();
    assert_eq!(info.rope_scaling, None);
}
