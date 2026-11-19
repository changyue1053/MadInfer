// Portable mock-runtime contract test. NOT MadObs runtime or iOS execution.
#include "Adapter.h"
#include <madobs/runtime.h>
#include <cassert>
#include <cstdint>

static int scenario, calls, admitted, done_count, destroyed;
static madobs_channel_send_fn sink;
static void *sink_ctx;
extern "C" madobs_handle_t madobs_create(const char *, madobs_channel_send_fn fn, void *ctx) {
  ++calls; sink=fn; sink_ctx=ctx; return 1;
}
extern "C" madobs_status_t madobs_start(madobs_handle_t) { return MADOBS_OK; }
extern "C" madobs_status_t madobs_emit_generic_event(madobs_handle_t, const madobs_event_v1 *e) {
  ++calls;
  assert(e->event_id == static_cast<unsigned>(admitted+1));
  assert(e->sdk_id == 0x4d494601 && e->sdk_version == 1);
  assert(e->attribute_count == 0 && e->monotonic_ns == 0 && e->reserved == 0);
  for (const auto &a:e->attributes) assert(a.key == 0 && a.enum_value == 0);
  ++admitted;
  return scenario == 1 ? MADOBS_E_QUEUE_FULL : MADOBS_OK;
}
extern "C" madobs_private_batch_result madobs_check_private_batch(const uint8_t *, size_t, const madobs_privacy_policy_v1 *p) {
  assert(p->event_count == 2 && madobs_privacy_policy_valid(p));
  return scenario == 2 ? MADOBS_PRIVATE_UNSAFE_RECORD : MADOBS_PRIVATE_BATCH_OK;
}
static void done(void *, int ok) { ++done_count; assert(ok == (scenario != 2)); }
extern "C" madobs_status_t madobs_stop(madobs_handle_t) {
  const uint8_t bytes=0;
  sink(sink_ctx,0,0,&bytes,1,done,nullptr);
  return MADOBS_OK;
}
extern "C" madobs_status_t madobs_get_generic_stats(madobs_handle_t, madobs_generic_stats_v1 *s) {
  *s={2,2,0,0};
  if (scenario == 3) s->drained=1;
  if (scenario == 4) s->queue_dropped=1;
  if (scenario == 5) s->wal_dropped=1;
  return MADOBS_OK;
}
extern "C" madobs_status_t madobs_destroy(madobs_handle_t) { ++destroyed; return MADOBS_OK; }
int main() {
  assert(!madinfer_test_obs_after_join(nullptr,1,1,0));
  assert(!madinfer_test_obs_after_join("test-only",0,1,0));
  assert(!madinfer_test_obs_after_join("test-only",1,2,0));
  assert(!madinfer_test_obs_after_join("test-only",1,1,1));
  assert(calls==0);
  for (scenario=0;scenario<6;++scenario) {
    calls=admitted=done_count=destroyed=0;
    assert(madinfer_test_obs_after_join("test-only",1,1,0)==(scenario==0));
    assert(calls==3 && admitted==2 && done_count==1 && destroyed==1);
  }
}
