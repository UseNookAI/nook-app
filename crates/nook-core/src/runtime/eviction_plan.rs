//! Ports `runtime/EvictionPlan.java`.
//!
//! Decides which idle engines to evict for an incoming model. Candidates come least recently used
//! first with an estimate of what each holds on the GPU. A small model (the embedding model, say)
//! stays resident: it is evicted only when its eviction makes the difference, never when the
//! incoming model cannot be fully placed anyway, and never when the larger candidates behind it
//! cover the need on their own.

/// Below this a resident model buys the incoming one at most a layer of offload.
pub const SMALL_MODEL_BYTES: u64 = 512 << 20;

/// Indices into `candidates` to evict, in order.
///
/// - `budget`: free bytes now
/// - `need`: bytes the incoming model wants
/// - `candidates`: estimated resident bytes per idle engine, least recently used first
pub fn victims(budget: u64, need: u64, candidates: &[u64]) -> Vec<usize> {
    let reclaimable: u64 = candidates.iter().sum();
    let reachable = budget + reclaimable >= need;
    let mut out = Vec::new();
    let mut freed = 0u64;
    for (i, &candidate) in candidates.iter().enumerate() {
        if budget + freed >= need {
            break;
        }
        let rest: u64 = candidates[i + 1..].iter().sum();
        let small = candidate < SMALL_MODEL_BYTES;
        if small && (!reachable || budget + freed + rest >= need) {
            continue;
        }
        out.push(i);
        freed += candidate;
    }
    out
}

/// The eviction rule from QA finding 3: small models stay unless evicting them makes the
/// difference.
#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1 << 30;
    const NOMIC: u64 = 146 << 20;
    const QWEN: u64 = 5 * GB;
    const WHISPER: u64 = (488 << 20) * 2 + (512 << 20);

    #[test]
    fn keeps_the_embedding_model_when_the_chat_model_cannot_fit_anyway() {
        // 4.4 GB free, Qwen3 8B wants 5.7 GB: evicting nomic buys one layer and costs a reload.
        let budget = (4.4 * GB as f64) as u64;
        let need = (5.7 * GB as f64) as u64;
        assert_eq!(victims(budget, need, &[NOMIC]), Vec::<usize>::new());
    }

    #[test]
    fn evicts_the_big_idle_model_for_whisper_and_leaves_the_small_one() {
        // nomic is least recently used, but Qwen alone covers what whisper needs.
        assert_eq!(victims(0, WHISPER, &[NOMIC, QWEN]), vec![1]);
    }

    #[test]
    fn evicts_small_models_when_they_are_what_makes_the_difference() {
        let budget = GB;
        let need = budget + NOMIC + (50 << 20);
        assert_eq!(victims(budget, need, &[NOMIC, 100 << 20]), vec![0, 1]);
    }

    #[test]
    fn stops_once_the_need_is_covered() {
        assert_eq!(victims(0, 3 * GB, &[QWEN, 4 * GB]), vec![0]);
        assert_eq!(victims(6 * GB, 3 * GB, &[QWEN]), Vec::<usize>::new());
    }

    #[test]
    fn evicts_large_models_even_when_the_target_is_out_of_reach() {
        // A bigger incoming model still takes every layer it can get from a large idle one.
        assert_eq!(victims(0, 8 * GB, &[QWEN, NOMIC]), vec![0]);
    }
}
