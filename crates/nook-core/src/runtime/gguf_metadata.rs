//! Ports `runtime/GgufMetadata.java`.

use std::collections::HashMap;
use std::io::{BufReader, Read, Seek};
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};

const GGUF_MAGIC: u32 = 0x4655_4747; // "GGUF" little-endian

const T_UINT8: u32 = 0;
const T_INT8: u32 = 1;
const T_UINT16: u32 = 2;
const T_INT16: u32 = 3;
const T_UINT32: u32 = 4;
const T_INT32: u32 = 5;
const T_FLOAT32: u32 = 6;
const T_BOOL: u32 = 7;
const T_STRING: u32 = 8;
const T_ARRAY: u32 = 9;
const T_UINT64: u32 = 10;
const T_INT64: u32 = 11;
const T_FLOAT64: u32 = 12;

/// Strings longer than this are skipped (kept as ""), so a huge chat template costs nothing.
const MAX_STRING_KEEP: u64 = 8192;

/// One scalar metadata value. Integers of every width are widened to i64 (a u64 above
/// `i64::MAX` wraps, as Java's signed long did); arrays are never kept.
#[derive(Clone, Debug, PartialEq)]
pub enum GgufValue {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
}

/// Minimal reader for the GGUF header. It reads only the key/value metadata block, never the
/// tensors, so opening a multi-gigabyte model costs a few milliseconds. Large arrays such as the
/// tokenizer vocabulary are skipped without being materialised.
///
/// The values needed for the memory footprint are exposed directly; everything else is available
/// through [`GgufMetadata::get`].
///
/// Serializes (for the UI) as a summary: architecture, name, layers, context length, experts and
/// whether it is an embedding model; the raw key/value map stays inside.
#[derive(Clone, Debug)]
pub struct GgufMetadata {
    values: HashMap<String, GgufValue>,
    file_bytes: u64,
    tensor_count: u64,
    architecture: String,
}

impl GgufMetadata {
    /// Reads the header of a GGUF file. Blocking; a few milliseconds.
    pub fn read(file: &Path) -> Result<GgufMetadata> {
        let size = std::fs::metadata(file)
            .with_context(|| format!("Could not read {}", file.display()))?
            .len();
        let raw = std::fs::File::open(file)
            .with_context(|| format!("Could not read {}", file.display()))?;
        let mut r = Reader {
            inner: BufReader::with_capacity(1 << 16, raw),
            len: size,
        };
        let magic = r.u32()?;
        if magic != GGUF_MAGIC {
            bail!("Not a GGUF file: {}", file.display());
        }
        let version = r.u32()?;
        if !(2..=3).contains(&version) {
            bail!("Unsupported GGUF version {version} in {}", file.display());
        }
        let tensor_count = r.u64()?;
        let kv_count = r.u64()?;
        if tensor_count > i64::MAX as u64 || kv_count > 1_000_000 {
            bail!("Corrupt GGUF header in {}", file.display());
        }
        let mut values = HashMap::new();
        for _ in 0..kv_count {
            let key = r.string(i32::MAX as u64)?;
            let ty = r.u32()?;
            if let Some(value) = r.value(ty, &key)? {
                values.insert(key, value);
            }
        }
        Ok(GgufMetadata::from_values(values, size, tensor_count))
    }

    fn from_values(
        values: HashMap<String, GgufValue>,
        file_bytes: u64,
        tensor_count: u64,
    ) -> GgufMetadata {
        let architecture = match values.get("general.architecture") {
            Some(GgufValue::Str(s)) => s.clone(),
            Some(GgufValue::Int(i)) => i.to_string(),
            Some(GgufValue::Float(f)) => f.to_string(),
            Some(GgufValue::Bool(b)) => b.to_string(),
            None => "llama".to_string(),
        };
        GgufMetadata {
            values,
            file_bytes,
            tensor_count,
            architecture,
        }
    }

    pub fn get(&self, key: &str) -> Option<&GgufValue> {
        self.values.get(key)
    }

    pub fn architecture(&self) -> &str {
        &self.architecture
    }

    pub fn name(&self) -> String {
        self.str("general.name")
            .unwrap_or(&self.architecture)
            .to_string()
    }

    pub fn file_bytes(&self) -> u64 {
        self.file_bytes
    }

    /// Tensors the file holds, as its header counts them.
    pub fn tensor_count(&self) -> u64 {
        self.tensor_count
    }

    /// Why the file is not a language model llama.cpp can run, as a clause that finishes "X can't
    /// run in Nook: ..." ("its architecture 'clip' is a vision encoder or projector, not a
    /// language model"), or None when the header is one's.
    ///
    /// GGUF is also how the files that work beside a model ship: vision projectors and encoders
    /// (`clip`, `deepseek4-vision`), multi-token-prediction heads, adapters, importance matrices.
    /// A language model's header names its architecture and carries `<arch>.block_count`,
    /// `<arch>.context_length` and `<arch>.embedding_length` (llama.cpp refuses a file without
    /// them), and the file holds at least a tensor per layer; prediction heads cut from a model
    /// keep its header but hold only their own layer's tensors. The first file of a split model
    /// may hold no tensors at all, so the count is not held against one.
    pub fn not_a_language_model(&self) -> Option<String> {
        if let Some(kind) = self
            .str("general.type")
            .filter(|t| !t.eq_ignore_ascii_case("model"))
        {
            let what = match kind.to_ascii_lowercase().as_str() {
                "mmproj" => "a vision projector".to_string(),
                "adapter" => "an adapter (LoRA)".to_string(),
                "imatrix" => "an importance matrix".to_string(),
                other => format!("a '{other}' file"),
            };
            return Some(format!("it is {what}, not a language model"));
        }
        let arch = self
            .str("general.architecture")
            .filter(|a| !a.trim().is_empty());
        let Some(arch) = arch else {
            return Some("its header names no model architecture".to_string());
        };
        let a = arch.to_ascii_lowercase();
        if a.split(|c: char| !c.is_ascii_alphanumeric())
            .any(|t| t == "mtp")
        {
            return Some(format!(
                "its architecture '{arch}' is multi-token-prediction heads, not a language model"
            ));
        }
        let vision = a == "clip" || a.contains("vision") || a.contains("mmproj");
        if vision || a.contains("projector") || a.contains("encoder") {
            return Some(format!(
                "its architecture '{arch}' is {} encoder or projector, not a language model",
                if vision { "a vision" } else { "an" }
            ));
        }
        let missing: Vec<String> = ["block_count", "context_length", "embedding_length"]
            .iter()
            .filter(|k| self.arch_num(k).is_none())
            .map(|k| format!("{arch}.{k}"))
            .collect();
        if let Some((last, rest)) = missing.split_last() {
            let keys = if rest.is_empty() {
                last.clone()
            } else {
                format!("{} and {last}", rest.join(", "))
            };
            return Some(format!(
                "its header lacks {keys}, which every language model has"
            ));
        }
        let split = matches!(self.values.get("split.count"), Some(GgufValue::Int(n)) if *n > 1);
        let layers = self.layers() as u64;
        if !split && self.tensor_count < layers {
            return Some(format!(
                "it holds {} tensors for {layers} layers, a part of a model (such as its multi-token-prediction heads), not a whole one",
                self.tensor_count
            ));
        }
        None
    }

    pub fn layers(&self) -> u32 {
        self.arch_num("block_count").unwrap_or(32)
    }

    /// Experts per layer for a mixture-of-experts model, 0 for a dense one.
    pub fn expert_count(&self) -> u32 {
        self.arch_num("expert_count").unwrap_or(0)
    }

    pub fn is_mixture_of_experts(&self) -> bool {
        self.expert_count() > 0
    }

    pub fn heads(&self) -> u32 {
        self.arch_num("attention.head_count").unwrap_or(32)
    }

    pub fn kv_heads(&self) -> u32 {
        self.arch_num("attention.head_count_kv")
            .unwrap_or_else(|| self.heads())
    }

    pub fn embedding_length(&self) -> u32 {
        self.arch_num("embedding_length").unwrap_or(4096)
    }

    pub fn context_length(&self) -> u32 {
        self.arch_num("context_length").unwrap_or(8192)
    }

    /// Head dimension: explicit key_length when present, else embedding / heads.
    pub fn head_dim(&self) -> u32 {
        self.arch_num("attention.key_length")
            .unwrap_or_else(|| (self.embedding_length() / self.heads().max(1)).max(1))
    }

    /// Bytes of KV cache per context token for an f16 cache (both K and V, all layers).
    pub fn kv_bytes_per_token_f16(&self) -> u64 {
        2 * self.layers() as u64 * self.kv_heads() as u64 * self.head_dim() as u64 * 2
    }

    /// True for embedding-only models, which need no KV cache sizing for generation.
    pub fn is_embedding_model(&self) -> bool {
        self.values
            .contains_key(&format!("{}.pooling_type", self.architecture))
            || self.architecture == "bert"
            || self.architecture == "nomic-bert"
    }

    fn str(&self, key: &str) -> Option<&str> {
        match self.values.get(key) {
            Some(GgufValue::Str(s)) => Some(s),
            _ => None,
        }
    }

    /// A numeric value under `<architecture>.<suffix>`, truncated to an int as Java's
    /// `Number.intValue()` did for the values that matter (all non-negative in real files).
    fn arch_num(&self, suffix: &str) -> Option<u32> {
        match self
            .values
            .get(&format!("{}.{suffix}", self.architecture))?
        {
            GgufValue::Int(i) => Some(*i as u32),
            GgufValue::Float(f) => Some(*f as u32),
            _ => None,
        }
    }
}

impl Serialize for GgufMetadata {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut s = serializer.serialize_struct("GgufMetadata", 8)?;
        s.serialize_field("architecture", &self.architecture)?;
        s.serialize_field("name", &self.name())?;
        s.serialize_field("fileBytes", &self.file_bytes)?;
        s.serialize_field("layers", &self.layers())?;
        s.serialize_field("contextLength", &self.context_length())?;
        s.serialize_field("expertCount", &self.expert_count())?;
        s.serialize_field("mixtureOfExperts", &self.is_mixture_of_experts())?;
        s.serialize_field("embeddingModel", &self.is_embedding_model())?;
        s.end()
    }
}

/// Little-endian primitive reader.
struct Reader<R: Read + Seek> {
    inner: BufReader<R>,
    len: u64,
}

impl<R: Read + Seek> Reader<R> {
    fn fill<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut b = [0u8; N];
        self.inner
            .read_exact(&mut b)
            .context("Unexpected end of GGUF header")?;
        Ok(b)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.fill::<1>()?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.fill()?))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.fill()?))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.fill()?))
    }
    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_le_bytes(self.fill()?))
    }
    fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_le_bytes(self.fill()?))
    }

    fn string(&mut self, keep_up_to: u64) -> Result<String> {
        let len = self.u64()?;
        if len > i32::MAX as u64 {
            bail!("Corrupt GGUF string length {len}");
        }
        if len > keep_up_to {
            self.skip(len)?;
            return Ok(String::new());
        }
        let mut b = vec![0u8; len as usize];
        self.inner
            .read_exact(&mut b)
            .context("Unexpected end of GGUF header")?;
        Ok(String::from_utf8_lossy(&b).into_owned())
    }

    fn skip(&mut self, n: u64) -> Result<()> {
        let n = i64::try_from(n).map_err(|_| anyhow::anyhow!("Unexpected end of GGUF header"))?;
        self.inner
            .seek_relative(n)
            .context("Unexpected end of GGUF header")?;
        // Seeking past the end succeeds silently; a skip that runs off the file is a truncated one.
        if self.inner.stream_position()? > self.len {
            bail!("Unexpected end of GGUF header");
        }
        Ok(())
    }

    /// Reads a scalar value; arrays are skipped and return None.
    fn value(&mut self, ty: u32, key: &str) -> Result<Option<GgufValue>> {
        Ok(Some(match ty {
            T_UINT8 => GgufValue::Int(self.u8()? as i64),
            T_INT8 => GgufValue::Int(self.u8()? as i8 as i64),
            T_UINT16 => GgufValue::Int(self.u16()? as i64),
            T_INT16 => GgufValue::Int(self.u16()? as i16 as i64),
            T_UINT32 => GgufValue::Int(self.u32()? as i64),
            T_INT32 => GgufValue::Int(self.u32()? as i32 as i64),
            T_FLOAT32 => GgufValue::Float(self.f32()? as f64),
            T_BOOL => GgufValue::Bool(self.u8()? != 0),
            T_STRING => GgufValue::Str(self.string(MAX_STRING_KEEP)?),
            T_UINT64 | T_INT64 => GgufValue::Int(self.u64()? as i64),
            T_FLOAT64 => GgufValue::Float(self.f64()?),
            T_ARRAY => {
                let elem = self.u32()?;
                let count = self.u64()?;
                self.skip_array(elem, count)?;
                return Ok(None);
            }
            _ => bail!("Unknown GGUF value type {ty} for key {key}"),
        }))
    }

    fn skip_array(&mut self, elem: u32, count: u64) -> Result<()> {
        match elem {
            T_UINT8 | T_INT8 | T_BOOL => self.skip(count),
            T_UINT16 | T_INT16 => self.skip(count.saturating_mul(2)),
            T_UINT32 | T_INT32 | T_FLOAT32 => self.skip(count.saturating_mul(4)),
            T_UINT64 | T_INT64 | T_FLOAT64 => self.skip(count.saturating_mul(8)),
            T_STRING => {
                for _ in 0..count {
                    let len = self.u64()?;
                    self.skip(len)?;
                }
                Ok(())
            }
            T_ARRAY => {
                for _ in 0..count {
                    let t = self.u32()?;
                    let c = self.u64()?;
                    self.skip_array(t, c)?;
                }
                Ok(())
            }
            _ => bail!("Unknown GGUF array element type {elem}"),
        }
    }
}

/// Test helpers shared with the footprint and registry tests.
#[cfg(test)]
pub(crate) mod testing {
    use std::io::Write;
    use std::path::{Path, PathBuf};

    /// A value for [`write_gguf`].
    #[derive(Clone)]
    pub enum Kv<'a> {
        U32(u32),
        Str(&'a str),
        StrArray(&'a [&'a str]),
    }

    /// The tensor count a test header claims: a whole model's worth for any layer count the tests
    /// use. The tensor descriptions themselves are not written; the reader stops after the
    /// metadata.
    pub const TENSORS: u64 = 1024;

    /// Writes the header of a language model of architecture `arch` to `dir/name`: the keys every
    /// one has (8 layers, 4096 trained context, 512 wide), then `extra`.
    pub fn write_language_model(
        dir: &Path,
        name: &str,
        arch: &str,
        extra: &[(&str, Kv)],
        pad_to_bytes: u64,
    ) -> PathBuf {
        let keys = [
            format!("{arch}.block_count"),
            format!("{arch}.context_length"),
            format!("{arch}.embedding_length"),
        ];
        let mut kv = vec![
            ("general.architecture", Kv::Str(arch)),
            (keys[0].as_str(), Kv::U32(8)),
            (keys[1].as_str(), Kv::U32(4096)),
            (keys[2].as_str(), Kv::U32(512)),
        ];
        kv.extend(extra.iter().cloned());
        write_gguf_named(dir, name, &kv, pad_to_bytes)
    }

    /// Writes a minimal GGUF v3 header with the given metadata to `dir/test.gguf`, padded
    /// (sparsely) to `pad_to_bytes`.
    pub fn write_gguf(dir: &Path, kv: &[(&str, Kv)], pad_to_bytes: u64) -> PathBuf {
        write_gguf_named(dir, "test.gguf", kv, pad_to_bytes)
    }

    pub fn write_gguf_named(
        dir: &Path,
        name: &str,
        kv: &[(&str, Kv)],
        pad_to_bytes: u64,
    ) -> PathBuf {
        write_gguf_tensors(dir, name, kv, TENSORS, pad_to_bytes)
    }

    /// [`write_gguf_named`] with a header that claims `tensors` tensors.
    pub fn write_gguf_tensors(
        dir: &Path,
        name: &str,
        kv: &[(&str, Kv)],
        tensors: u64,
        pad_to_bytes: u64,
    ) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let file = dir.join(name);
        let mut out = Vec::new();
        out.extend_from_slice(&0x4655_4747u32.to_le_bytes());
        out.extend_from_slice(&3u32.to_le_bytes());
        out.extend_from_slice(&tensors.to_le_bytes());
        out.extend_from_slice(&(kv.len() as u64).to_le_bytes());
        fn string(out: &mut Vec<u8>, s: &str) {
            out.extend_from_slice(&(s.len() as u64).to_le_bytes());
            out.extend_from_slice(s.as_bytes());
        }
        for (key, value) in kv {
            string(&mut out, key);
            match value {
                Kv::U32(i) => {
                    out.extend_from_slice(&4u32.to_le_bytes());
                    out.extend_from_slice(&i.to_le_bytes());
                }
                Kv::Str(s) => {
                    out.extend_from_slice(&8u32.to_le_bytes());
                    string(&mut out, s);
                }
                Kv::StrArray(arr) => {
                    out.extend_from_slice(&9u32.to_le_bytes());
                    out.extend_from_slice(&8u32.to_le_bytes());
                    out.extend_from_slice(&(arr.len() as u64).to_le_bytes());
                    for s in arr.iter() {
                        string(&mut out, s);
                    }
                }
            }
        }
        let mut f = std::fs::File::create(&file).unwrap();
        f.write_all(&out).unwrap();
        if pad_to_bytes > out.len() as u64 {
            // Sparse extension: the reader only needs the file size, not real weights.
            f.set_len(pad_to_bytes).unwrap();
        }
        file
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{write_gguf, write_gguf_tensors, write_language_model, Kv};
    use super::*;

    fn verdict(kv: &[(&str, Kv)], tensors: u64) -> Option<String> {
        let dir = tempfile::tempdir().unwrap();
        let file = write_gguf_tensors(dir.path(), "x.gguf", kv, tensors, 0);
        GgufMetadata::read(&file).unwrap().not_a_language_model()
    }

    /// The headers of the files that sit beside a model, as the real ones carry them: a llama.cpp
    /// vision projector, the DeepSeek V4 vision encoder and Qwen's prediction heads a person
    /// downloaded as models on 2026-09-26, next to a language model's.
    #[test]
    fn a_header_tells_a_language_model_from_the_files_beside_one() {
        let dir = tempfile::tempdir().unwrap();
        let model = write_language_model(dir.path(), "m.gguf", "qwen3", &[], 0);
        let meta = GgufMetadata::read(&model).unwrap();
        assert_eq!(meta.not_a_language_model(), None);
        assert_eq!(meta.tensor_count(), testing::TENSORS);

        let mmproj = [
            ("general.architecture", Kv::Str("clip")),
            ("general.type", Kv::Str("mmproj")),
            ("clip.has_vision_encoder", Kv::U32(1)),
            ("clip.projector_type", Kv::Str("gemma3")),
            ("clip.vision.block_count", Kv::U32(27)),
            ("clip.vision.embedding_length", Kv::U32(1152)),
        ];
        assert_eq!(
            verdict(&mmproj, 439).as_deref(),
            Some("it is a vision projector, not a language model")
        );
        // An older projector says only its architecture.
        assert_eq!(
            verdict(
                &mmproj[2..]
                    .iter()
                    .cloned()
                    .chain([("general.architecture", Kv::Str("clip"))])
                    .collect::<Vec<_>>(),
                439
            )
            .as_deref(),
            Some("its architecture 'clip' is a vision encoder or projector, not a language model")
        );

        let encoder = [
            ("general.architecture", Kv::Str("deepseek4-vision")),
            ("general.name", Kv::Str("DeepSeek V4 Flash Vision Encoder")),
            ("deepseek4-vision.block_count", Kv::U32(32)),
            ("deepseek4-vision.embedding_length", Kv::U32(1024)),
            ("deepseek4-vision.language.block_count", Kv::U32(43)),
        ];
        assert_eq!(
            verdict(&encoder, 316).as_deref(),
            Some("its architecture 'deepseek4-vision' is a vision encoder or projector, not a language model")
        );
        assert_eq!(
            verdict(
                &[
                    ("general.architecture", Kv::Str("t5encoder")),
                    ("t5encoder.block_count", Kv::U32(24))
                ],
                219
            )
            .as_deref(),
            Some("its architecture 't5encoder' is an encoder or projector, not a language model")
        );
        assert_eq!(
            verdict(&[("general.architecture", Kv::Str("glm4-mtp"))], 20).as_deref(),
            Some(
                "its architecture 'glm4-mtp' is multi-token-prediction heads, not a language model"
            )
        );

        // Qwen's prediction heads keep the whole model's header but hold one layer.
        let heads = [
            ("general.architecture", Kv::Str("qwen4exp")),
            ("general.type", Kv::Str("model")),
            ("qwen4exp.block_count", Kv::U32(49)),
            ("qwen4exp.context_length", Kv::U32(262144)),
            ("qwen4exp.embedding_length", Kv::U32(2560)),
            ("qwen4exp.nextn_predict_layers", Kv::U32(1)),
        ];
        assert_eq!(
            verdict(&heads, 32).as_deref(),
            Some("it holds 32 tensors for 49 layers, a part of a model (such as its multi-token-prediction heads), not a whole one")
        );
        assert_eq!(verdict(&heads, 700), None, "the whole model");
        // The first file of a split model may hold only the metadata.
        let split: Vec<(&str, Kv)> = heads
            .iter()
            .cloned()
            .chain([("split.count", Kv::U32(3)), ("split.no", Kv::U32(0))])
            .collect();
        assert_eq!(verdict(&split, 0), None);

        assert_eq!(
            verdict(&[("general.name", Kv::Str("Nameless"))], 100).as_deref(),
            Some("its header names no model architecture")
        );
        assert_eq!(
            verdict(&[("general.architecture", Kv::Str("llama")), ("llama.block_count", Kv::U32(8))], 100).as_deref(),
            Some("its header lacks llama.context_length and llama.embedding_length, which every language model has")
        );
        assert_eq!(
            verdict(
                &[
                    ("general.type", Kv::Str("adapter")),
                    ("general.architecture", Kv::Str("llama"))
                ],
                64
            )
            .as_deref(),
            Some("it is an adapter (LoRA), not a language model")
        );
    }

    #[test]
    fn reads_architecture_fields_and_skips_arrays() {
        let dir = tempfile::tempdir().unwrap();
        let file = write_gguf(
            dir.path(),
            &[
                ("general.architecture", Kv::Str("llama")),
                ("general.name", Kv::Str("Test Llama")),
                ("tokenizer.ggml.tokens", Kv::StrArray(&["a", "b", "c"])),
                ("llama.block_count", Kv::U32(32)),
                ("llama.attention.head_count", Kv::U32(32)),
                ("llama.attention.head_count_kv", Kv::U32(8)),
                ("llama.embedding_length", Kv::U32(4096)),
                ("llama.context_length", Kv::U32(131072)),
            ],
            0,
        );

        let meta = GgufMetadata::read(&file).unwrap();

        assert_eq!(meta.architecture(), "llama");
        assert_eq!(meta.name(), "Test Llama");
        assert_eq!(meta.layers(), 32);
        assert_eq!(meta.kv_heads(), 8);
        assert_eq!(meta.head_dim(), 128);
        assert_eq!(meta.context_length(), 131072);
        assert!(!meta.is_embedding_model());
        assert!(
            meta.get("tokenizer.ggml.tokens").is_none(),
            "arrays are skipped"
        );
        // 2 (K,V) * 32 layers * 8 kv heads * 128 dims * 2 bytes = 128 KiB per token
        assert_eq!(meta.kv_bytes_per_token_f16(), 131072);
    }

    #[test]
    fn rejects_non_gguf_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not.gguf");
        std::fs::write(&file, b"hello").unwrap();
        assert!(GgufMetadata::read(&file).is_err());
        let err = GgufMetadata::read(&file).unwrap_err().to_string();
        assert!(err.starts_with("Not a GGUF file: "), "{err}");
    }

    #[test]
    fn a_truncated_array_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = write_gguf(
            dir.path(),
            &[("tokenizer.ggml.tokens", Kv::StrArray(&["abc"]))],
            0,
        );
        let bytes = std::fs::read(&file).unwrap();
        std::fs::write(&file, &bytes[..bytes.len() - 2]).unwrap();
        assert!(GgufMetadata::read(&file).is_err());
    }

    #[test]
    fn defaults_and_embedding_detection() {
        let dir = tempfile::tempdir().unwrap();
        let file = write_gguf(
            dir.path(),
            &[
                ("general.architecture", Kv::Str("nomic-bert")),
                ("nomic-bert.pooling_type", Kv::U32(1)),
            ],
            0,
        );
        let meta = GgufMetadata::read(&file).unwrap();
        assert!(meta.is_embedding_model());
        assert_eq!(
            meta.name(),
            "nomic-bert",
            "no general.name: the architecture"
        );
        assert_eq!(meta.layers(), 32);
        assert_eq!(meta.context_length(), 8192);
        let json = serde_json::to_value(&meta).unwrap();
        assert_eq!(json["embeddingModel"], true);
        assert_eq!(json["architecture"], "nomic-bert");
    }
}
