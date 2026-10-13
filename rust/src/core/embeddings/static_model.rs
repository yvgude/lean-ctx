// SPDX-License-Identifier: Apache-2.0
//! Static (model2vec) embeddings in pure Rust.
//!
//! A static model is a token-embedding table: a text's vector is the weighted
//! mean of its tokens' rows, L2-normalised. There is no attention pass, so it
//! needs no ONNX Runtime and embeds a code chunk in microseconds — semantic
//! search works in every build, including those without the `embeddings`
//! feature.
//!
//! The file format is safetensors, read here directly (8-byte little-endian
//! header length, a JSON header, then raw little-endian tensors). The model
//! comes from the network, so every offset, shape and type is checked before a
//! byte is read.

use std::collections::HashMap;
use std::path::Path;

/// `embeddings`: `[rows, dims]`; `mapping` (optional): token id → row
/// (vocabulary quantisation); `weights` (optional): per-token weight.
pub(crate) struct StaticModel {
    embeddings: Vec<f32>,
    rows: usize,
    dims: usize,
    mapping: Option<Vec<u32>>,
    weights: Option<Vec<f32>>,
}

/// Larger headers are not a model file.
const MAX_HEADER_BYTES: u64 = 1 << 20;

impl StaticModel {
    pub(crate) fn load(path: &Path) -> anyhow::Result<Self> {
        let bytes =
            std::fs::read(path).map_err(|e| anyhow::anyhow!("read {}: {e}", path.display()))?;
        Self::from_safetensors(&bytes)
    }

    pub(crate) fn from_safetensors(bytes: &[u8]) -> anyhow::Result<Self> {
        let header_len = bytes
            .get(..8)
            .map(|b| u64::from_le_bytes(b.try_into().expect("8 bytes")))
            .ok_or_else(|| anyhow::anyhow!("safetensors: file shorter than its header length"))?;
        if header_len > MAX_HEADER_BYTES {
            anyhow::bail!("safetensors: header of {header_len} bytes is not a model header");
        }
        let header_end = 8 + header_len as usize;
        let header: HashMap<String, serde_json::Value> = serde_json::from_slice(
            bytes
                .get(8..header_end)
                .ok_or_else(|| anyhow::anyhow!("safetensors: truncated header"))?,
        )
        .map_err(|e| anyhow::anyhow!("safetensors: header is not JSON: {e}"))?;
        let data = &bytes[header_end..];

        let embeddings = Tensor::parse(&header, "embeddings", data)?
            .ok_or_else(|| anyhow::anyhow!("safetensors: no `embeddings` tensor"))?;
        let [rows, dims] = embeddings.shape[..] else {
            anyhow::bail!(
                "safetensors: `embeddings` must be 2-D, got {:?}",
                embeddings.shape
            );
        };
        if rows == 0 || dims == 0 {
            anyhow::bail!("safetensors: empty `embeddings` tensor");
        }
        let embeddings_f32 = embeddings.to_f32()?;

        let mapping = match Tensor::parse(&header, "mapping", data)? {
            Some(t) => {
                let rows_of = t.to_row_indices()?;
                if let Some(bad) = rows_of.iter().find(|&&r| r as usize >= rows) {
                    anyhow::bail!("safetensors: `mapping` points at row {bad} of {rows}");
                }
                Some(rows_of)
            }
            None => None,
        };
        let weights = Tensor::parse(&header, "weights", data)?
            .map(|t| t.to_f32())
            .transpose()?;
        let vocab = mapping.as_ref().map_or(rows, Vec::len);
        if let Some(w) = &weights
            && w.len() != vocab
        {
            anyhow::bail!("safetensors: {} weights for {vocab} tokens", w.len());
        }

        Ok(Self {
            embeddings: embeddings_f32,
            rows,
            dims,
            mapping,
            weights,
        })
    }

    pub(crate) fn dimensions(&self) -> usize {
        self.dims
    }

    /// Token ids the model has a vector for.
    pub(crate) fn vocab_size(&self) -> usize {
        self.mapping.as_ref().map_or(self.rows, Vec::len)
    }

    /// Weighted mean of the tokens' vectors, L2-normalised. Ids outside the
    /// vocabulary are skipped; no token at all yields the zero vector.
    pub(crate) fn embed_ids(&self, ids: &[i32]) -> Vec<f32> {
        let mut out = vec![0.0f32; self.dims];
        let mut count = 0usize;
        for &id in ids {
            let Ok(token) = usize::try_from(id) else {
                continue;
            };
            if token >= self.vocab_size() {
                continue;
            }
            let row = self.mapping.as_ref().map_or(token, |m| m[token] as usize);
            let weight = self.weights.as_ref().map_or(1.0, |w| w[token]);
            let vector = &self.embeddings[row * self.dims..(row + 1) * self.dims];
            for (acc, v) in out.iter_mut().zip(vector) {
                *acc += weight * v;
            }
            count += 1;
        }
        if count == 0 {
            return out;
        }
        // The mean only rescales; normalising makes it irrelevant, but it
        // keeps intermediate magnitudes bounded for long inputs.
        let inv = 1.0 / count as f32;
        for v in &mut out {
            *v *= inv;
        }
        let norm = out.iter().map(|v| v * v).sum::<f32>().sqrt();
        if norm > 0.0 {
            for v in &mut out {
                *v /= norm;
            }
        }
        out
    }
}

struct Tensor<'a> {
    dtype: String,
    shape: Vec<usize>,
    bytes: &'a [u8],
}

impl<'a> Tensor<'a> {
    fn parse(
        header: &HashMap<String, serde_json::Value>,
        name: &str,
        data: &'a [u8],
    ) -> anyhow::Result<Option<Self>> {
        let Some(entry) = header.get(name) else {
            return Ok(None);
        };
        let dtype = entry
            .get("dtype")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("safetensors: `{name}` has no dtype"))?
            .to_string();
        let shape: Vec<usize> = entry
            .get("shape")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("safetensors: `{name}` has no shape"))?
            .iter()
            .map(|d| d.as_u64().and_then(|d| usize::try_from(d).ok()))
            .collect::<Option<_>>()
            .ok_or_else(|| anyhow::anyhow!("safetensors: `{name}` has a bad shape"))?;
        let offsets = entry
            .get("data_offsets")
            .and_then(serde_json::Value::as_array)
            .filter(|o| o.len() == 2)
            .and_then(|o| Some((o[0].as_u64()?, o[1].as_u64()?)))
            .ok_or_else(|| anyhow::anyhow!("safetensors: `{name}` has bad data_offsets"))?;
        let (start, end) = (usize::try_from(offsets.0)?, usize::try_from(offsets.1)?);
        let bytes = data
            .get(start..end)
            .filter(|_| start <= end)
            .ok_or_else(|| anyhow::anyhow!("safetensors: `{name}` lies outside the file"))?;
        let elements = shape
            .iter()
            .try_fold(1usize, |acc, d| acc.checked_mul(*d))
            .ok_or_else(|| anyhow::anyhow!("safetensors: `{name}` shape overflows"))?;
        let width = match dtype.as_str() {
            "F64" | "I64" => 8,
            "F32" | "I32" => 4,
            "F16" | "BF16" => 2,
            other => anyhow::bail!("safetensors: `{name}` has unsupported dtype {other}"),
        };
        if elements.checked_mul(width) != Some(bytes.len()) {
            anyhow::bail!(
                "safetensors: `{name}` holds {} bytes for {elements} × {dtype}",
                bytes.len()
            );
        }
        Ok(Some(Self {
            dtype,
            shape,
            bytes,
        }))
    }

    // `parse` checked that the byte length is a whole number of elements, so
    // `as_chunks` leaves no remainder.
    fn to_f32(&self) -> anyhow::Result<Vec<f32>> {
        Ok(match self.dtype.as_str() {
            "F32" => self
                .bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_le_bytes(*b))
                .collect(),
            "F64" => self
                .bytes
                .as_chunks::<8>()
                .0
                .iter()
                .map(|b| f64::from_le_bytes(*b) as f32)
                .collect(),
            "F16" => self
                .bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| f16_to_f32(u16::from_le_bytes(*b)))
                .collect(),
            "BF16" => self
                .bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| f32::from_bits(u32::from(u16::from_le_bytes(*b)) << 16))
                .collect(),
            other => anyhow::bail!("safetensors: {other} is not a float tensor"),
        })
    }

    fn to_row_indices(&self) -> anyhow::Result<Vec<u32>> {
        let index = |v: i64| u32::try_from(v).map_err(|_| anyhow::anyhow!("mapping index {v}"));
        match self.dtype.as_str() {
            "I64" => self
                .bytes
                .as_chunks::<8>()
                .0
                .iter()
                .map(|b| index(i64::from_le_bytes(*b)))
                .collect(),
            "I32" => self
                .bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| index(i64::from(i32::from_le_bytes(*b))))
                .collect(),
            other => anyhow::bail!("safetensors: `mapping` must be an integer tensor, got {other}"),
        }
    }
}

/// IEEE 754 half → single precision (subnormals, infinities and NaN included).
fn f16_to_f32(h: u16) -> f32 {
    let sign = u32::from(h >> 15) << 31;
    let exp = u32::from((h >> 10) & 0x1f);
    let frac = u32::from(h & 0x3ff);
    let bits = match (exp, frac) {
        (0, 0) => sign,
        (0, _) => {
            // Subnormal: renormalise into an f32 normal.
            let shift = frac.leading_zeros() - 21;
            sign | ((113 - shift) << 23) | ((frac << shift) & 0x3ff) << 13
        }
        (0x1f, _) => sign | 0x7f80_0000 | (frac << 13),
        _ => sign | ((exp + 112) << 23) | (frac << 13),
    };
    f32::from_bits(bits)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A safetensors file from `(name, dtype, shape, raw bytes)` tensors.
    fn file(tensors: &[(&str, &str, &[usize], Vec<u8>)]) -> Vec<u8> {
        let mut header = serde_json::Map::new();
        let mut data = Vec::new();
        for (name, dtype, shape, bytes) in tensors {
            let start = data.len();
            data.extend_from_slice(bytes);
            header.insert(
                (*name).to_string(),
                serde_json::json!({"dtype": dtype, "shape": shape, "data_offsets": [start, data.len()]}),
            );
        }
        let header = serde_json::to_vec(&header).unwrap();
        let mut out = (header.len() as u64).to_le_bytes().to_vec();
        out.extend(header);
        out.extend(data);
        out
    }

    fn f32s(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    #[test]
    fn weighted_mapped_mean_is_normalised() {
        // 3 rows of 2 dims; tokens 0,1,2 map to rows 2,0,0 with weights 1,1,3.
        let bytes = file(&[
            (
                "embeddings",
                "F32",
                &[3, 2],
                f32s(&[1.0, 0.0, 0.0, 9.0, 0.0, 1.0]),
            ),
            (
                "mapping",
                "I64",
                &[3],
                [2i64, 0, 0].iter().flat_map(|v| v.to_le_bytes()).collect(),
            ),
            (
                "weights",
                "F64",
                &[3],
                [1.0f64, 1.0, 3.0]
                    .iter()
                    .flat_map(|v| v.to_le_bytes())
                    .collect(),
            ),
        ]);
        let model = StaticModel::from_safetensors(&bytes).unwrap();
        assert_eq!((model.dimensions(), model.vocab_size()), (2, 3));
        // token 0 → row 2 [0,1]·1; token 2 → row 0 [1,0]·3 ⇒ mean [1.5,0.5].
        let v = model.embed_ids(&[0, 2]);
        let expected = [3.0 / 10f32.sqrt(), 1.0 / 10f32.sqrt()];
        assert!(
            (v[0] - expected[0]).abs() < 1e-6 && (v[1] - expected[1]).abs() < 1e-6,
            "{v:?}"
        );
        assert!(model.embed_ids(&[]).iter().all(|x| *x == 0.0));
        assert!(
            model.embed_ids(&[-1, 99]).iter().all(|x| *x == 0.0),
            "out-of-vocab ids skipped"
        );
    }

    #[test]
    fn hostile_files_are_rejected_before_any_read() {
        let ok = file(&[("embeddings", "F32", &[1, 2], f32s(&[1.0, 2.0]))]);
        assert!(StaticModel::from_safetensors(&ok).is_ok());
        assert!(
            StaticModel::from_safetensors(&ok[..4]).is_err(),
            "truncated length"
        );
        let mut huge = ok.clone();
        huge[..8].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(
            StaticModel::from_safetensors(&huge).is_err(),
            "absurd header length"
        );
        let wrong_size = file(&[("embeddings", "F32", &[2, 2], f32s(&[1.0, 2.0]))]);
        assert!(
            StaticModel::from_safetensors(&wrong_size).is_err(),
            "shape vs bytes"
        );
        let bad_map = file(&[
            ("embeddings", "F32", &[1, 2], f32s(&[1.0, 2.0])),
            ("mapping", "I64", &[1], 5i64.to_le_bytes().to_vec()),
        ]);
        assert!(
            StaticModel::from_safetensors(&bad_map).is_err(),
            "mapping out of range"
        );
        let no_embeddings = file(&[("weights", "F32", &[1], f32s(&[1.0]))]);
        assert!(StaticModel::from_safetensors(&no_embeddings).is_err());
    }

    #[test]
    fn half_precision_decodes() {
        for (bits, value) in [
            (0x3c00u16, 1.0f32),
            (0xc000, -2.0),
            (0x3800, 0.5),
            (0x0000, 0.0),
        ] {
            assert_eq!(f16_to_f32(bits), value);
        }
        assert!(
            (f16_to_f32(0x0001) - 5.960_464_5e-8).abs() < 1e-12,
            "smallest subnormal"
        );
        assert!(f16_to_f32(0x7c00).is_infinite());
    }
}
