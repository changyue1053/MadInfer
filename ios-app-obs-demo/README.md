# App-owned optional observation demo

Base: public MadInfer `047d7c1943f925667f032bd0a6055dfa176e9f06`, tree
`efffa334ee37e4db1a1ae7f2dc22eb2ff0cc4913` (equivalent local parent `152058f`).
All changes are additive here. Original app/framework/SDK/default dependency,
ABI, installed-package logic, build scripts and green hosted gates are untouched.
No new repository or committed model/binary. No new workflow in this phase.

## Ownership, not a test-side facade

`prepare.py` generates a separate disposable ordinary UIKit app consumer using
the exact existing positive/resource/package preparers. Its app sources own:
- a single admitted A/B scenario on a private serial queue;
- native core, callbacks, bounded output checks and joined destruction;
- ON only: actual optional observation admission/drain/stop/destroy;
- fixed A/B model resources located by mainBundle, never arbitrary caller paths.

The UI buttons call the same `MIDemoSubmit(0/1, completion)` ingress as XCTest.
App UI displays only Running/Passed and closed/Failed/Busy. The ingress is a
sample-only app symbol, not an SDK API or an ABI commitment. It accepts no model
text, path, key or opaque telemetry bytes. Completion returns only 0 (all exact
checks passed and closed) or 1 (failed session), on the main queue.

Admission is locked before dispatch: one running item, no unbounded pending
queue. It stays busy through main-thread completion delivery. Any failure latches
admission shut for the session, including observer cleanup failure, so retained
failed context cannot accumulate through subsequent requests. Successful
completion releases admission before invoking the callback, permitting deliberate
reentry only after native/observer shutdown has finished. Background/termination
is not promised to perform cleanup; no arbitrary hung-callback cancellation
claim. This is intentionally sample-level behavior, not a production service.

## Exact reuse and proof boundaries

Rather than maintaining copied native assertions, generation reuses the original
NativeChecks/PositiveChecks and optional AfterJoin injection, changing only their
app-private names and include path. The old three error-check definitions also
exist under app-private names but are not ingress actions. XCTest retains the
original NativeChecks.m byte-for-byte and original three regressions. Its two
positive tests now request the app-owned scenarios and assert the finite result.
The app executes all original actual AAA/BBB, order, token stats, native-kind,
zero-authorization/fallback checks. No positive driver or observation adapter is
compiled into the test target. There remain exactly five test methods.

ON moves the adapter to the app source phase and moves installed include/archive
link settings to app configurations only. OFF has no adapter/archive dependency.
Both fixtures move to the app resource phase, not XCTest resources. The ordinary
UI works without XCTest. Test linkage uses the existing BUNDLE_LOADER and explicit
sample C declaration in the bridging header, not hidden Swift symbols.

`prepare.py --mode off --dest <fresh sibling consumer>` needs no MadObs input.
ON additionally takes `--installed <real relocated prefix>` and
`--removed-producer <absent producer>` exactly as the reviewed existing preparer.
It does not acquire/install/build anything or prove the supplied archive source.
That remains the responsibility of the future approved orchestration. The
relative framework location is the existing `../MadInfer.xcframework` contract.

The optional adapter still emits only two finite post-join statuses, no deltas,
text, timestamps or paths as events. It uses local discard only. Runtime internal
WAL/batch timestamps and null-provider encryption limits remain unchanged.

## Verification

The independently linked installed app-owned demo passed five exact cases in
both modes at the historical pinned iOS checkpoint; see
[qualification](../docs/verification/QUALIFIED_SOURCE.md). Its build-time
preparer and launcher are retained for local use, with no extra workflow or
installer acquisition. The standalone source and orchestration tests use only
mocked package/archive bytes and do not establish new Apple runtime evidence.
The combined harness uses these same established subjects, with a sixth
app-owned lifecycle case. No device signing or runtime claim.
