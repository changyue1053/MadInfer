// Appended to a generated copy of MadInferSmokeTests.swift.
extension MadInferSmokeTests {
    func testNativeSyntheticAProducesAAA() {
        let path = Bundle(for: MadInferSmokeTests.self).path(forResource: "synthetic-A", ofType: "gguf")
        runNativeCheck { MadInferCheckPositive(path, "AAA") }
    }
    func testNativeSyntheticBProducesBBB() {
        let path = Bundle(for: MadInferSmokeTests.self).path(forResource: "synthetic-B", ofType: "gguf")
        runNativeCheck { MadInferCheckPositive(path, "BBB") }
    }
}
