// Appended to the generated installed-framework consumer only.
@interface PendingHost : SmokeState
@property(nonatomic,strong) dispatch_semaphore_t consent;
@property(nonatomic) uint64_t consentHandle;
@property(nonatomic) BOOL badConsent;
@end
@implementation PendingHost
@end

@interface PendingSink : SmokeState
@property(nonatomic) NSUInteger startedCount;
@property(nonatomic) uint64_t failedRequest;
@property(nonatomic) BOOL exactFailure;
@end
@implementation PendingSink
@end

static BOOL lifecycle_equal(madinfer_str actual, const char *wanted) {
    return actual.ptr && actual.len==strlen(wanted) && memcmp(actual.ptr,wanted,actual.len)==0;
}
static void lifecycle_authorize(void *raw, uint64_t handle, const madinfer_consent *event) {
    PendingHost *host=(__bridge PendingHost *)raw;
    [host.lock lock];
    host.authorizationCount++;
    host.consentHandle=handle;
    host.badConsent |= !handle || !event || event->privacy!=MADINFER_PRIVACY_INTERNAL ||
        !lifecycle_equal(event->endpoint_id,"cloud.enterprise") || !lifecycle_equal(event->capability,"summarize");
    [host.lock unlock];
    dispatch_semaphore_signal(host.consent);
    // Never authorize, record payload, or wait in a callback.
}
static void lifecycle_started(void *raw, const madinfer_started *event) {
    (void)event;
    PendingSink *sink=(__bridge PendingSink *)raw;
    [sink.lock lock]; sink.startedCount++; [sink.lock unlock];
}
static void lifecycle_failed(void *raw, const madinfer_failed *event) {
    PendingSink *sink=(__bridge PendingSink *)raw;
    [sink.lock lock];
    sink.failedCount++;
    sink.failedRequest=event?event->request:0;
    sink.exactFailure=event && event->code==3008 && lifecycle_equal(event->wire,"cancelled") &&
        !event->has_partial_text && !event->partial_text.len && !event->has_retry_after_ms;
    [sink.lock unlock];
    dispatch_semaphore_signal(sink.terminal);
}
static BOOL lifecycle_exact(PendingSink *sink, uint64_t request, BOOL cancelled) {
    [sink.lock lock];
    BOOL ok=!sink.startedCount && !sink.deltaCount && !sink.completeCount &&
        (cancelled ? (sink.failedCount==1 && sink.failedRequest==request && sink.exactFailure)
                   : sink.failedCount==0);
    [sink.lock unlock];
    return ok;
}
static uint64_t lifecycle_submit(madinfer_core_handle core, PendingSink *sink, int32_t *error) {
    madinfer_message message={.role=MADINFER_ROLE_USER,.content=text("A")};
    madinfer_request request={.capability=text("summarize"),.business=7,
        .privacy=MADINFER_PRIVACY_INTERNAL,.qos=MADINFER_QOS_ASYNC,.deadline_ms=15000,
        .messages=&message,.messages_len=1,.options={.has_max_tokens=1,.max_tokens=3},
        .output_kind=MADINFER_OUTPUT_TEXT};
    madinfer_sink_vtable vtable={.user_data=(__bridge void *)sink,.on_started=lifecycle_started,
        .on_delta=delta,.on_complete=complete,.on_failed=lifecycle_failed};
    return madinfer_stream_start(core,&request,&vtable,error);
}
NSString *MadInferCheckLifecycle(NSString *path) {
    if (!path || ![[NSFileManager defaultManager] fileExistsAtPath:path]) return @"Required generated GGUF missing";
    PendingHost * __attribute__((objc_precise_lifetime)) host=[PendingHost new];
    PendingSink * __attribute__((objc_precise_lifetime)) first=[PendingSink new];
    PendingSink * __attribute__((objc_precise_lifetime)) second=[PendingSink new];
    host.consent=dispatch_semaphore_create(0);
    madinfer_host_vtable callbacks={.user_data=(__bridge void *)host,.now_ms=now_ms,.kv_get=kv_get,
        .kv_put=kv_put,.telemetry_send=telemetry,.authorize=lifecycle_authorize,
        .verify_ed25519=reject_signature,.on_worker_started=worker_started};
    madinfer_str capability=text("summarize");
    madinfer_trusted_key key={text("smoke-reject-only"),
        text("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a")};
    madinfer_core_options options={.instance_name=text("ios-native-lifecycle"),.tier=MADINFER_TIER_FLAGSHIP,
        .os=MADINFER_OS_IOS,.frame_ms=20,.sampling_bp=0,.capabilities=&capability,.capabilities_len=1,
        .has_consent_endpoint=1,.consent_endpoint=text("cloud.enterprise"),.trusted_keys=&key,.trusted_keys_len=1};
    int32_t error=-1;
    madinfer_core_handle core=madinfer_core_create_with_backend(&callbacks,&options,1,text(path.UTF8String),&error);
    if (!core) return @"Native lifecycle creation failed";
    NSString *failure=nil;
    uint64_t firstID=0,secondID=0,firstConsent=0;
    uint32_t kind=UINT32_MAX;
    const int64_t wait=4*NSEC_PER_SEC;
    do {
        if (error || madinfer_backend_kind(core,&kind) || kind!=1) { failure=@"Native lifecycle backend"; break; }
        firstID=lifecycle_submit(core,first,&error);
        if (!firstID || error || firstID==UINT64_MAX ||
            dispatch_semaphore_wait(host.consent,dispatch_time(DISPATCH_TIME_NOW,wait)) ||
            !lifecycle_exact(first,firstID,NO)) { failure=@"First request not pending"; break; }
        [host.lock lock];
        BOOL valid=host.authorizationCount==1 && !host.badConsent;
        firstConsent=host.consentHandle;
        [host.lock unlock];
        if (!valid) { failure=@"First consent identity"; break; }
        if (madinfer_stream_cancel(core,firstID) ||
            dispatch_semaphore_wait(first.terminal,dispatch_time(DISPATCH_TIME_NOW,wait)) ||
            !lifecycle_exact(first,firstID,YES) ||
            madinfer_stream_cancel(core,firstID) || madinfer_stream_cancel(core,UINT64_MAX)) {
            failure=@"Cancel terminal contract"; break;
        }
        secondID=lifecycle_submit(core,second,&error);
        if (!secondID || secondID==firstID || secondID==UINT64_MAX || error ||
            dispatch_semaphore_wait(host.consent,dispatch_time(DISPATCH_TIME_NOW,wait)) ||
            !lifecycle_exact(second,secondID,NO)) { failure=@"Second request not pending"; break; }
        [host.lock lock];
        valid=host.authorizationCount==2 && !host.badConsent && host.consentHandle!=firstConsent;
        [host.lock unlock];
        if (!valid) { failure=@"Second consent identity"; break; }
    } while (0);
    // The owner, not a callback, joins all sources before releasing contexts.
    // XCTest/CI timeouts bound the test if the SDK fails to return from destroy.
    madinfer_core_destroy(core);
    if (failure) return failure;
    if (host.authorizationCount!=2 || host.badConsent ||
        !lifecycle_exact(first,firstID,YES) || !lifecycle_exact(second,secondID,YES) ||
        dispatch_semaphore_wait(second.terminal,DISPATCH_TIME_NOW) ||
        !dispatch_semaphore_wait(second.terminal,DISPATCH_TIME_NOW) ||
        !dispatch_semaphore_wait(first.terminal,DISPATCH_TIME_NOW) ||
        !dispatch_semaphore_wait(host.consent,DISPATCH_TIME_NOW))
        return @"Destroy did not join exact terminal callbacks";
    return nil;
}
