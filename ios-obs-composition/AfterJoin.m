        // Optional consumer-only composition. Existing output assertions above
        // must pass first; all inference callbacks have already been joined.
        if (!failure) {
            NSString *wal = [NSTemporaryDirectory() stringByAppendingPathComponent:[[NSUUID UUID] UUIDString]];
            NSError *directoryError = nil;
            BOOL created = [[NSFileManager defaultManager] createDirectoryAtPath:wal withIntermediateDirectories:NO attributes:nil error:&directoryError];
            if (!created || !madinfer_test_obs_after_join(wal.fileSystemRepresentation, state.startedCount, state.completeCount, state.failedCount))
                failure = @"Optional observation admission/drain mismatch";
            // Only remove this test-owned directory after successful stop/destroy.
            if (created && !failure && ![[NSFileManager defaultManager] removeItemAtPath:wal error:&directoryError])
                failure = @"Optional observation cleanup failed";
        }
