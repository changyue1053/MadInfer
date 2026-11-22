#include "Adapter.h"
#include <madobs/runtime.h>
#include <atomic>
#include <new>

namespace {
// Test-local identifier, NOT a global SDK registry assignment.
constexpr uint32_t test_sdk = 0x4d494601;
struct State {
  madobs_event_rule_v1 rules[2]{};
  madobs_privacy_policy_v1 policy{};
  std::atomic<unsigned> batches{0}, rejected{0};
};
void discard(void *ctx, uint8_t, uint64_t, const uint8_t *bytes, size_t size,
             madobs_send_done_fn done, void *done_ctx) {
  auto &s = *static_cast<State *>(ctx);
  const auto attempt = s.batches.fetch_add(1);
  const bool safe = attempt < 16 && size <= 1024 * 1024 &&
      madobs_check_private_batch(bytes, size, &s.policy) == MADOBS_PRIVATE_BATCH_OK;
  if (!safe) s.rejected.fetch_add(1);
  // No storage, logging, endpoint or lifecycle call. Exactly one completion.
  done(done_ctx, safe ? 1 : 0);
}
}
extern "C" int32_t madinfer_test_obs_after_join(const char *wal_dir,
    uint64_t started, uint64_t completed, uint64_t failed) {
  // Reject duplicates, failures and absent observations before any runtime IO.
  if (!wal_dir || started != 1 || completed != 1 || failed != 0) return 0;
  auto *s = new (std::nothrow) State;
  if (!s) return 0;
  for (unsigned i = 0; i < 2; ++i) {
    s->rules[i].sdk_id = test_sdk;
    s->rules[i].sdk_version = 1;
    s->rules[i].event_id = i + 1; // actual STARTED, then COMPLETED
    s->rules[i].max_severity = 1;
  }
  s->policy = {sizeof(s->policy), MADOBS_ABI_VERSION, 2, s->rules};
  if (!madobs_privacy_policy_valid(&s->policy)) { delete s; return 0; }
  const auto runtime = madobs_create(wal_dir, discard, s);
  if (!runtime) { delete s; return 0; }
  bool ok = madobs_start(runtime) == MADOBS_OK;
  if (ok) {
    for (unsigned i = 0; i < 2; ++i) {
      madobs_event_v1 event{}, clean{};
      event.struct_size = sizeof(event); event.abi_version = MADOBS_ABI_VERSION;
      event.sdk_id = test_sdk; event.sdk_version = 1;
      event.event_id = i + 1; event.severity = 1;
      // Zero attributes, timestamp, strings, request IDs and token data.
      const bool safe = madobs_sanitize_event(&s->policy, &event, &clean) == MADOBS_EMITTED;
      if (!safe || madobs_emit_generic_event(runtime, &clean) != MADOBS_OK) ok = false;
    }
  }
  // Inference has already joined, and both observation emits are synchronous.
  // stop joins runtime consumers before examining counters or releasing ctx.
  if (madobs_stop(runtime) != MADOBS_OK) return 0; // retain ctx on failed join
  madobs_generic_stats_v1 stats{};
  ok = madobs_get_generic_stats(runtime, &stats) == MADOBS_OK && ok &&
       stats.enqueued == 2 && stats.drained == 2 &&
       stats.queue_dropped == 0 && stats.wal_dropped == 0 &&
       s->batches.load() > 0 && s->batches.load() <= 16 && !s->rejected.load();
  if (madobs_destroy(runtime) != MADOBS_OK) return 0; // retain ctx on failure
  delete s;
  return ok ? 1 : 0;
}
