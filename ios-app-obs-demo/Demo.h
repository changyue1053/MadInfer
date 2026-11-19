#import <Foundation/Foundation.h>
// Sample-only app ingress. Variants 0/1 are fixed bundled fixtures, not paths.
// Completion runs on main queue after driver/optional observer cleanup.
// Result 0 is passed/closed; result 1 is failed and latches admission closed.
// False means invalid input, busy or failed session; no callback/queued work.
FOUNDATION_EXPORT BOOL MIDemoSubmit(uint32_t variant, void (^completion)(int32_t));
