//! Deterministic [`Embedder`] for tests and the eval harness.
//!
//! Two modes:
//! - **Override map**: tests inject specific vectors for specific texts,
//!   giving full control over similarity relationships.
//! - **Hash fallback**: texts not in the override map get a deterministic
//!   unit-length vector derived from a hash of the text. Different texts
//!   produce different directions; same text always produces the same vector.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use anyhow::Result;
use async_trait::async_trait;

use crate::embed::Embedder;

pub struct MockEmbedder {
    dimension: usize,
    overrides: HashMap<String, Vec<f32>>,
}

impl MockEmbedder {
    pub fn new(dimension: usize) -> Self {
        Self {
            dimension,
            overrides: HashMap::new(),
        }
    }

    pub fn with_overrides(dimension: usize, overrides: HashMap<String, Vec<f32>>) -> Self {
        Self {
            dimension,
            overrides,
        }
    }

    /// Register a text → vector mapping. The vector must match the
    /// configured dimension.
    pub fn set_override(&mut self, text: impl Into<String>, vector: Vec<f32>) {
        self.overrides.insert(text.into(), vector);
    }

    fn hash_embed(&self, text: &str) -> Vec<f32> {
        let mut v: Vec<f32> = (0..self.dimension)
            .map(|i| {
                let mut hasher = std::hash::DefaultHasher::new();
                text.hash(&mut hasher);
                i.hash(&mut hasher);
                let h = hasher.finish();
                // Map to [-1, 1] range
                ((h % 20000) as f32 / 10000.0) - 1.0
            })
            .collect();
        normalize(&mut v);
        v
    }
}

fn normalize(v: &mut [f32]) {
    let mag: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if mag > 0.0 {
        for x in v.iter_mut() {
            *x /= mag;
        }
    }
}

#[async_trait]
impl Embedder for MockEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        if let Some(v) = self.overrides.get(text) {
            return Ok(v.clone());
        }
        Ok(self.hash_embed(text))
    }

    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let mut results = Vec::with_capacity(texts.len());
        for text in texts {
            results.push(self.embed(text).await?);
        }
        Ok(results)
    }

    fn dimension(&self) -> usize {
        self.dimension
    }

    fn max_input_tokens(&self) -> usize {
        16_000
    }

    fn provider_name(&self) -> &str {
        "mock"
    }

    fn model_name(&self) -> &str {
        "mock-embed-v1"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn deterministic_same_text_same_vector() {
        let e = MockEmbedder::new(8);
        let a = e.embed("hello world").await.unwrap();
        let b = e.embed("hello world").await.unwrap();
        assert_eq!(a, b);
    }

    #[tokio::test]
    async fn different_text_different_vector() {
        let e = MockEmbedder::new(8);
        let a = e.embed("hello").await.unwrap();
        let b = e.embed("goodbye").await.unwrap();
        assert_ne!(a, b);
    }

    #[tokio::test]
    async fn override_takes_precedence() {
        let mut e = MockEmbedder::new(3);
        e.set_override("special", vec![1.0, 0.0, 0.0]);
        let v = e.embed("special").await.unwrap();
        assert_eq!(v, vec![1.0, 0.0, 0.0]);
    }

    #[tokio::test]
    async fn non_override_falls_back_to_hash() {
        let mut e = MockEmbedder::new(3);
        e.set_override("special", vec![1.0, 0.0, 0.0]);
        let v = e.embed("not-special").await.unwrap();
        assert_ne!(v, vec![1.0, 0.0, 0.0]);
        assert_eq!(v.len(), 3);
    }

    #[tokio::test]
    async fn embed_batch_matches_individual() {
        let e = MockEmbedder::new(8);
        let texts = &["alpha", "beta", "gamma"];
        let batch = e.embed_batch(texts).await.unwrap();

        for (i, text) in texts.iter().enumerate() {
            let single = e.embed(text).await.unwrap();
            assert_eq!(batch[i], single);
        }
    }

    #[tokio::test]
    async fn hash_vectors_are_unit_length() {
        let e = MockEmbedder::new(128);
        let v = e.embed("test normalization").await.unwrap();
        let mag: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((mag - 1.0).abs() < 1e-5, "expected unit length, got {mag}");
    }

    #[tokio::test]
    async fn dimension_matches_config() {
        let e = MockEmbedder::new(256);
        assert_eq!(e.dimension(), 256);
        let v = e.embed("any text").await.unwrap();
        assert_eq!(v.len(), 256);
    }

    #[test]
    fn accessor_methods() {
        let e = MockEmbedder::new(3);
        assert_eq!(e.provider_name(), "mock");
        assert_eq!(e.model_name(), "mock-embed-v1");
        assert_eq!(e.max_input_tokens(), 16_000);
    }
}
