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
        private(set) var calls: [String] = []
        func attach(to view: UIView) { onMessage?(["type": "ready"]) }
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

    final class Box { var events: [KeyboardDictation.Event] = [] }

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
        await until { !box.events.isEmpty }
        #expect(box.events == [.heard("new", final: false)])
    }

    @Test("a page that never says the microphone opened counts as refused once the start time passes")
    func startDeadlineRefuses() async {
        let (microphone, box) = microphone(start: .milliseconds(50))
        microphone.start(language: "en-US", hints: [])
        await until { !box.events.isEmpty }
        #expect(box.events == [.captureRefused])
    }

    @Test("a final result before Stop ends the dictation, and a later Stop asks the page for nothing")
    func finalBeforeStop() async {
        let (microphone, box) = microphone()
        microphone.start(language: "en-US", hints: [])
        page.onMessage?(["type": "started", "sampleRate": 48_000.0])
        recognizer.reports[0]("Meet me at noon.", true, nil)
        await until { box.events.count == 2 }
        microphone.end()
        #expect(box.events == [.captureStarted, .heard("Meet me at noon.", final: true)])
        #expect(!page.calls.contains("finishCapture()"))
        #expect(page.calls.last == "stopCapture()")
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

    @Test("after a refusal Dictate goes through the app at once, across keyboard appearances, for ten minutes")
    func refusalHoldsForTenMinutes() {
        var dictation = KeyboardDictation()
        _ = dictation.handle(.start)
        #expect(dictation.handle(.captureRefused).contains(.fallBack))
        KeyboardRoute(defaults: defaults).refuse(at: now)
        let route = KeyboardRoute(defaults: defaults)
        #expect(route.choice(at: now.addingTimeInterval(60), warm: false) == .openApp)
        #expect(route.choice(at: now.addingTimeInterval(60), warm: true) == .appSession)
        #expect(route.choice(at: now.addingTimeInterval(10 * 60 + 1), warm: false) == .keyboard)
    }
}
