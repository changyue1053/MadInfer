//! Synthetic arithmetic qualification only: no pretrained weights or quality claim.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;

use madinfer_backend::BackendError;
use madinfer_backend_llama::{
    native_engine_available, native_engine_build_number, native_greedy_trace,
};

// Reuse the unmodified production RAII bridge in this test crate so actual
// allocated context size is observable without adding a production public API.
#[allow(dead_code)]
#[path = "../src/sys.rs"]
mod engine;

fn fixture(name: &str) -> PathBuf {
    let root = std::env::var_os("MADINFER_SYNTHETIC_DIR")
        .filter(|v| !v.is_empty())
        .expect("MADINFER_SYNTHETIC_DIR required; no skip or downloaded-model fallback");
    PathBuf::from(root).join(name)
}

fn linked() {
    assert_eq!(std::env::consts::OS, "linux");
    assert_eq!(std::env::consts::ARCH, "x86_64");
    assert!(native_engine_available());
    assert_eq!(native_engine_build_number(), 10_883);
    assert!(engine::native_engine_available());
}

fn positive(variant: &str, token: i32, text: &str) {
    linked();
    let path = fixture(&format!("synthetic-{variant}.gguf"));
    assert!(path.is_file(), "fixture missing");
    for _ in 0..2 {
        let trace = native_greedy_trace(&path, "A", 3, 1).expect("real native decode");
        assert_eq!(trace.n_vocab, 261);
        assert_eq!(trace.n_ctx_train, 128);
        assert_eq!(trace.n_params, 27_040);
        assert_eq!(trace.tensor_bytes, 108_160);
        assert_eq!(trace.kv_bytes_per_token, 128);
        assert_eq!(trace.prompt_tokens, vec![1, 259]);
        assert_eq!(trace.tokens, vec![token; 3]);
        assert_eq!(trace.text, text);
    }
}

#[test]
fn synthetic_a_real_decode() {
    positive("A", 259, "AAA");
}

#[test]
fn synthetic_b_real_decode() {
    positive("B", 260, "BBB");
}

#[test]
fn synthetic_context_and_incremental_decode() {
    linked();
    let model = Arc::new(
        engine::ModelHandle::load(&fixture("synthetic-A.gguf"), false).expect("native load"),
    );
    assert_eq!(model.n_layer(), 1);
    assert_eq!(model.n_embd(), 32);
    assert_eq!(model.n_head(), 1);
    assert_eq!(model.n_head_kv(), 1);
    assert_eq!(model.n_ctx_train(), 128);
    let mut context = engine::ContextHandle::new(&model, 128, 32, 1).expect("native context");
    // Pinned llama-context.cpp aligns cparams.n_ctx to 256, including small models.
    assert_eq!(context.n_ctx(), 256);
    let mut prompt = model.tokenize("A", true);
    assert_eq!(prompt, vec![1, 259]);
    context.decode(&mut prompt).expect("prefill");
    for _ in 0..3 {
        let token = context.sample_greedy();
        assert_eq!(token, 259);
        assert!(!model.is_eog(token));
        assert_eq!(model.token_to_piece(token), b"A");
        context.decode(&mut [token]).expect("incremental decode");
    }
    context.clear();
    context.decode(&mut prompt).expect("prefill after clear");
    assert_eq!(context.sample_greedy(), 259);
}

#[test]
fn synthetic_missing_and_corrupt_fail() {
    linked();
    let missing = fixture("must-not-exist.gguf");
    assert!(!missing.exists());
    assert_eq!(
        native_greedy_trace(&missing, "A", 3, 1).err(),
        Some(BackendError::ModelMissing)
    );
    // The runner creates these explicit invalid inputs; no test mutates valid data.
    for name in ["bad-magic.gguf", "truncated.gguf"] {
        let path = fixture(name);
        assert!(path.is_file());
        assert_eq!(
            native_greedy_trace(&path, "A", 3, 1).err(),
            Some(BackendError::ModelCorrupt)
        );
    }
}
