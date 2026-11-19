# App-owned optional observation demo: source-only candidate

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

## Not yet proven

`python3 ios-app-obs-demo/test_source.py` runs three portable source/project tests,
using explicitly fake archive bytes, never native qualification. It checks target
ownership, unchanged assertions/error source, resource movement, OFF isolation,
five-test count and bounded ingress structure. UIKit/Objective-C/Swift compilation,
app-export linkage and actual main-thread/runtime scheduling have NOT run here.

A separately reviewed hosted phase must inspect app-owned static MadObs exports,
absence of test-bundle definitions/direct Obs linkage, OFF no Obs symbols across
all artifacts, exact app-hosted xctestrun closure and both genuine A/B results.
It must not reuse the existing XCTest-owned green run as evidence for this app.
Device build/signing/execution is out of scope. No model quality claim.

## Isolated hosted orchestration candidate (not executed here)

A dedicated branch-only workflow has separate standalone-off and optional-on
jobs with the same exact original preflight, Apple/Rust prerequisites and pinned
public checkout. OFF has no MadObs acquisition. The small install/Simulator glue
is copied from the proven optional checkpoint with only the selected app preparer
or qualifier entrypoint changed; tests compare those files exactly. No original
script is changed. Framework production remains the existing unchanged builder.

The new qualifier reuses the proven platform/archive inspectors, xctestrun
parser, contained bundle selection and complete artifact snapshot helpers. It
changes ownership: the actual app executable must define MIDemoSubmit and its
private positive driver; ON app defines the actual MadObs exports with no Obs
unresolved references. Test bundle must import MIDemoSubmit and define no sample
or observation symbols or have Obs imports. Its three original error regressions
can still directly reference public MadInfer. Generated project test settings
and sources are checked to exclude direct Obs archive/source inputs. OFF checks
app, test and embedded framework isolation.

Only the generated sample app configurations set ENABLE_DEBUG_DYLIB=NO, so its
code/exports reside in the inspected executable rather than a separate Xcode
debug dylib. This does not change the framework or original application defaults.
The exact selected app/test/embedded resources are inspected and hashed before
and after test-without-building. Execution is capped at 1800 seconds within the
60-minute job; the same job-owned simulator cleanup and exact five-result parser
remain mandatory. Device build, signing and execution remain out of scope.

New local orchestration tests use mocks/synthetic symbols only. No workflow was
triggered and no artifact/native application qualification has run for this
candidate. Actual Xcode app exports, BUNDLE_LOADER resolution and app lifecycle
must pass independent review followed by exact hosted execution before any claim.
