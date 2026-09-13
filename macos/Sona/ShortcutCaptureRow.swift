import AppKit
import SwiftUI

/// One shortcut row: what the chord is now, a way to record a new one, and a
/// reset when it is no longer the default.
///
/// Works for any binding the core carries, not just transcribe. Which layer
/// reads the keys is the keyboard-implementation setting, and the two behave
/// differently enough that the row keeps both:
///
/// - The native listener records in the core. The row arms it, watches
///   `handy-keys-event`, and lets the core spell the chord — it is the same
///   listener that will match the chord later, so what it spells is what will
///   fire.
/// - The Tauri layer has no recorder, so the row reads the keys itself from
///   this window and spells the chord the way the core's parser expects.
///
/// Either way a chord is committed on release, never on press: a shortcut is
/// what the user finished pressing, and committing on the first key down
/// would store a modifier the moment it went down.
struct ShortcutCaptureRow: View {
    let store: SettingsStore
    /// The binding key in the settings record: `transcribe`, `cancel`, and
    /// whatever else the core carries.
    let id: String
    /// Disabled while some other setting makes this shortcut meaningless —
    /// push-to-talk has no cancel chord, for one.
    var disabled = false

    @State private var capture = ShortcutCaptureSession()

    var body: some View {
        if let record = store.binding(id) {
            CardRow {
                VStack(alignment: .leading, spacing: 3) {
                    Text(BindingCopy.title(record))
                        .bodyText(15, Theme.ink)
                    if let detail = BindingCopy.detail(record) {
                        Text(detail).metaText(Theme.inkTertiary)
                    }
                }
            } trailing: {
                HStack(spacing: 10) {
                    if record.changed, !capture.recording {
                        Button("Reset") {
                            Task { await store.resetBinding(id) }
                        }
                        .buttonStyle(.quiet)
                        .disabled(disabled || store.isBusy("binding_\(id)"))
                    }
                    field(record)
                }
            }
        } else if store.loaded {
            /// The core dropped a binding this build still shows a row for.
            /// Saying so beats an empty control that records into nothing.
            CardRow {
                Text(BindingCopy.title(placeholder))
                    .bodyText(15, Theme.ink)
            } trailing: {
                Text("Shortcut not found").metaText(Theme.live)
            }
        } else {
            CardRow {
                Text(BindingCopy.title(placeholder))
                    .bodyText(15, Theme.ink)
            } trailing: {
                Text("Loading shortcuts…").metaText(Theme.inkTertiary)
            }
        }
    }

    /// A stand-in record, so a row with nothing behind it still says which
    /// shortcut it was for.
    private var placeholder: BindingRecord {
        BindingRecord(id: id, name: "Shortcut", description: "", defaultBinding: "", currentBinding: "")
    }

    /// The control itself: the chord, or the keys as they are pressed.
    @ViewBuilder
    private func field(_ record: BindingRecord) -> some View {
        Button {
            Task { await start(record) }
        } label: {
            Group {
                if capture.recording {
                    if capture.candidate.isEmpty {
                        Text("Press your keys…").metaText(Theme.accent)
                    } else {
                        Shortcut(BindingCopy.chord(capture.candidate))
                    }
                } else if record.currentBinding.isEmpty {
                    Text("Not set").metaText(Theme.inkTertiary)
                } else {
                    Shortcut(BindingCopy.chord(record.currentBinding))
                }
            }
            .padding(.horizontal, 10)
            .frame(height: 32)
            .frame(minWidth: 132)
            .background(capture.recording ? Theme.accentSoft : Theme.inset, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
            .overlay(
                RoundedRectangle(cornerRadius: Theme.radiusControl)
                    .strokeBorder(capture.recording ? Theme.accent : Theme.border, lineWidth: 1)
            )
        }
        .buttonStyle(.plain)
        .disabled(disabled || capture.recording || store.isBusy("binding_\(id)"))
        .help(capture.recording ? "Press Escape to keep the current shortcut" : "Record a new shortcut for \(BindingCopy.title(record))")
        .onDisappear {
            /// Leaving the page mid-capture would leave a listener running
            /// with nothing reading it, and every shortcut suspended.
            if capture.recording { Task { await finish() } } else { capture.tearDown() }
        }
    }

    // MARK: - Recording

    private func start(_ record: BindingRecord) async {
        guard !capture.recording else { return }
        capture.original = record.currentBinding
        switch store.settings.keyboardImplementation {
        case .handyKeys:
            /// The core refuses while secure input holds the keyboard, and
            /// the reason it gives is already on screen: nothing to record.
            guard await store.startHandyKeysRecording(id) == nil else { return }
            store.onHandyKeys = { event in Task { await handle(event) } }
        case .tauri:
            /// Nothing may fire, or swallow the keys, while they are being
            /// read — including the shortcut being re-recorded.
            await store.suspendAllBindings()
            capture.monitor = NSEvent.addLocalMonitorForEvents(matching: [.keyDown, .keyUp, .flagsChanged]) { event in
                let key = ShortcutCaptureKey(event)
                Task { @MainActor in await read(key) }
                /// Swallowed: a key pressed into a recorder is not text and
                /// has no business reaching the window behind it.
                return nil
            }
        }
        capture.recording = true
        capture.candidate = ""
        capture.keyed = ""
        capture.modifierOnly = ""
        capture.held = []
        capture.recorded = []
    }

    /// One key event from the core's own listener.
    ///
    /// A chord with a key in it commits when that key comes back up; a
    /// modifier-only chord commits when the last modifier does. Committing on
    /// the first release would store the modifier alone whenever the key
    /// event never arrived, which is exactly what secure input causes.
    private func handle(_ event: HandyKeysEvent) async {
        guard capture.recording else { return }
        if event.isKeyDown, !event.hotkeyString.isEmpty {
            if event.key == nil {
                capture.modifierOnly = event.hotkeyString
            } else {
                capture.keyed = event.hotkeyString
            }
            capture.candidate = event.hotkeyString
        } else if !event.isKeyDown, event.key != nil {
            let chord = capture.keyed.isEmpty ? event.hotkeyString : capture.keyed
            if !chord.isEmpty { await commit(chord) }
        } else if !event.isKeyDown, event.key == nil, event.modifiers.isEmpty,
                  capture.keyed.isEmpty, !capture.modifierOnly.isEmpty {
            await commit(capture.modifierOnly)
        }
    }

    /// One key event read from this window, for the Tauri layer.
    private func read(_ event: ShortcutCaptureKey) async {
        guard capture.recording else { return }
        switch event.kind {
        case .down:
            guard !event.repeated else { return }
            /// Escape alone leaves the shortcut as it was: a recorder with
            /// no way out is a trap on a row one click from the window edge.
            if event.escape, event.modifiers.isEmpty {
                await cancel()
                return
            }
            press(event.name)
        case .up:
            await release(event.name)
        case .flags:
            /// A flags event says which modifiers are down now, not which
            /// one moved, so the difference against what was held is the
            /// press or the release.
            let held = Set(capture.held)
            for name in event.modifiers where !held.contains(name) { press(name) }
            for name in capture.held where !event.modifiers.contains(name) { await release(name) }
        }
    }

    private func press(_ name: String) {
        if !capture.held.contains(name) { capture.held.append(name) }
        if !capture.recorded.contains(name) { capture.recorded.append(name) }
        capture.candidate = ShortcutCaptureSession.chord(capture.recorded)
    }

    /// Everything let go, so the chord is whatever was pressed along the way
    /// — not only what is still down.
    private func release(_ name: String) async {
        capture.held.removeAll { $0 == name }
        guard capture.held.isEmpty, !capture.recorded.isEmpty else { return }
        await commit(ShortcutCaptureSession.chord(capture.recorded))
    }

    private func commit(_ chord: String) async {
        let original = capture.original
        await finish()
        /// A refused chord — one another app already holds — leaves the row
        /// showing whatever the core kept, so put the old one back rather
        /// than leave the shortcut on a chord that will not fire.
        if await store.changeBinding(id, chord: chord) == false, !original.isEmpty {
            await store.changeBinding(id, chord: original)
        }
    }

    private func cancel() async {
        await finish()
    }

    /// Stop listening, whichever layer was listening, and let the shortcuts
    /// work again.
    private func finish() async {
        capture.tearDown()
        store.onHandyKeys = nil
        capture.recording = false
        capture.candidate = ""
        capture.keyed = ""
        capture.modifierOnly = ""
        capture.held = []
        capture.recorded = []
        switch store.settings.keyboardImplementation {
        case .handyKeys: await store.stopHandyKeysRecording()
        case .tauri: await store.resumeAllBindings()
        }
    }
}

/// What one row holds while it is recording.
@MainActor
@Observable
final class ShortcutCaptureSession {
    var recording = false
    /// The chord as it stands, for the row to show.
    var candidate = ""
    /// A chord that has a key in it, kept apart from a modifier-only one so
    /// a key's release never commits the modifiers alone.
    var keyed = ""
    var modifierOnly = ""
    /// Still down, and everything that went down during this capture.
    var held: [String] = []
    var recorded: [String] = []
    /// The chord to put back if the capture is abandoned or refused.
    var original = ""
    @ObservationIgnored var monitor: Any?

    func tearDown() {
        if let monitor {
            NSEvent.removeMonitor(monitor)
            self.monitor = nil
        }
    }

    /// Modifiers first, in the order they were pressed, then the key: the
    /// shape the core's chord parser reads.
    static func chord(_ keys: [String]) -> String {
        let modifiers = keys.filter(isModifier)
        let rest = keys.filter { !isModifier($0) }
        return (modifiers + rest).joined(separator: "+")
    }

    static func isModifier(_ key: String) -> Bool {
        ["shift", "ctrl", "option", "command", "fn"].contains(key)
    }
}

/// One key event, read off an `NSEvent` where it arrives and carried as
/// plain values from there: a chord is made of names, and nothing past the
/// monitor needs the event itself.
struct ShortcutCaptureKey: Sendable {
    enum Kind: Sendable { case down, up, flags }

    let kind: Kind
    let name: String
    /// Every modifier down at this moment, in the order the caps read.
    let modifiers: [String]
    let repeated: Bool
    let escape: Bool

    init(_ event: NSEvent) {
        kind = switch event.type {
        case .keyDown: Kind.down
        case .keyUp: Kind.up
        default: Kind.flags
        }
        var names: [String] = []
        let flags = event.modifierFlags
        if flags.contains(.control) { names.append("ctrl") }
        if flags.contains(.option) { names.append("option") }
        if flags.contains(.shift) { names.append("shift") }
        if flags.contains(.command) { names.append("command") }
        /// The globe key only counts as a modifier when it is the key that
        /// moved: arrows and the editing block set the same flag.
        if flags.contains(.function), event.type == .flagsChanged, event.keyCode == 63 {
            names.append("fn")
        }
        modifiers = names
        repeated = event.type == .keyDown && event.isARepeat
        escape = event.keyCode == 53
        name = Self.name(of: event)
    }

    /// The core's own name for a key. Arrows and the editing block carry the
    /// function flag too, so the table is read by key code and the typed
    /// character is only the fallback for letters, digits, and punctuation.
    private static func name(of event: NSEvent) -> String {
        if event.type == .flagsChanged { return "" }
        if let named = codes[event.keyCode] { return named }
        let typed = (event.charactersIgnoringModifiers ?? "").lowercased()
        return typed.isEmpty ? "key \(event.keyCode)" : typed
    }

    /// Virtual key codes whose name is not the character they type. The
    /// numbers are Carbon's `kVK_` constants, which is still where macOS
    /// publishes them.
    private static let codes: [UInt16: String] = [
        36: "enter", 76: "enter", 48: "tab", 49: "space", 51: "backspace",
        53: "esc", 117: "delete", 114: "insert", 57: "caps lock",
        122: "f1", 120: "f2", 99: "f3", 118: "f4", 96: "f5", 97: "f6",
        98: "f7", 100: "f8", 101: "f9", 109: "f10", 103: "f11", 111: "f12",
        105: "f13", 107: "f14", 113: "f15", 106: "f16", 64: "f17", 79: "f18",
        80: "f19", 90: "f20",
        123: "left", 124: "right", 125: "down", 126: "up",
        115: "home", 119: "end", 116: "page up", 121: "page down",
        71: "num lock", 75: "numpad /", 67: "numpad *", 78: "numpad -",
        69: "numpad +", 65: "numpad .", 82: "numpad 0", 83: "numpad 1",
        84: "numpad 2", 85: "numpad 3", 86: "numpad 4", 87: "numpad 5",
        88: "numpad 6", 89: "numpad 7", 91: "numpad 8", 92: "numpad 9",
    ]
}
