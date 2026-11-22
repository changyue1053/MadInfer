#import <Foundation/Foundation.h>
// Each helper returns nil on success, or an explanatory failure. No raw handles escape.
FOUNDATION_EXPORT NSString * _Nullable MadInferCheckABI(void);
FOUNDATION_EXPORT NSString * _Nullable MadInferCheckMissingNativeFile(void);
FOUNDATION_EXPORT NSString * _Nullable MadInferCheckCorruptNativeFile(void);
