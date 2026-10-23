import XCTest

final class OnboardingTests: XCTestCase {
    func testEnglishOnlyRecommendationCannotBeUsedForFrenchSetup() throws {
        let model = try decodeModel(languages: ["en"], detects: false, recommended: true)
        XCTAssertFalse(model.supports(["fr"]))
    }

    func testInstalledEnglishOnlyModelCannotOverrideJapaneseChoice() throws {
        let model = try decodeModel(languages: ["en"], detects: false, downloaded: true)
        XCTAssertFalse(model.supports(["ja"]))
    }

    func testModelMustRecognizeEverySelectedLanguage() throws {
        let model = try decodeModel(languages: ["en", "fr"], detects: true)
        XCTAssertFalse(model.supports(["en", "ja"]))
    }

    func testCompatibleMultilingualModelAcceptsScriptAndRegionChoices() throws {
        let model = try decodeModel(languages: ["pt", "zh", "fr"], detects: true)
        XCTAssertTrue(model.supports(["pt-BR", "zh-Hant"]))
    }

    func testUnrestrictedDetectionDoesNotRecommendEnglishOnlyModel() throws {
        let model = try decodeModel(languages: ["en"], detects: false)
        XCTAssertFalse(model.supports([]))
    }

    func testUnknownModelLanguagesAreNotPresentedAsCompatible() throws {
        let model = try decodeModel(languages: [], detects: false, downloaded: true)
        XCTAssertFalse(model.supports(["fr"]))
    }

    private func decodeModel(
        languages: [String],
        detects: Bool,
        downloaded: Bool = false,
        recommended: Bool = false
    ) throws -> OnboardingModel {
        let record: [String: JSONValue] = [
            "id": "fixture/speech.gguf",
            "name": "Speech model",
            "description": "Speech recognition",
            "size_mb": 700,
            "is_downloaded": .bool(downloaded),
            "is_downloading": false,
            "is_recommended": .bool(recommended),
            "source": ["HuggingFace": ["repo_id": "fixture", "revision": "main"]],
            "supported_languages": .array(languages.map(JSONValue.string)),
            "supports_language_detection": .bool(detects),
        ]
        return try Core.decoder.decode(OnboardingModel.self, from: JSONEncoder().encode(record))
    }
}
