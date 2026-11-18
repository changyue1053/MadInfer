//! testkit 六条后端契约对确定性仿真核实例化：缺省参数与并发 3 两种形态（宏每条用例各构一次后端）。

use madinfer_backend_llama::{LlamaBackend, LlamaOptions};
use madinfer_testkit::backend_contract_tests;

backend_contract_tests!(llama_emulated_default => LlamaBackend::new(LlamaOptions::default()));

backend_contract_tests!(llama_emulated_concurrency_3 => LlamaBackend::new(LlamaOptions {
    concurrency: 3,
    ..LlamaOptions::default()
}));
