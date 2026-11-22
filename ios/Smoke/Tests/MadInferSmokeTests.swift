import XCTest
import MadInfer

final class MadInferSmokeTests: XCTestCase {
    private func runNativeCheck(_ check: @escaping () -> String?, file: StaticString = #filePath, line: UInt = #line) {
        let finished = expectation(description: "Native C ABI check completed")
        DispatchQueue.global(qos: .userInitiated).async {
            if let failure = check() { XCTFail(failure, file: file, line: line) }
            finished.fulfill()
        }
        wait(for: [finished], timeout: 30)
    }
    func testABIVersionTwo() { runNativeCheck { MadInferCheckABI() } }
    func testNativeMissingFileIs4003NotFeatureDisabled() { runNativeCheck { MadInferCheckMissingNativeFile() } }
    func testNativeSelectionAndCorruptNonModelFailure() { runNativeCheck { MadInferCheckCorruptNativeFile() } }
}
