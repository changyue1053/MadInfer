// Appended to a generated copy of the independent public-ABI consumer only.
@interface PositiveState : SmokeState
@property(nonatomic, strong) NSMutableData *pieces;
@property(nonatomic, strong) NSData *finalText;
@property(nonatomic, strong) NSData *finalOutput;
@property(nonatomic) NSUInteger startedCount;
@property(nonatomic) BOOL invalidOrder;
@property(nonatomic) uint32_t generated;
@property(nonatomic) uint32_t prompt;
@property(nonatomic) uint8_t outputKind;
@property(nonatomic) uint8_t fallbackSteps;
@end
@implementation PositiveState
@end
static void positive_started(void *raw, const madinfer_started *event) {
    PositiveState *s = (__bridge PositiveState *)raw;
    [s.lock lock];
    if (event->source != MADINFER_SOURCE_BUNDLED || s.startedCount || s.deltaCount || s.completeCount || s.failedCount) s.invalidOrder = YES;
    s.startedCount++;
    [s.lock unlock];
}
static void positive_delta(void *raw, const madinfer_delta *event) {
    PositiveState *s = (__bridge PositiveState *)raw;
    [s.lock lock];
    if (s.startedCount != 1 || s.completeCount || s.failedCount || event->seq != s.deltaCount || event->has_structured_patch) s.invalidOrder = YES;
    s.deltaCount++;
    // Copy borrowed bytes before returning to the C ABI.
    if (event->text.len > 3 || s.pieces.length + event->text.len > 3 || (!event->text.ptr && event->text.len)) s.invalidOrder = YES;
    else [s.pieces appendBytes:event->text.ptr length:event->text.len];
    [s.lock unlock];
}
static void positive_complete(void *raw, const madinfer_complete *event) {
    PositiveState *s = (__bridge PositiveState *)raw;
    [s.lock lock];
    if (event->stats.has_source != 1 || event->stats.source != MADINFER_SOURCE_BUNDLED || s.startedCount != 1 || s.completeCount || s.failedCount) s.invalidOrder = YES;
    s.completeCount++;
    if (event->text.len != 3 || event->output.len != 3 || !event->text.ptr || !event->output.ptr) s.invalidOrder = YES;
    else {
        s.finalText = [NSData dataWithBytes:event->text.ptr length:event->text.len];
        s.finalOutput = [NSData dataWithBytes:event->output.ptr length:event->output.len];
    }
    s.generated = event->stats.gen_tokens; s.prompt = event->stats.prompt_tokens;
    s.outputKind = event->output_kind; s.fallbackSteps = event->stats.fallback_steps;
    [s.lock unlock];
    dispatch_semaphore_signal(s.terminal);
}
NSString *MadInferCheckPositive(NSString *path, NSString *expected) {
    @autoreleasepool {
        if (!path || ![[NSFileManager defaultManager] fileExistsAtPath:path]) return @"Required generated GGUF missing";
        PositiveState *state = [PositiveState new]; state.pieces = [NSMutableData new];
        int32_t error = -1;
        madinfer_core_handle core = create_native(state, path, &error);
        if (!core || error != 0) {
            if (core) madinfer_core_destroy(core);
            return @"Native core creation failed";
        }
        NSString *failure = nil;
        uint32_t kind = UINT32_MAX;
        if (madinfer_backend_kind(core, &kind) != 0 || kind != 1) failure = @"Not the native backend";
        else {
            madinfer_message message = {.role = MADINFER_ROLE_USER, .content = text("A")};
            madinfer_request request = {
                .capability = text("summarize"), .business = 7,
                .privacy = MADINFER_PRIVACY_PUBLIC, .qos = MADINFER_QOS_INTERACTIVE,
                .deadline_ms = 15000, .messages = &message, .messages_len = 1,
                .options = {.has_max_tokens = 1, .max_tokens = 3},
                .output_kind = MADINFER_OUTPUT_TEXT
            };
            madinfer_sink_vtable sink = {.user_data = (__bridge void *)state,
                .on_started = positive_started, .on_delta = positive_delta,
                .on_complete = positive_complete, .on_failed = failed};
            uint64_t stream = madinfer_stream_start(core, &request, &sink, &error);
            if (!stream || error != 0) failure = @"Positive stream admission failed";
            else if (dispatch_semaphore_wait(state.terminal, dispatch_time(DISPATCH_TIME_NOW, 20 * NSEC_PER_SEC))) failure = @"Positive native decode timed out";
        }
        // Owner thread joins callbacks before inspecting state or releasing resources.
        madinfer_core_destroy(core);
        NSData *bytes = [expected dataUsingEncoding:NSUTF8StringEncoding];
        if (!failure && (state.invalidOrder || state.startedCount != 1 || state.completeCount != 1 || state.failedCount || !state.deltaCount || state.authorizationCount || state.generated != 3 || state.prompt != 2 || state.outputKind != MADINFER_OUTPUT_TEXT || state.fallbackSteps || ![state.pieces isEqualToData:bytes] || ![state.finalText isEqualToData:bytes] || ![state.finalOutput isEqualToData:bytes])) failure = @"Positive native output/order/stats mismatch";
        return failure;
    }
}
