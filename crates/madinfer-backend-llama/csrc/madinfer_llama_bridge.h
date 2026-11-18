// madinfer_llama_bridge —— MadInfer 侧对 llama.cpp 的最小 C 面（madai-w16 I-C-15 spike；feature `llama-sys`）。
//
// 为什么隔一层 C 而不直接在 Rust 里声明 llama.h：llama_model_params / llama_context_params / llama_batch 都按值跨 ABI，
// 字段随 tag 频繁增删（b10883 已是 load_mode / lazy_mode / n_rs_seq 形态）；手抄一处错位就是静默内存错。本文件只让
// 标量与不透明指针过边界，结构体在同一编译器、同一 llama.h 下由 C 消化——升 tag 时编译期报错，不留到运行期。
// 与 sys.rs 的 extern "C" 声明逐条对应；改任一侧必须同改另一侧。
#pragma once

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct madinfer_llama_model madinfer_llama_model;   // 不透明：即 struct llama_model
typedef struct madinfer_llama_ctx   madinfer_llama_ctx;     // 不透明：llama_context + greedy 采样链

// 链接自检：编译进桥的 llama.cpp build number（LLAMA_BUILD_NUMBER，钉版 tag b<n> 的 n）。
int32_t madinfer_llama_build_number(void);

// 进程级初始化（可重复调用；日志回调置空——llama / ggml 不写 stderr，模型元数据也不外泄）。
void madinfer_llama_backend_init(void);

// 模型：返回 NULL 表示加载失败（文件是否存在由调用方先判，以区分 missing / corrupt）。use_mmap 非 0 走 mmap。
madinfer_llama_model * madinfer_llama_model_load(const char * path, int use_mmap);
void madinfer_llama_model_free(madinfer_llama_model * model);

int32_t  madinfer_llama_model_n_ctx_train(const madinfer_llama_model * model);
int32_t  madinfer_llama_model_n_vocab    (const madinfer_llama_model * model);
int32_t  madinfer_llama_model_n_layer    (const madinfer_llama_model * model);
int32_t  madinfer_llama_model_n_embd     (const madinfer_llama_model * model);
int32_t  madinfer_llama_model_n_head     (const madinfer_llama_model * model);
int32_t  madinfer_llama_model_n_head_kv  (const madinfer_llama_model * model);
uint64_t madinfer_llama_model_n_params   (const madinfer_llama_model * model);
uint64_t madinfer_llama_model_size       (const madinfer_llama_model * model);   // 张量字节数
// 模型描述串（如 "llama 135M Q4_K - Medium"）；返回写入字节数（不含 NUL）。
int32_t  madinfer_llama_model_desc(const madinfer_llama_model * model, char * buf, int32_t cap);

// 词元化：返回词元数；缓冲不够返回负的所需数。add_special 非 0 按模型配置加 BOS。
int32_t madinfer_llama_tokenize(const madinfer_llama_model * model, const char * text, int32_t text_len,
                               int32_t * out, int32_t out_cap, int add_special);
// 词元 → 字节片（不写 NUL、不渲染特殊词元）；返回字节数；缓冲不够返回负的所需数。
int32_t madinfer_llama_token_to_piece(const madinfer_llama_model * model, int32_t token, char * buf, int32_t cap);
// 是否生成结束词元（EOS / EOT 族）。
int     madinfer_llama_token_is_eog(const madinfer_llama_model * model, int32_t token);

// 上下文：n_ctx 窗口、n_batch 单次 decode 上限、n_threads 线程数；返回 NULL 表示分配失败。
madinfer_llama_ctx * madinfer_llama_ctx_new(const madinfer_llama_model * model, uint32_t n_ctx, uint32_t n_batch,
                                          int32_t n_threads);
void     madinfer_llama_ctx_free(madinfer_llama_ctx * ctx);
uint32_t madinfer_llama_ctx_n_ctx(const madinfer_llama_ctx * ctx);
// 清 KV 与采样链（一轮开始）。
void     madinfer_llama_ctx_clear(madinfer_llama_ctx * ctx);
// 解码一批词元（seq 0，位置由 llama 跟踪，只对最后一个出 logits）。返回 llama_decode 码：
// 0 成功；1 无 KV 槽；2 被中止；-1 输入非法；< -1 致命。tokens 需可写（llama_batch 形态要求）。
int32_t  madinfer_llama_decode(madinfer_llama_ctx * ctx, int32_t * tokens, int32_t n_tokens);
// 对最后一次 decode 的 logits 做 greedy（argmax）采样；返回词元 id。
int32_t  madinfer_llama_sample_greedy(madinfer_llama_ctx * ctx);

#ifdef __cplusplus
}
#endif
