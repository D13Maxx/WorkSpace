//! The memory pre-check (master spec s8.10).
//!
//! This is mandatory rather than defensive. A Rust allocation failure aborts
//! the process: there is no `Err` to handle and no panic to catch, so the only
//! place to stop an oversized load is *before* asking for the memory. The
//! whole app disappearing is not an acceptable outcome for choosing a model
//! that is too big for the machine.
//!
//! Every number here is reported back to the user, because "not enough memory"
//! on its own leaves them with nothing to change. The error names the gap and
//! the context cap that would close it.

use crate::error::{EngineError, Result};
use crate::gguf::GgufInfo;
use serde::{Deserialize, Serialize};

/// Headroom over the computed total, for activations, the tokenizer, the
/// sampler's working buffers and allocator slack (master spec s8.10).
const OVERHEAD_PERCENT: u64 = 10;

const MIB: u64 = 1024 * 1024;

/// Element type of the KV cache.
///
/// F16 halves the cache at a precision cost the parity suite is sensitive to,
/// which is why F32 exists as an option rather than only as a default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KvDType {
    #[default]
    F16,
    F32,
}

impl KvDType {
    #[must_use]
    pub fn bytes_per_element(self) -> u64 {
        match self {
            Self::F16 => 2,
            Self::F32 => 4,
        }
    }
}

/// What a load would cost, broken down so the user can see where it goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryEstimate {
    /// The quantized weights, read straight from the file's tensor table.
    pub weights_bytes: u64,
    /// The KV cache at the requested context cap.
    pub kv_cache_bytes: u64,
    pub overhead_bytes: u64,
    pub total_bytes: u64,
}

impl MemoryEstimate {
    #[must_use]
    pub fn total_mib(&self) -> u64 {
        self.total_bytes / MIB
    }
}

/// Bytes the KV cache occupies at a given context length.
///
/// `2 x n_layers x n_kv_heads x head_dim x context x bytes_per_element`: one
/// tensor each for keys and values, per layer, sized to the cap up front so a
/// long conversation cannot fail an allocation halfway through.
///
/// Saturating throughout: the inputs come from a file that may be lying, and
/// an overflow here would wrap into a small number that then passes the check.
#[must_use]
pub fn kv_cache_bytes(info: &GgufInfo, context_cap: u32, kv_dtype: KvDType) -> u64 {
    let layers = u64::from(info.block_count.unwrap_or(0));
    // Absent `head_count_kv` means no grouped-query attention, so KV heads
    // equal query heads.
    let kv_heads = u64::from(info.head_count_kv.or(info.head_count).unwrap_or(0));
    let head_dim = u64::from(info.head_dim().unwrap_or(0));

    2u64.saturating_mul(layers)
        .saturating_mul(kv_heads)
        .saturating_mul(head_dim)
        .saturating_mul(u64::from(context_cap))
        .saturating_mul(kv_dtype.bytes_per_element())
}

/// What loading this model at this context cap would need.
#[must_use]
pub fn estimate(info: &GgufInfo, context_cap: u32, kv_dtype: KvDType) -> MemoryEstimate {
    let weights_bytes = info.tensor_bytes;
    let kv_cache_bytes = kv_cache_bytes(info, context_cap, kv_dtype);
    let subtotal = weights_bytes.saturating_add(kv_cache_bytes);
    let overhead_bytes = subtotal / 100 * OVERHEAD_PERCENT;

    MemoryEstimate {
        weights_bytes,
        kv_cache_bytes,
        overhead_bytes,
        total_bytes: subtotal.saturating_add(overhead_bytes),
    }
}

/// Physical memory currently available, in bytes.
#[must_use]
pub fn available_memory() -> u64 {
    use sysinfo::{MemoryRefreshKind, System};
    let mut system = System::new();
    system.refresh_memory_specifics(MemoryRefreshKind::nothing().with_ram());
    system.available_memory()
}

/// Default worker threads: physical cores minus one, so the UI thread still
/// gets scheduled while a generation is running (master spec s8.10).
#[must_use]
pub fn default_threads() -> usize {
    sysinfo::System::physical_core_count()
        .or_else(|| std::thread::available_parallelism().ok().map(Into::into))
        .unwrap_or(1)
        .saturating_sub(1)
        .max(1)
}

/// Refuse a load that would not fit, with the numbers and a way out.
///
/// # Errors
/// Returns [`EngineError::NotEnoughMemory`] when the estimate exceeds what is
/// available, naming a context cap that would fit.
pub fn check(info: &GgufInfo, context_cap: u32, kv_dtype: KvDType) -> Result<MemoryEstimate> {
    let estimate = estimate(info, context_cap, kv_dtype);
    let available = available_memory();

    if estimate.total_bytes <= available {
        return Ok(estimate);
    }

    Err(EngineError::NotEnoughMemory {
        model: info.path.display().to_string(),
        needed_mib: estimate.total_mib(),
        available_mib: available / MIB,
        context_cap,
        suggested_cap: largest_fitting_cap(info, kv_dtype, available),
    })
}

/// The largest context cap that would fit in `available`, rounded down to a
/// round number, or `None` when the weights alone do not fit.
///
/// Without this the error tells the user to lower the cap but not to what, and
/// they are left bisecting by hand.
fn largest_fitting_cap(info: &GgufInfo, kv_dtype: KvDType, available: u64) -> Option<u32> {
    // Candidates rather than solving for it: these are the values the UI
    // offers, so suggesting anything else would not be selectable.
    [32_768u32, 16_384, 8_192, 4_096, 2_048, 1_024, 512]
        .into_iter()
        .find(|cap| estimate(info, *cap, kv_dtype).total_bytes <= available)
}

#[cfg(test)]
mod tests {
    // Spec s14 permits unwrap/expect in tests.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::path::PathBuf;

    /// Llama-3.2-1B-like dimensions, the worked example in master spec s8.5.
    fn info() -> GgufInfo {
        GgufInfo {
            path: PathBuf::from("model.gguf"),
            file_size: 1_400_000_000,
            architecture: "llama".to_owned(),
            tensor_count: 147,
            quant_summary: "Q4K x 100".to_owned(),
            context_length: Some(131_072),
            block_count: Some(16),
            embedding_length: Some(2_048),
            head_count: Some(32),
            head_count_kv: Some(8),
            rope_freq_base: Some(500_000.0),
            rms_norm_eps: Some(1e-5),
            has_chat_template: true,
            rope_scaling: None,
            tensor_bytes: 1_300_000_000,
        }
    }

    #[test]
    fn the_kv_cache_matches_the_specs_worked_example() {
        // 16 layers, 8 KV heads, head dim 64, 8192 tokens, F32 -> about 512 MiB.
        let bytes = kv_cache_bytes(&info(), 8_192, KvDType::F32);
        assert_eq!(bytes / MIB, 512, "got {bytes} bytes");
    }

    #[test]
    fn f16_halves_the_cache() {
        let f32_bytes = kv_cache_bytes(&info(), 8_192, KvDType::F32);
        let f16_bytes = kv_cache_bytes(&info(), 8_192, KvDType::F16);
        assert_eq!(f16_bytes * 2, f32_bytes);
    }

    #[test]
    fn the_cache_grows_linearly_with_the_context_cap() {
        let at_4k = kv_cache_bytes(&info(), 4_096, KvDType::F16);
        let at_8k = kv_cache_bytes(&info(), 8_192, KvDType::F16);
        assert_eq!(at_4k * 2, at_8k, "this is what makes lowering the cap work");
    }

    #[test]
    fn grouped_query_attention_is_sized_by_kv_heads_not_query_heads() {
        // 8 KV heads against 32 query heads: getting this wrong overestimates
        // the cache fourfold and refuses loads that would have fitted.
        let mut no_gqa = info();
        no_gqa.head_count_kv = Some(32);
        assert_eq!(
            kv_cache_bytes(&no_gqa, 8_192, KvDType::F16),
            kv_cache_bytes(&info(), 8_192, KvDType::F16) * 4
        );
    }

    #[test]
    fn a_file_without_head_counts_reports_no_cache_rather_than_overflowing() {
        let mut bare = info();
        bare.block_count = None;
        bare.head_count = None;
        bare.head_count_kv = None;
        assert_eq!(kv_cache_bytes(&bare, 8_192, KvDType::F16), 0);
    }

    #[test]
    fn implausible_dimensions_saturate_instead_of_wrapping() {
        // A hostile file could otherwise wrap this product into a small number
        // that passes the check, and the allocation that follows aborts.
        let mut huge = info();
        huge.block_count = Some(u32::MAX);
        huge.head_count = Some(u32::MAX);
        huge.head_count_kv = Some(u32::MAX);
        huge.embedding_length = Some(u32::MAX);
        assert_eq!(
            kv_cache_bytes(&huge, u32::MAX, KvDType::F32),
            u64::MAX,
            "must saturate, never wrap"
        );
    }

    #[test]
    fn the_estimate_adds_ten_percent_over_weights_and_cache() {
        let e = estimate(&info(), 8_192, KvDType::F16);
        assert_eq!(e.weights_bytes, 1_300_000_000);
        let subtotal = e.weights_bytes + e.kv_cache_bytes;
        assert!(
            e.overhead_bytes >= subtotal / 11 && e.overhead_bytes <= subtotal / 9,
            "about 10%: {} of {subtotal}",
            e.overhead_bytes
        );
        assert_eq!(e.total_bytes, subtotal + e.overhead_bytes);
    }

    #[test]
    fn a_model_larger_than_memory_is_refused_with_numbers() {
        let mut enormous = info();
        enormous.tensor_bytes = u64::MAX / 2;

        let err = check(&enormous, 8_192, KvDType::F16).unwrap_err();
        let EngineError::NotEnoughMemory {
            needed_mib,
            available_mib,
            ..
        } = &err
        else {
            panic!("expected a memory refusal, got {err:?}");
        };
        assert!(needed_mib > available_mib);
        // The message has to be actionable on its own.
        let msg = err.to_string();
        assert!(msg.contains("MiB"), "{msg}");
        assert!(msg.contains("context"), "{msg}");
    }

    #[test]
    fn the_refusal_suggests_a_cap_that_would_actually_fit() {
        // Weights that fit, but a cache at 32k that does not.
        let available = available_memory();
        let mut big = info();
        big.tensor_bytes = available / 2;
        big.block_count = Some(80);
        big.head_count = Some(64);
        big.head_count_kv = Some(64);
        big.embedding_length = Some(8_192);

        match check(&big, 32_768, KvDType::F32) {
            Err(EngineError::NotEnoughMemory {
                suggested_cap: Some(cap),
                ..
            }) => {
                assert!(cap < 32_768, "the suggestion must be smaller");
                assert!(
                    check(&big, cap, KvDType::F32).is_ok(),
                    "a suggested cap of {cap} must itself pass"
                );
            }
            Err(EngineError::NotEnoughMemory {
                suggested_cap: None,
                ..
            }) => { /* Nothing fits; the message says so. Acceptable here. */ }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_small_model_passes_on_any_machine_that_can_run_the_tests() {
        let mut small = info();
        small.tensor_bytes = 64 * MIB;
        small.block_count = Some(4);
        let e = check(&small, 512, KvDType::F16).unwrap();
        assert!(e.total_bytes > 0);
    }

    #[test]
    fn the_default_thread_count_leaves_something_for_the_ui() {
        let threads = default_threads();
        assert!(threads >= 1, "always at least one worker");
        if let Some(cores) = sysinfo::System::physical_core_count() {
            assert!(threads <= cores.max(1));
            if cores > 1 {
                assert!(threads < cores, "one core is left for the rest of the app");
            }
        }
    }
}
