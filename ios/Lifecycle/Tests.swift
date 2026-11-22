extension MadInferSmokeTests {
    func testInstalledNativePendingCancelAndDestroy() {
        let path = Bundle(for: MadInferSmokeTests.self).path(forResource: "synthetic-A", ofType: "gguf")
        runNativeCheck { MadInferCheckLifecycle(path) }
    }
}
