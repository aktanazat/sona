import Foundation
import Speech
import Testing
import UIKit

/// The keyboard's microphone with the web page and Apple's recognizer replaced by stand-ins
/// the test drives, since neither runs in a unit test.
@MainActor
struct KeyboardMicrophoneTests {
    final class Page: CapturePage {
        var onMessage: (([String: Any]) -> Void)?
        var onFailure: ((String) -> Void)?
        var onReload: (() -> Void)?
        private(set) var calls: [String] = []
        func attach(to view: UIView) { onMessage?(["type": "ready"]) }
        func detach() {}
        func run(_ call: String) { calls.append(call) }
    }

    final class Recognizer {
        private(set) var reports: [@Sendable (String?, Bool, String?) -> Void] = []
        var recognize: Recognize {
            { [unowned self] _, _, report in
                reports.append(report)
                return {}
            }
        }
    }

    let page = Page()
    let recognizer = Recognizer()

    func microphone(start: Duration = .seconds(3)) -> (KeyboardMicrophone, Box) {
        let microphone = KeyboardMicrophone(
            page: page, recognize: recognizer.recognize,
            timing: .init(start: start, drain: .milliseconds(150), finish: .seconds(3)), queue: nil
        )
        let box = Box()
        microphone.onEvent = { box.events.append($0) }
        microphone.attach(to: UIView())
        return (microphone, box)
    }

    /// The events the microphone sent, and a wait for the first one that needs no clock.
    final class Box {
        var events: [KeyboardDictation.Event] = [] {
            didSet { waiting.forEach { $0.resume() }; waiting = [] }
        }
        private var waiting: [CheckedContinuation<Void, Never>] = []

        func firstEvent() async {
            guard events.isEmpty else { return }
            await withCheckedContinuation { waiting.append($0) }
        }
    }

    /// Waits on the main actor until `condition` holds, for at most two seconds.
    func until(_ condition: () -> Bool) async {
        let deadline = ContinuousClock.now + .seconds(2)
        while !condition(), ContinuousClock.now < deadline { try? await Task.sleep(for: .milliseconds(5)) }
    }

    @Test("words from a dictation that was replaced never reach the new one")
    func staleRecognitionIsIgnored() async {
        let (microphone, box) = microphone()
        microphone.start(language: "en-US", hints: [])
        microphone.start(language: "en-US", hints: [])
        recognizer.reports[0]("old words", true, nil)
        recognizer.reports[1]("new", false, nil)
        await box.firstEvent()
        #expect(box.events == [.heard("new", final: false)])
    }

    @Test("a page that never says the microphone opened counts as refused once the start time passes")
    func startDeadlineRefuses() async {
        let (microphone, box) = microphone(start: .milliseconds(50))
        microphone.start(language: "en-US", hints: [])
        await box.firstEvent()
        #expect(box.events == [.captureRefused])
    }

    @Test("a final result before Stop inserts once, and the dictation's own stop asks the page for nothing more")
    func finalBeforeStop() async {
        let (microphone, box) = microphone()
        var dictation = KeyboardDictation()
        var inserted: [String] = []
        microphone.onEvent = { event in
            box.events.append(event)
            for effect in dictation.handle(event) {
                switch effect {
                case .endCapture: microphone.end()
                case .abandon: microphone.abandon()
                case let .insert(text): inserted.append(text)
                case .beginCapture, .fallBack: break
                }
            }
        }
        for effect in dictation.handle(.start) where effect == .beginCapture {
            microphone.start(language: "en-US", hints: [])
        }
        page.onMessage?(["type": "started", "sampleRate": 48_000.0])
        recognizer.reports[0]("Meet me at noon.", true, nil)
        await until { !inserted.isEmpty }
        #expect(inserted == ["Meet me at noon."])
        #expect(dictation.phase == .idle)
        #expect(!page.calls.contains("finishCapture()"))
    }

    @Test("a page call that fails while nothing is dictating does not keep the next Dictate waiting for the page")
    func failedCallWhileIdle() {
        let (microphone, box) = microphone()
        page.onFailure?("captureHeartbeat(): A JavaScript exception occurred")
        microphone.start(language: "en-US", hints: [])
        #expect(page.calls.last == "startCapture()")
        #expect(box.events.isEmpty)
    }

    @Test("Stop asks the page for the samples it still holds before the audio ends")
    func stopDrainsThePage() async {
        let (microphone, _) = microphone()
        microphone.start(language: "en-US", hints: [])
        page.onMessage?(["type": "started", "sampleRate": 48_000.0])
        microphone.end()
        #expect(page.calls.last == "finishCapture()")
        page.onMessage?(["type": "stopped"])
        #expect(page.calls.last == "stopCapture()")
    }
}

/// Where Dictate goes after the keyboard could not listen.
struct KeyboardRouteTests {
    let defaults = UserDefaults(suiteName: "keyboard-route-\(UUID().uuidString)")!
    let now = Date(timeIntervalSince1970: 1_800_000_000)

    @Test("a fallback is kept: Dictate goes through the app at once, across keyboard appearances, for ten minutes")
    func refusalHoldsForTenMinutes() {
        var dictation = KeyboardDictation()
        _ = dictation.handle(.start)
        KeyboardRoute(defaults: defaults).record(dictation.handle(.captureRefused), at: now)
        let route = KeyboardRoute(defaults: defaults)
        #expect(route.choice(at: now.addingTimeInterval(60), warm: false) == .openApp)
        #expect(route.choice(at: now.addingTimeInterval(60), warm: true) == .appSession)
        #expect(route.choice(at: now.addingTimeInterval(10 * 60 + 1), warm: false) == .keyboard)
    }

    @Test("a language with no recognizer on this iPhone does not send Dictate to the app")
    func unavailableLanguageStaysInTheKeyboard() {
        var dictation = KeyboardDictation()
        _ = dictation.handle(.start)
        KeyboardRoute(defaults: defaults).record(dictation.handle(.recognizerUnavailable), at: now)
        #expect(dictation.phase == .idle)
        #expect(KeyboardRoute(defaults: defaults).choice(at: now, warm: false) == .keyboard)
    }
}
