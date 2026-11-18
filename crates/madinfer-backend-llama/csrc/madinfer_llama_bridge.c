// madinfer_llama_bridge —— 见同名头文件。函数名与 llama.h（third_party/llama.cpp@b10883）实名一一对应，升 tag 时只改本文件。
#include "madinfer_llama_bridge.h"

#include <stdlib.h>
#include <string.h>

#include "llama.h"

#ifndef MADINFER_LLAMA_BUILD_NUMBER
#error "MADINFER_LLAMA_BUILD_NUMBER must be set by csrc/CMakeLists.txt"
#endif

struct madinfer_llama_ctx {
    struct llama_context * ctx;
    struct llama_sampler * sampler;   // 采样链：只挂 greedy
};

static void madinfer_llama_log_drop(enum ggml_log_level level, const char * text, void * user_data) {
    (void) level;
    (void) text;
    (void) user_data;
}

static const struct llama_model * as_model(const madinfer_llama_model * model) {
    return (const struct llama_model *) model;
}

static const struct llama_vocab * vocab_of(const madinfer_llama_model * model) {
    return llama_model_get_vocab(as_model(model));
}

int32_t madinfer_llama_build_number(void) {
    return (int32_t) MADINFER_LLAMA_BUILD_NUMBER;
}

void madinfer_llama_backend_init(void) {
    llama_log_set(madinfer_llama_log_drop, NULL);   // 同时接管 ggml 日志
    llama_backend_init();
}

madinfer_llama_model * madinfer_llama_model_load(const char * path, int use_mmap) {
    struct llama_model_params params = llama_model_default_params();
    params.n_gpu_layers       = 0;
    params.load_mode          = use_mmap ? LLAMA_LOAD_MODE_MMAP : LLAMA_LOAD_MODE_NONE;
    params.progress_callback  = NULL;
    params.vocab_only         = false;
    params.check_tensors      = false;
    return (madinfer_llama_model *) llama_model_load_from_file(path, params);
}

void madinfer_llama_model_free(madinfer_llama_model * model) {
    llama_model_free((struct llama_model *) model);
}

int32_t madinfer_llama_model_n_ctx_train(const madinfer_llama_model * model) {
    return llama_model_n_ctx_train(as_model(model));
}

int32_t madinfer_llama_model_n_vocab(const madinfer_llama_model * model) {
    return llama_vocab_n_tokens(vocab_of(model));
}

int32_t madinfer_llama_model_n_layer(const madinfer_llama_model * model) {
    return llama_model_n_layer(as_model(model));
}

int32_t madinfer_llama_model_n_embd(const madinfer_llama_model * model) {
    return llama_model_n_embd(as_model(model));
}

int32_t madinfer_llama_model_n_head(const madinfer_llama_model * model) {
    return llama_model_n_head(as_model(model));
}

int32_t madinfer_llama_model_n_head_kv(const madinfer_llama_model * model) {
    return llama_model_n_head_kv(as_model(model));
}

uint64_t madinfer_llama_model_n_params(const madinfer_llama_model * model) {
    return llama_model_n_params(as_model(model));
}

uint64_t madinfer_llama_model_size(const madinfer_llama_model * model) {
    return llama_model_size(as_model(model));
}

int32_t madinfer_llama_model_desc(const madinfer_llama_model * model, char * buf, int32_t cap) {
    if (cap <= 0) {
        return 0;
    }
    return llama_model_desc(as_model(model), buf, (size_t) cap);
}

int32_t madinfer_llama_tokenize(const madinfer_llama_model * model, const char * text, int32_t text_len,
                               int32_t * out, int32_t out_cap, int add_special) {
    return llama_tokenize(vocab_of(model), text, text_len, (llama_token *) out, out_cap, add_special != 0,
                          /* parse_special */ false);
}

int32_t madinfer_llama_token_to_piece(const madinfer_llama_model * model, int32_t token, char * buf, int32_t cap) {
    return llama_token_to_piece(vocab_of(model), (llama_token) token, buf, cap, /* lstrip */ 0, /* special */ false);
}

int madinfer_llama_token_is_eog(const madinfer_llama_model * model, int32_t token) {
    return llama_vocab_is_eog(vocab_of(model), (llama_token) token) ? 1 : 0;
}

madinfer_llama_ctx * madinfer_llama_ctx_new(const madinfer_llama_model * model, uint32_t n_ctx, uint32_t n_batch,
                                          int32_t n_threads) {
    struct llama_context_params params = llama_context_default_params();
    params.n_ctx           = n_ctx;
    params.n_batch         = n_batch;
    params.n_ubatch        = n_batch;
    params.n_seq_max       = 1;
    params.n_threads       = n_threads;
    params.n_threads_batch = n_threads;
    params.flash_attn_type = LLAMA_FLASH_ATTN_TYPE_DISABLED;   // 固定一条 CPU 注意力路径，金样不随自动选择漂
    params.embeddings      = false;
    params.no_perf         = true;

    struct llama_context * ctx = llama_init_from_model((struct llama_model *) model, params);
    if (ctx == NULL) {
        return NULL;
    }
    struct llama_sampler_chain_params chain = llama_sampler_chain_default_params();
    chain.no_perf = true;
    struct llama_sampler * sampler = llama_sampler_chain_init(chain);
    if (sampler == NULL) {
        llama_free(ctx);
        return NULL;
    }
    llama_sampler_chain_add(sampler, llama_sampler_init_greedy());

    madinfer_llama_ctx * out = (madinfer_llama_ctx *) calloc(1, sizeof(*out));
    if (out == NULL) {
        llama_sampler_free(sampler);
        llama_free(ctx);
        return NULL;
    }
    out->ctx     = ctx;
    out->sampler = sampler;
    return out;
}

void madinfer_llama_ctx_free(madinfer_llama_ctx * ctx) {
    if (ctx == NULL) {
        return;
    }
    llama_sampler_free(ctx->sampler);
    llama_free(ctx->ctx);
    free(ctx);
}

uint32_t madinfer_llama_ctx_n_ctx(const madinfer_llama_ctx * ctx) {
    return llama_n_ctx(ctx->ctx);
}

void madinfer_llama_ctx_clear(madinfer_llama_ctx * ctx) {
    llama_memory_clear(llama_get_memory(ctx->ctx), /* data */ true);
    llama_sampler_reset(ctx->sampler);
}

int32_t madinfer_llama_decode(madinfer_llama_ctx * ctx, int32_t * tokens, int32_t n_tokens) {
    struct llama_batch batch = llama_batch_get_one((llama_token *) tokens, n_tokens);
    return llama_decode(ctx->ctx, batch);
}

int32_t madinfer_llama_sample_greedy(madinfer_llama_ctx * ctx) {
    return (int32_t) llama_sampler_sample(ctx->sampler, ctx->ctx, /* idx */ -1);
}
