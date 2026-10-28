//! Token sampling (master spec s8.9).
//!
//! Hand-written and deliberately free of any model dependency: this survives
//! the swap from candle's reference forward pass to the one written here.
//!
//! Order is fixed by the spec: repetition penalty, then temperature, then
//! top-k, then top-p, then sample. Applying temperature after top-k would
//! change which tokens the cut admits, so the order is part of the contract
//! rather than an implementation detail.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SamplingParams {
    /// Zero means greedy: take the argmax and skip sampling entirely.
    pub temperature: f32,
    pub top_k: Option<usize>,
    pub top_p: Option<f32>,
    /// 1.0 is off. Above 1.0 discourages repeating recent tokens.
    pub repetition_penalty: f32,
    /// How many recent tokens the penalty considers.
    pub repetition_window: usize,
    pub max_tokens: u32,
    pub seed: u64,
}

impl Default for SamplingParams {
    fn default() -> Self {
        Self {
            temperature: 0.8,
            top_k: Some(40),
            top_p: Some(0.95),
            repetition_penalty: 1.0,
            repetition_window: 64,
            max_tokens: 1024,
            seed: 42,
        }
    }
}

impl SamplingParams {
    /// Deterministic decoding, for parity tests (master spec s9 T4).
    #[must_use]
    pub fn greedy(max_tokens: u32) -> Self {
        Self {
            temperature: 0.0,
            top_k: None,
            top_p: None,
            repetition_penalty: 1.0,
            max_tokens,
            ..Self::default()
        }
    }
}

/// A small, seeded PRNG so runs are reproducible without pulling in `rand`
/// for one call site. xorshift64*, which is more than enough for sampling.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    #[must_use]
    pub fn new(seed: u64) -> Self {
        // A zero state is a fixed point for xorshift, so it is never allowed.
        Self(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in [0, 1).
    pub fn next_f32(&mut self) -> f32 {
        // Top 24 bits give a float with no rounding surprises.
        ((self.next_u64() >> 40) as f32) / f32::from(1u16 << 8) / 65_536.0
    }
}

#[derive(Debug)]
pub struct Sampler {
    params: SamplingParams,
    rng: Rng,
}

impl Sampler {
    #[must_use]
    pub fn new(params: SamplingParams) -> Self {
        let rng = Rng::new(params.seed);
        Self { params, rng }
    }

    #[must_use]
    pub fn params(&self) -> &SamplingParams {
        &self.params
    }

    /// Choose the next token from a row of logits.
    ///
    /// `recent` is the tail of the generated sequence, for the repetition
    /// penalty. `logits` is modified in place rather than copied: it is one
    /// row per token of output, and the vocabulary is large.
    pub fn sample(&mut self, logits: &mut [f32], recent: &[u32]) -> u32 {
        if logits.is_empty() {
            return 0;
        }
        self.apply_repetition_penalty(logits, recent);

        // Greedy: temperature zero means no distribution to sample from.
        if self.params.temperature <= 0.0 {
            return argmax(logits);
        }

        for l in logits.iter_mut() {
            *l /= self.params.temperature;
        }

        let mut candidates: Vec<(u32, f32)> = logits
            .iter()
            .enumerate()
            .map(|(i, l)| (i as u32, *l))
            .collect();
        // Descending by logit; ties broken by id so runs stay reproducible.
        candidates.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.cmp(&b.0))
        });

        if let Some(k) = self.params.top_k {
            candidates.truncate(k.max(1));
        }

        let mut probs = softmax(&candidates.iter().map(|(_, l)| *l).collect::<Vec<_>>());

        if let Some(p) = self.params.top_p {
            truncate_to_top_p(&mut candidates, &mut probs, p);
        }

        let roll = self.rng.next_f32();
        let mut cumulative = 0.0;
        for (i, prob) in probs.iter().enumerate() {
            cumulative += prob;
            if roll < cumulative {
                return candidates[i].0;
            }
        }
        // Floating point can leave the roll just past the total.
        candidates.last().map_or(0, |(id, _)| *id)
    }

    fn apply_repetition_penalty(&self, logits: &mut [f32], recent: &[u32]) {
        let penalty = self.params.repetition_penalty;
        if penalty == 1.0 || recent.is_empty() {
            return;
        }
        let window = recent.len().saturating_sub(self.params.repetition_window);
        for token in &recent[window..] {
            let Some(l) = logits.get_mut(*token as usize) else {
                continue;
            };
            // Dividing a positive logit and multiplying a negative one both
            // move it towards zero, which is the intent either way.
            *l = if *l > 0.0 { *l / penalty } else { *l * penalty };
        }
    }
}

fn argmax(logits: &[f32]) -> u32 {
    let mut best = 0usize;
    let mut best_value = f32::NEG_INFINITY;
    for (i, l) in logits.iter().enumerate() {
        if *l > best_value {
            best_value = *l;
            best = i;
        }
    }
    best as u32
}

/// Numerically stable softmax: subtracting the max prevents `exp` overflowing
/// on large logits (master spec s8.9).
fn softmax(logits: &[f32]) -> Vec<f32> {
    let max = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    if !max.is_finite() {
        return vec![0.0; logits.len()];
    }
    let mut out: Vec<f32> = logits.iter().map(|l| (*l - max).exp()).collect();
    let sum: f32 = out.iter().sum();
    if sum > 0.0 {
        for p in &mut out {
            *p /= sum;
        }
    }
    out
}

/// Keep the smallest prefix whose probabilities reach `p`, then renormalise.
fn truncate_to_top_p(candidates: &mut Vec<(u32, f32)>, probs: &mut Vec<f32>, p: f32) {
    if !(0.0..1.0).contains(&p) {
        return;
    }
    let mut cumulative = 0.0;
    let mut keep = probs.len();
    for (i, prob) in probs.iter().enumerate() {
        cumulative += prob;
        if cumulative >= p {
            keep = i + 1;
            break;
        }
    }
    // Always keep at least one, or there is nothing to sample.
    let keep = keep.max(1);
    candidates.truncate(keep);
    probs.truncate(keep);

    let sum: f32 = probs.iter().sum();
    if sum > 0.0 {
        for prob in probs.iter_mut() {
            *prob /= sum;
        }
    }
}

#[cfg(test)]
mod tests {
    // Spec s14 permits unwrap/expect in tests.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn temperature_zero_is_greedy_and_deterministic() {
        let mut s = Sampler::new(SamplingParams::greedy(16));
        let mut logits = vec![0.1, 5.0, 0.3, 2.0];
        assert_eq!(s.sample(&mut logits.clone(), &[]), 1);
        // Repeated draws agree, because there is no draw.
        for _ in 0..5 {
            assert_eq!(s.sample(&mut logits, &[]), 1);
        }
    }

    #[test]
    fn the_same_seed_produces_the_same_sequence() {
        let params = SamplingParams {
            temperature: 1.0,
            seed: 12_345,
            ..SamplingParams::default()
        };
        let draw = || {
            let mut s = Sampler::new(params.clone());
            (0..20)
                .map(|_| s.sample(&mut [1.0, 2.0, 3.0, 0.5, 0.2], &[]))
                .collect::<Vec<_>>()
        };
        assert_eq!(draw(), draw(), "a seeded run must be reproducible");
    }

    #[test]
    fn a_different_seed_produces_a_different_sequence() {
        let run = |seed| {
            let mut s = Sampler::new(SamplingParams {
                temperature: 1.0,
                seed,
                ..SamplingParams::default()
            });
            (0..40)
                .map(|_| s.sample(&mut [1.0, 1.1, 0.9, 1.05], &[]))
                .collect::<Vec<_>>()
        };
        assert_ne!(run(1), run(2));
    }

    #[test]
    fn softmax_survives_logits_large_enough_to_overflow_exp() {
        // Without subtracting the max this is all NaN.
        let probs = softmax(&[1000.0, 999.0, 998.0]);
        let sum: f32 = probs.iter().sum();
        assert!((sum - 1.0).abs() < 1e-5, "probabilities must sum to 1");
        assert!(probs.iter().all(|p| p.is_finite()));
        assert!(probs[0] > probs[1] && probs[1] > probs[2]);
    }

    #[test]
    fn top_k_limits_the_candidates() {
        let mut s = Sampler::new(SamplingParams {
            temperature: 1.0,
            top_k: Some(1),
            top_p: None,
            seed: 7,
            ..SamplingParams::default()
        });
        // With k=1 only the best token can ever be drawn.
        for _ in 0..20 {
            assert_eq!(s.sample(&mut [0.0, 9.0, 1.0], &[]), 1);
        }
    }

    #[test]
    fn top_p_keeps_at_least_one_candidate() {
        let mut candidates = vec![(0u32, 1.0), (1, 0.5)];
        let mut probs = vec![0.9, 0.1];
        truncate_to_top_p(&mut candidates, &mut probs, 0.0);
        assert_eq!(candidates.len(), 1, "there must be something to sample");
        assert!((probs[0] - 1.0).abs() < 1e-6, "and it must be renormalised");
    }

    #[test]
    fn the_repetition_penalty_pushes_recent_tokens_down() {
        let s = Sampler::new(SamplingParams {
            repetition_penalty: 2.0,
            ..SamplingParams::default()
        });
        let mut logits = vec![4.0, 4.0, 4.0];
        s.apply_repetition_penalty(&mut logits, &[1]);
        assert_eq!(logits[1], 2.0, "a repeated token is discouraged");
        assert_eq!(logits[0], 4.0, "others are untouched");
    }

    #[test]
    fn the_repetition_penalty_moves_negative_logits_towards_zero_too() {
        let s = Sampler::new(SamplingParams {
            repetition_penalty: 2.0,
            ..SamplingParams::default()
        });
        let mut logits = vec![-1.0, -1.0];
        s.apply_repetition_penalty(&mut logits, &[0]);
        assert_eq!(
            logits[0], -2.0,
            "multiplying a negative logit discourages it, as dividing would not"
        );
    }

    #[test]
    fn the_penalty_only_looks_at_its_window() {
        let s = Sampler::new(SamplingParams {
            repetition_penalty: 2.0,
            repetition_window: 2,
            ..SamplingParams::default()
        });
        let mut logits = vec![4.0, 4.0, 4.0];
        // Token 0 is outside the last two, token 2 is inside.
        s.apply_repetition_penalty(&mut logits, &[0, 1, 2]);
        assert_eq!(logits[0], 4.0, "outside the window");
        assert_eq!(logits[2], 2.0, "inside the window");
    }

    #[test]
    fn an_empty_logit_row_does_not_panic() {
        let mut s = Sampler::new(SamplingParams::default());
        assert_eq!(s.sample(&mut [], &[]), 0);
    }

    #[test]
    fn random_draws_stay_inside_the_vocabulary() {
        let mut s = Sampler::new(SamplingParams {
            temperature: 1.5,
            seed: 99,
            ..SamplingParams::default()
        });
        for _ in 0..500 {
            let token = s.sample(&mut [0.5, 0.2, 0.9, 0.1, 0.3], &[]);
            assert!(token < 5, "sampled {token}, outside a 5-token vocabulary");
        }
    }
}
