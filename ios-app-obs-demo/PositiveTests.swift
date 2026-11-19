// XCTest requests the same bounded app-owned ingress as the ordinary UI.
extension MadInferSmokeTests {
    private func runAppScenario(_ variant: UInt32) {
        let done = expectation(description: "App-owned scenario closed")
        DispatchQueue.main.async {
            let accepted = MIDemoSubmit(variant) { status in
                XCTAssertTrue(Thread.isMainThread)
                XCTAssertEqual(status, 0)
                done.fulfill()
            }
            XCTAssertTrue(accepted)
            if !accepted { done.fulfill() }
        }
        wait(for: [done], timeout: 45)
    }
    func testNativeSyntheticAProducesAAA() { runAppScenario(0) }
    func testNativeSyntheticBProducesBBB() { runAppScenario(1) }
}
