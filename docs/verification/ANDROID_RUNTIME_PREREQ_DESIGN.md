# Android runtime prerequisite observation, not runtime qualification

Base public 6f7df01f62018b92e12794179857352684d30030 has passed unsigned
NativeActivity APK packaging only. This proposal does not change any engine,
architecture, packaging or model. It adds a separate ubuntu-24.04 observation
workflow, exact-SHA/clean-checkout guard and fixed-category Python probe.

The probe checks default non-root UID, Linux x86_64, /dev/kvm character-device
existence and an actual O_RDWR open followed immediately by close. No ioctl,
VM creation, permission or group change occurs. If the host/UID/open checks pass
and an emulator executable already exists, only its bounded 20-second
-accel-check is run with stdout/stderr discarded. It never invokes sdkmanager,
avdmanager or adb; their executable presence is recorded only. SDK license-file
existence is explicitly not acceptance proof. An existing API35 default x86_64
revision2 image is checked by metadata only, not declared checksum-verified.
Existing emulator revision37.2.13 is categorized separately. No installation,
image download, sudo/udev/chmod, signing key, APK, boot, ADB or external account.

A successful probe job means observations were collected, not prerequisites
were met. Every field is a fixed code, ending with observation-only/no-runtime/
no-license-approval. Missing/denied resources are useful evidence, not skipped
runtime tests. Generic source checks remain enabled; the native pretrained
model-download job excludes this branch/head. No logs or assets are uploaded.

## Read-only findings and unresolved conditions

GitHub documents Linux Android acceleration, but its official enabling example
uses privileged udev changes. Consequently default UID access cannot be assumed:
https://docs.github.com/en/actions/reference/runners/github-hosted-runners
https://github.blog/changelog/2024-04-02-github-actions-hardware-accelerated-android-virtualization-now-available/

The official image catalog offers system-images;android-35;default;x86_64,
revision2, archive x86_64-35_r02.zip, size782404023, published SHA1
2d857d170c0d1b827149565da34b3383e5306f7f. This is metadata, not a downloaded or
verified image. The official stable emulator metadata consulted was37.2.13,
Linux archive emulator-linux_x64-16740729.zip, size349658551, published SHA1
f41f9e31eb5ca64926603c03265c50bbc0ff4172. No payload SHA256 was acquired.
https://dl.google.com/android/repository/sys-img/android/sys-img2-1.xml
https://dl.google.com/android/repository/repository2-1.xml

Both refer to android-sdk-license, but equal license IDs do not prove current
terms were accepted on a future job. The runner README does not establish that
the selected emulator and image are preinstalled. Image versions and files,
license approval and permissions remain unknown until independently checked.
https://github.com/actions/runner-images/blob/main/images/ubuntu/Ubuntu2404-Readme.md

## Subsequent runtime design, not implemented or authorized by this probe

A true x86_64 Android lane cannot simply use the existing ARM64 archive: backend
build.rs hardcodes Android arm64-v8a. It would need an isolated, verified x86_64
CPU engine build from the same vendored source, keeping ARM64 claims separate.
Only if bounded existing host prerequisites are proven should a subsequent
review consider a job-local disposable signing key and a separately debuggable
APK, same first-party hash-pinned A/B fixtures, and C-ABI worker callback tests.
Require actual installed app execution with fresh job-nonce result in private
app storage, native backend identity, exact AAA/BBB, prompt/generated counts,
terminal uniqueness and zero fallback. An install/start success is insufficient.
Only fixed parsed result summaries may leave local logs; no model/key/APK/raw
log upload. No ARM translation, ARM64 runtime, physical device, 16KiB hardware
or inference-quality claim. Signing must follow zipalign and be independently
verified; no user credentials or production key would be involved.
https://developer.android.com/studio/run/emulator-acceleration
https://developer.android.com/tools/apksigner

Local tests are mocked prerequisite/probe tests only, not a hosted observation.
No hosted probe or runtime was started by this candidate. Current conclusion:
feasibility conditional, prerequisites not verified, no runtime claim.
