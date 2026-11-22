# Installed iOS app-owned pending cancellation with optional observation

Base: public app-owned `a6b0e579153053b4e5a8fb3586984a8c8ea4c597`,
tree `12e61b176a683a598d13814f9c0e7bf20469b1a6`. The native cancellation
input `Fixture.m` is byte-for-byte `ios/Lifecycle/Checks.m` from the same
repository's sibling checkpoint `e621a782db03cbcc56be5e4a9877be1385902eea`,
tree `2a43a3ae0f3da78e4af5f080110e048457c6e167`, Git blob
`e95856a72e6b68c0b8f52a5d6b823c3756171d4a`. Both share the exact SDK
core and original native fixture code. The existing repository license
applies (same unchanged license blob in both public inputs); no license
grant or new third-party source is introduced.

The original optional OFF/ON package producers, App/XCTest build graph,
same generated A/B resources and explicit receipt/artifact binding are
unchanged. A deterministic source overlay only adjusts the disposable
consumer. The same app-owned single-slot ingress accepts fixed variant 2,
which uses bundled synthetic A. No extra UI or input paths are added.
The private app worker runs the pinned joined-destruction case with two
distinct pending internal host authorization requests. No host grant, cloud
transport or inference fallback. One owner explicitly cancels the first
request and destroys the native core while the second is pending; each
must produce exactly one 3008 cancellation for its own request, with no
started/delta/complete, partial text, retry or extra callback.

ON uses the real unchanged pinned installed static MadObs package. After
native owner has joined the core and validated both terminal outcomes, the
App passes only the two fixed typed outcomes (IDs 3 explicit cancellation,
4 pending destruction) to the app-owned optional adapter. There are no
attributes, IDs, times, request/model text, paths as event fields or external
export. The shared local-discard adapter validates the bounded status profile,
requires two admitted/drained events with zero queue/WAL drops, accepts only
private policy-checked batches, and joins/destroys before app completion.
Its old A/B status profile (IDs 1 started, 2 completed) remains unchanged.
OFF executes the same native lifecycle assertions with neither MadObs
checkout nor archive/adapter. This does not exercise running MadObs during
native teardown, fleet delivery, arbitrary cancellation/host callbacks,
or production device runtime.

The same independently checked and selected app-hosted XCTest receipt is
launched by the existing unchanged qualifier. Test code calls only the
sample ingress for this scenario; actual product checks reject an XCTest
driver/Obs link and require the expected ON/OFF app symbols. Each branch
job requires the previous five exact named native tests plus exactly
`testInstalledNativePendingCancelAndDestroy`, zero failures and skips.
The source bundle retains the existing tool/dependency pins with no added
workflow or automated dependency acquisition on main. See
[qualification](../docs/verification/QUALIFIED_SOURCE.md) for historical evidence
and the explicit local optional setup. No checked-in or uploaded
binary/test artifacts are introduced.

Portable checks: `python3 -B ios-app-obs-lifecycle/test_overlay.py`. They
prove the transform, exact named result policy, typed input shape, generated
source/graph ownership and precise reuse of the existing launcher, but do
not establish Apple compile/link or simulator behavior.
