#import "NativeChecks.h"
@import MadInfer;
#include <time.h>
#include <string.h>

@interface SmokeState : NSObject
@property(nonatomic, strong) NSLock *lock;
@property(nonatomic, strong) NSMutableDictionary<NSData *, NSData *> *kv;
@property(nonatomic, strong) dispatch_semaphore_t terminal;
@property(nonatomic) NSUInteger failedCount;
@property(nonatomic) NSUInteger completeCount;
@property(nonatomic) NSUInteger deltaCount;
@property(nonatomic) NSUInteger authorizationCount;
@property(nonatomic) uint16_t failureCode;
@end
@implementation SmokeState
- (instancetype)init {
    if ((self = [super init])) {
        _lock = [NSLock new];
        _kv = [NSMutableDictionary new];
        _terminal = dispatch_semaphore_create(0);
    }
    return self;
}
@end

static madinfer_str text(const char *s) {
    return (madinfer_str){(const uint8_t *)s, (uint32_t)strlen(s)};
}
static uint64_t now_ms(void *unused) {
    (void)unused;
    struct timespec time;
    clock_gettime(CLOCK_MONOTONIC, &time);
    return (uint64_t)time.tv_sec * 1000 + (uint64_t)time.tv_nsec / 1000000;
}
static int32_t kv_get(void *raw, madinfer_str key, uint8_t *out, uint32_t capacity, uint32_t *length) {
    @autoreleasepool {
        SmokeState *state = (__bridge SmokeState *)raw;
        NSData *k = [NSData dataWithBytes:key.ptr length:key.len];
        [state.lock lock];
        NSData *value = state.kv[k];
        [state.lock unlock];
        *length = (uint32_t)value.length;
        if (!value) return 0;
        NSUInteger count = MIN(value.length, (NSUInteger)capacity);
        if (count) memcpy(out, value.bytes, count);
        return 1;
    }
}
static void kv_put(void *raw, madinfer_str key, const uint8_t *value, uint32_t length) {
    @autoreleasepool {
        SmokeState *state = (__bridge SmokeState *)raw;
        NSData *k = [NSData dataWithBytes:key.ptr length:key.len];
        NSData *v = [NSData dataWithBytes:value length:length];
        [state.lock lock]; state.kv[k] = v; [state.lock unlock];
    }
}
static void telemetry(void *raw, const madinfer_telemetry_batch *batch) {
    // Intentionally no upload, no interpretation, no retained borrowed pointers.
    (void)raw; (void)batch;
}
static void authorize(void *raw, uint64_t request, const madinfer_consent *consent) {
    // No cloud endpoint is configured. Record unexpected authorization, never grant it.
    (void)request; (void)consent;
    SmokeState *state = (__bridge SmokeState *)raw;
    [state.lock lock]; state.authorizationCount++; [state.lock unlock];
}
static int32_t reject_signature(void *raw, const uint8_t *message, uint32_t message_len,
                               const uint8_t *signature, uint32_t signature_len,
                               const uint8_t *key, uint32_t key_len) {
    (void)raw; (void)message; (void)message_len; (void)signature;
    (void)signature_len; (void)key; (void)key_len;
    return 0; // Fail closed. This fixture must never trust arbitrary configuration.
}
static void worker_started(void *raw, uint8_t worker_class) { (void)raw; (void)worker_class; }

static madinfer_core_handle create_native(SmokeState *state, NSString *path, int32_t *error) {
    madinfer_host_vtable host = {
        .user_data = (__bridge void *)state, .now_ms = now_ms, .kv_get = kv_get,
        .kv_put = kv_put, .telemetry_send = telemetry, .authorize = authorize,
        .verify_ed25519 = reject_signature, .on_worker_started = worker_started
    };
    madinfer_str capability = text("summarize");
    // Public RFC 8032 test-vector public key only, no signing material.
    madinfer_trusted_key key = {
        text("smoke-reject-only"),
        text("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a")
    };
    madinfer_core_options options = {
        .instance_name = text("ios-native-binary-smoke"),
        .tier = MADINFER_TIER_FLAGSHIP, .os = MADINFER_OS_IOS,
        .frame_ms = 20, .sampling_bp = 0,
        .capabilities = &capability, .capabilities_len = 1,
        .trusted_keys = &key, .trusted_keys_len = 1
    };
    // Options and path are copied by ABI; state stays strongly held until destroy returns.
    return madinfer_core_create_with_backend(&host, &options, 1, text(path.UTF8String), error);
}
static void started(void *raw, const madinfer_started *event) { (void)raw; (void)event; }
static void delta(void *raw, const madinfer_delta *event) {
    (void)event; SmokeState *state = (__bridge SmokeState *)raw;
    [state.lock lock]; state.deltaCount++; [state.lock unlock];
}
static void complete(void *raw, const madinfer_complete *event) {
    (void)event; SmokeState *state = (__bridge SmokeState *)raw;
    [state.lock lock]; state.completeCount++; [state.lock unlock];
    dispatch_semaphore_signal(state.terminal);
}
static void failed(void *raw, const madinfer_failed *event) {
    SmokeState *state = (__bridge SmokeState *)raw;
    [state.lock lock]; state.failedCount++; state.failureCode = event->code; [state.lock unlock];
    dispatch_semaphore_signal(state.terminal);
}

NSString *MadInferCheckABI(void) {
    uint32_t version = madinfer_abi_version();
    return version == 2 ? nil : [NSString stringWithFormat:@"Expected ABI 2, got %u", version];
}
NSString *MadInferCheckMissingNativeFile(void) {
    SmokeState *state = [SmokeState new];
    NSString *path = [NSTemporaryDirectory() stringByAppendingPathComponent:NSUUID.UUID.UUIDString];
    if ([[NSFileManager defaultManager] fileExistsAtPath:path]) return @"Missing-file fixture unexpectedly exists";
    int32_t error = -1;
    madinfer_core_handle core = create_native(state, path, &error);
    if (core) {
        madinfer_core_destroy(core); // Exactly once, from owner thread, never a callback.
        return @"Missing file unexpectedly created a native core";
    }
    return error == 4003 ? nil : [NSString stringWithFormat:@"Native missing-file expected 4003, got %d (4005 means native feature absent)", error];
}
NSString *MadInferCheckCorruptNativeFile(void) {
    @autoreleasepool {
        NSFileManager *files = [NSFileManager defaultManager];
        NSString *path = [NSTemporaryDirectory() stringByAppendingPathComponent:NSUUID.UUID.UUIDString];
        NSData *invalid = [@"Deliberately not a GGUF file. No model weights." dataUsingEncoding:NSUTF8StringEncoding];
        NSError *writeError = nil;
        if (![invalid writeToFile:path options:NSDataWritingAtomic error:&writeError])
            return [NSString stringWithFormat:@"Cannot write non-model fixture: %@", writeError];
        SmokeState *state = [SmokeState new];
        int32_t error = -1;
        madinfer_core_handle core = create_native(state, path, &error);
        if (!core) {
            [files removeItemAtPath:path error:NULL];
            return [NSString stringWithFormat:@"Existing non-model file should allow lazy native core creation, got %d", error];
        }
        NSString *failure = nil;
        uint32_t kind = UINT32_MAX;
        int32_t status = madinfer_backend_kind(core, &kind);
        if (status != 0 || kind != 1) {
            failure = [NSString stringWithFormat:@"Expected native kind 1, status=%d kind=%u", status, kind];
        } else {
            madinfer_message message = {.role = MADINFER_ROLE_USER, .content = text("Smoke test")};
            madinfer_request request = {
                .capability = text("summarize"), .business = 7,
                .privacy = MADINFER_PRIVACY_PUBLIC, .qos = MADINFER_QOS_INTERACTIVE,
                .deadline_ms = 5000, .messages = &message, .messages_len = 1,
                .options = {.has_max_tokens = 1, .max_tokens = 1},
                .output_kind = MADINFER_OUTPUT_TEXT
            };
            madinfer_sink_vtable sink = {.user_data = (__bridge void *)state,
                .on_started = started, .on_delta = delta, .on_complete = complete, .on_failed = failed};
            uint64_t stream = madinfer_stream_start(core, &request, &sink, &error);
            if (!stream || error != 0) failure = [NSString stringWithFormat:@"Stream start failed: %d", error];
            else if (dispatch_semaphore_wait(state.terminal, dispatch_time(DISPATCH_TIME_NOW, 10 * NSEC_PER_SEC)) != 0)
                failure = @"Timed out waiting for corrupt-model terminal failure";
        }
        // Shutdown joins all callbacks; state and non-model file remain live through this call.
        madinfer_core_destroy(core);
        [files removeItemAtPath:path error:NULL];
        if (!failure && (state.failedCount != 1 || state.failureCode != 3005 || state.completeCount != 0 || state.deltaCount != 0))
            failure = [NSString stringWithFormat:@"Expected one asset-corrupt(3005), no inference output: failed=%lu code=%u complete=%lu delta=%lu", (unsigned long)state.failedCount, state.failureCode, (unsigned long)state.completeCount, (unsigned long)state.deltaCount];
        if (!failure && state.authorizationCount != 0) failure = @"Unexpected cloud authorization request";
        return failure;
    }
}
