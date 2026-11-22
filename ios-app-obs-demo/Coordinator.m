#import "Demo.h"
#import "Driver.h"

BOOL MIDemoSubmit(uint32_t variant, void (^completion)(int32_t)) {
    if (variant > 1 || !completion) return NO;
    static NSLock *lock;
    static dispatch_queue_t worker;
    static BOOL busy;
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        lock = [NSLock new];
        worker = dispatch_queue_create("org.example.madinfer.demo", DISPATCH_QUEUE_SERIAL);
    });
    [lock lock];
    if (busy) { [lock unlock]; return NO; }
    busy = YES;
    [lock unlock];
    // Only one admitted work item. Completion is retained until main delivery.
    dispatch_async(worker, ^{
        @autoreleasepool {
            NSString *name = variant == 0 ? @"synthetic-A" : @"synthetic-B";
            NSString *path = [[NSBundle mainBundle] pathForResource:name ofType:@"gguf"];
            NSString *failure = MIDemoInternalCheckPositive(path, variant == 0 ? @"AAA" : @"BBB");
            int32_t status = failure == nil ? 0 : 1;
            // Never return/log native error strings, paths, tokens or model data.
            dispatch_async(dispatch_get_main_queue(), ^{
                // Failure is terminal for this sample session. A failed observer
                // stop may intentionally retain context; never accumulate more.
                [lock lock]; if (status == 0) busy = NO; [lock unlock];
                completion(status);
            });
        }
    });
    return YES;
}
