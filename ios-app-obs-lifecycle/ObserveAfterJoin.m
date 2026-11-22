    // The pinned helper has already joined and verified both different request
    // terminals, no partial/retry payload and no started/delta/complete.
    const mi_demo_cancellation_record record = {
        first.failedCount == 1 ? MI_DEMO_EXPLICIT_CANCELLATION : MI_DEMO_NO_CANCELLATION,
        second.failedCount == 1 ? MI_DEMO_DESTROY_CANCELLATION : MI_DEMO_NO_CANCELLATION,
    };
    NSString *wal = [NSTemporaryDirectory() stringByAppendingPathComponent:[[NSUUID UUID] UUIDString]];
    NSError *directoryError = nil;
    if (![[NSFileManager defaultManager] createDirectoryAtPath:wal withIntermediateDirectories:NO
                                                    attributes:nil error:&directoryError] ||
        !madinfer_test_obs_lifecycle_after_join(wal.fileSystemRepresentation, &record))
        return @"Optional cancellation observation admission/drain mismatch";
    // As with A/B, never remove a directory whose observer may not have joined.
    if (![[NSFileManager defaultManager] removeItemAtPath:wal error:&directoryError])
        return @"Optional cancellation observation cleanup failed";
