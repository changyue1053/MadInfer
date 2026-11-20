extern "C" int32_t madinfer_test_obs_after_join(const char *wal_dir,
    uint64_t started, uint64_t completed, uint64_t failed) {
  // Preserve the pinned A/B acceptance contract unchanged.
  if (!wal_dir || started != 1 || completed != 1 || failed != 0) return 0;
  return emit_after_join(wal_dir, 1, 2);
}

extern "C" int32_t madinfer_test_obs_lifecycle_after_join(const char *wal_dir,
    const mi_demo_cancellation_record *record) {
  if (!wal_dir || !record || record->explicit_cancel != MI_DEMO_EXPLICIT_CANCELLATION ||
      record->destroy_cancel != MI_DEMO_DESTROY_CANCELLATION) return 0;
  return emit_after_join(wal_dir, MI_DEMO_EXPLICIT_CANCELLATION, MI_DEMO_DESTROY_CANCELLATION);
}
