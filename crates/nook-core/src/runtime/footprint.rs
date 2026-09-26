//! Ports `runtime/Footprint.java`.

use serde::{Deserialize, Serialize};

use super::gguf_metadata::GgufMetadata;

/// Memory plan for one model at a given context size. All values in bytes.
///
/// Context has two units and this record names both: `ctx_per_slot` is what one request sees,
/// [`Footprint::ctx_total`] is what llama-server is launched with (`--ctx-size`), which it
/// divides between its `--parallel` slots. The KV cache is sized for the total; the conversion
/// lives in `ctx_total` and in the engine plan's `ctx_total` and nowhere else (the Codex report of
/// 2026-09-22, P4: the estimate used to budget per slot while the launcher passed the same number
/// as the total, so each request got a fraction of the context the plan described).
///
/// - `weights_bytes`: size of the GGUF file (weights are already quantised)
/// - `kv_bytes`: KV cache for `ctx_per_slot * slots` tokens at the chosen cache type
/// - `compute_bytes`: scratch buffers, estimated at 6% of the weights with a 256 MB floor
/// - `layers`: transformer layers in the model
/// - `ctx_per_slot`: context tokens one request sees
/// - `slots`: parallel slots, each with its own `ctx_per_slot`
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Footprint {
    pub weights_bytes: u64,
    pub kv_bytes: u64,
    pub compute_bytes: u64,
    pub layers: u32,
    pub ctx_per_slot: u32,
    pub slots: u32,
}

impl Footprint {
    pub const MIN_COMPUTE_BYTES: u64 = 256 << 20;

    /// The context llama-server is launched with: every slot's share added up.
    pub fn ctx_total(&self) -> u32 {
        self.ctx_per_slot.saturating_mul(self.slots.max(1))
    }

    pub fn total_bytes(&self) -> u64 {
        self.weights_bytes + self.kv_bytes + self.compute_bytes
    }

    /// Approximate bytes per layer; the embedding and output tensors count as one extra layer.
    pub fn bytes_per_layer(&self) -> u64 {
        self.weights_bytes / (self.layers as u64 + 1).max(1)
    }

    /// Builds the plan for a model.
    ///
    /// - `ctx_per_slot`: context tokens one request sees
    /// - `slots`: parallel slots, each with that context
    /// - `kv_quantized`: true when the K and V caches run at q8_0 (half of f16)
    pub fn of(meta: &GgufMetadata, ctx_per_slot: u32, slots: u32, kv_quantized: bool) -> Footprint {
        let weights = meta.file_bytes();
        let mut kv_per_token = if meta.is_embedding_model() {
            0
        } else {
            meta.kv_bytes_per_token_f16()
        };
        if kv_quantized {
            kv_per_token = kv_per_token * 17 / 32; // q8_0 uses 8.5 bits per value instead of 16
        }
        let n = slots.max(1);
        let kv = kv_per_token * ctx_per_slot as u64 * n as u64;
        let compute = Footprint::MIN_COMPUTE_BYTES.max(weights * 6 / 100);
        Footprint {
            weights_bytes: weights,
            kv_bytes: kv,
            compute_bytes: compute,
            layers: meta.layers(),
            ctx_per_slot,
            slots: n,
        }
    }

    /// How many layers fit on a device with the given free budget when the whole KV cache and the
    /// compute buffers must also live on that device. Returns a value in `[0, layers + 1]`; a
    /// value of `layers + 1` means everything fits and full offload should be requested.
    pub fn gpu_layers_for(&self, budget_bytes: u64) -> u32 {
        let for_weights = budget_bytes as i128 - self.kv_bytes as i128 - self.compute_bytes as i128;
        if for_weights <= 0 {
            return 0;
        }
        let per_layer = self.bytes_per_layer().max(1) as i128;
        let layers_fit = for_weights / per_layer;
        layers_fit.min(self.layers as i128 + 1).max(0) as u32
    }

    pub fn fits_fully(&self, budget_bytes: u64) -> bool {
        // layers + 1: the embedding and output tensors are placed too
        self.gpu_layers_for(budget_bytes) > self.layers
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::gguf_metadata::testing::{write_gguf, Kv};
    use crate::runtime::gpu_inventory::DRIVER_RESERVE_BYTES;

    fn llama8b(dir: &std::path::Path, weights_bytes: u64) -> GgufMetadata {
        let file = write_gguf(
            dir,
            &[
                ("general.architecture", Kv::Str("llama")),
                ("llama.block_count", Kv::U32(32)),
                ("llama.attention.head_count", Kv::U32(32)),
                ("llama.attention.head_count_kv", Kv::U32(8)),
                ("llama.embedding_length", Kv::U32(4096)),
                ("llama.context_length", Kv::U32(131072)),
            ],
            weights_bytes,
        );
        GgufMetadata::read(&file).unwrap()
    }

    #[test]
    fn eight_billion_q4_at_eight_k_fits_an_eight_gigabyte_card() {
        let dir = tempfile::tempdir().unwrap();
        let weights = 4_900u64 << 20; // 4.9 GB
        let meta = llama8b(dir.path(), weights);

        let f16 = Footprint::of(&meta, 8192, 1, false);
        assert_eq!(f16.weights_bytes, weights);
        assert_eq!(f16.kv_bytes, 1 << 30, "128 KiB per token * 8192 = 1 GiB");
        assert_eq!(f16.compute_bytes, weights * 6 / 100);

        let q8 = Footprint::of(&meta, 8192, 1, true);
        assert!(
            q8.kv_bytes < f16.kv_bytes && q8.kv_bytes > f16.kv_bytes / 2,
            "q8_0 KV is about half of f16"
        );

        let budget8gb = (8192u64 << 20) - DRIVER_RESERVE_BYTES;
        assert!(q8.fits_fully(budget8gb));
        assert_eq!(q8.gpu_layers_for(budget8gb), 33);
    }

    #[test]
    fn partial_offload_when_budget_is_tight() {
        let dir = tempfile::tempdir().unwrap();
        let meta = llama8b(dir.path(), 4_900 << 20);
        let f = Footprint::of(&meta, 8192, 1, true);
        let budget4gb = 4096u64 << 20;
        let layers = f.gpu_layers_for(budget4gb);
        assert!(
            layers > 0 && layers < 33,
            "some but not all layers fit: {layers}"
        );
        assert!(!f.fits_fully(budget4gb));
        assert_eq!(f.gpu_layers_for(0), 0);
    }

    /// The KV cache is sized for every slot's share; the engine is launched with their sum.
    #[test]
    fn slots_multiply_the_context_the_engine_is_launched_with() {
        let dir = tempfile::tempdir().unwrap();
        let meta = llama8b(dir.path(), 4_900 << 20);
        let one = Footprint::of(&meta, 8192, 1, true);
        let two = Footprint::of(&meta, 4096, 2, true);
        let four = Footprint::of(&meta, 8192, 4, true);
        assert_eq!(
            one.kv_bytes, two.kv_bytes,
            "two requests of 4096 hold as many tokens as one of 8192"
        );
        assert_eq!(four.kv_bytes, 4 * one.kv_bytes);
        assert_eq!(one.ctx_total(), 8192);
        assert_eq!(two.ctx_total(), 8192);
        assert_eq!(four.ctx_total(), 32768);
        assert_eq!(two.ctx_per_slot, 4096);
        assert_eq!(two.slots, 2);
        assert_eq!(
            Footprint::of(&meta, 8192, 0, true).slots,
            1,
            "no slots means one"
        );
    }

    #[test]
    fn compute_buffer_has_a_floor() {
        let dir = tempfile::tempdir().unwrap();
        let meta = llama8b(dir.path(), 100 << 20);
        let f = Footprint::of(&meta, 2048, 1, false);
        assert_eq!(f.compute_bytes, Footprint::MIN_COMPUTE_BYTES);
    }
}
