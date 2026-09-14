//! Token counting (master spec s6.1).

/// Counts tokens for a target model.
///
/// The local engine implements this exactly, using its own tokenizer and chat
/// template. API backends implement it as an estimate, which is why the budget
/// keeps a safety margin.
pub trait TokenCounter: std::fmt::Debug + Send + Sync {
    fn count_text(&self, text: &str) -> u32;
}

/// Byte-based estimate: `ceil(utf8_bytes / 3)` plus 10% (master spec s6.1).
///
/// Deliberately conservative for Latin text, where real tokenizers average
/// closer to 4 bytes per token. It is much tighter for scripts whose
/// characters are 3 UTF-8 bytes each (Devanagari, Bengali, CJK), where it can
/// still land under the true count; the safety margin absorbs the difference,
/// and exact provider counting replaces this where available.
#[derive(Debug, Clone, Copy, Default)]
pub struct ByteEstimateCounter;

impl TokenCounter for ByteEstimateCounter {
    fn count_text(&self, text: &str) -> u32 {
        let bytes = text.len() as u64;
        let base = bytes.div_ceil(3);
        let with_margin = base + base / 10;
        u32::try_from(with_margin).unwrap_or(u32::MAX)
    }
}

#[cfg(test)]
mod tests {
    // Spec s14 permits unwrap/expect in tests.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn empty_text_costs_nothing() {
        assert_eq!(ByteEstimateCounter.count_text(""), 0);
    }

    #[test]
    fn estimate_rounds_up_and_adds_margin() {
        // 30 bytes -> ceil(30/3) = 10, +10% = 11
        assert_eq!(ByteEstimateCounter.count_text(&"a".repeat(30)), 11);
    }

    #[test]
    fn multibyte_text_counts_by_bytes_not_chars() {
        // Bengali: 3 UTF-8 bytes per char, so 4 chars = 12 bytes -> 4 (+0 margin).
        let s = "\u{0986}\u{09AE}\u{09BF}\u{0995}";
        assert_eq!(s.len(), 12);
        assert_eq!(ByteEstimateCounter.count_text(s), 4);
    }
}
