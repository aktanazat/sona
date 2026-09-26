import AppKit
import ApplicationServices
import Foundation

// Read-only Accessibility. Never selects a tile, opens a panel, posts a key,
// requests a permission, or touches a camera/microphone. English layouts only;
// an unrecognised tree is unavailable, not an invitation to infer a name.
private struct CallTarget: Codable {
    let id: String
    let label: String
    let bundle_id: String
    let provider: String
}

private struct CallParticipant: Codable, Equatable {
    let id: String
    let name: String
    let is_local: Bool
}

private struct CallRead: Codable {
    var state: String
    var detail: String
    var participants: [CallParticipant] = []
    var active_ids: [String] = []
}

private struct CallTargets: Codable {
    var state: String
    var detail: String
    var targets: [CallTarget] = []
}

private final class HeldCall {
    let target: CallTarget
    let window: AXUIElement
    let root: AXUIElement
    let url: String?
    let pid: pid_t
    var participants: [(AXUIElement, String)] = []

    init(target: CallTarget, window: AXUIElement, root: AXUIElement, url: String?, pid: pid_t) {
        self.target = target
        self.window = window
        self.root = root
        self.url = url
        self.pid = pid
    }

    func identity(_ element: AXUIElement) -> String? {
        if let held = participants.first(where: { CFEqual($0.0, element) }) { return held.1 }
        guard participants.count < 512 else { return nil }
        let id = UUID().uuidString
        participants.append((element, id))
        return id
    }
}

private let callLock = NSLock()
private var heldCalls: [String: HeldCall] = [:]
private let callBrowsers: Set<String> = [
    "com.apple.safari", "com.google.chrome", "com.google.chrome.canary",
    "com.microsoft.edgemac", "org.mozilla.firefox", "company.thebrowser.browser",
]

private func callAttribute(_ element: AXUIElement, _ attribute: CFString) -> CFTypeRef? {
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, attribute, &value) == .success else { return nil }
    return value
}

private func callText(_ element: AXUIElement, _ attribute: CFString) -> String {
    (callAttribute(element, attribute) as? String)?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
}

private func callLabel(_ element: AXUIElement) -> String {
    let title = callText(element, kAXTitleAttribute as CFString)
    if !title.isEmpty { return title }
    let description = callText(element, kAXDescriptionAttribute as CFString)
    if !description.isEmpty { return description }
    return callText(element, kAXValueAttribute as CFString)
}

private func callURL(_ element: AXUIElement) -> String? {
    if let url = callAttribute(element, kAXURLAttribute as CFString) as? URL { return url.absoluteString }
    let text = callText(element, kAXURLAttribute as CFString)
    return text.isEmpty ? nil : text
}

private struct CallNode {
    let element: AXUIElement
    let parent: Int?
    let role: String
    let label: String
}

/// A budgeted breadth-first walk. Partial trees are never used as evidence.
private func callTree(_ root: AXUIElement) -> [CallNode]? {
    let deadline = ProcessInfo.processInfo.systemUptime + 0.6
    var pending: [(AXUIElement, Int?, Int)] = [(root, nil, 0)]
    var result: [CallNode] = []
    var cursor = 0
    while cursor < pending.count {
        guard result.count < 2500, ProcessInfo.processInfo.systemUptime < deadline else { return nil }
        let (element, parent, depth) = pending[cursor]
        cursor += 1
        let role = callText(element, kAXRoleAttribute as CFString)
        guard !role.isEmpty else { return nil }
        let index = result.count
        result.append(CallNode(element: element, parent: parent, role: role, label: callLabel(element)))
        let children = callAttribute(element, kAXChildrenAttribute as CFString) as? [AXUIElement] ?? []
        guard children.isEmpty || depth < 30 else { return nil }
        guard pending.count + children.count <= 2500 else { return nil }
        pending.append(contentsOf: children.map { ($0, index, depth + 1) })
    }
    return result
}

private func callProvider(_ bundle: String, url: String?) -> String? {
    if bundle == "us.zoom.xos" { return "zoom" }
    if bundle == "com.microsoft.teams2" || bundle == "com.microsoft.teams" { return "teams" }
    guard callBrowsers.contains(bundle), let url, let address = URL(string: url), address.scheme == "https" else { return nil }
    switch address.host?.lowercased() {
    case "meet.google.com":
        // Home, landing and help pages cannot become a target.
        let code = address.path.split(separator: "/").first.map(String.init) ?? ""
        return code.range(of: "^[a-z]{3}-[a-z]{4}-[a-z]{3}$", options: .regularExpression) != nil ? "meet" : nil
    case "teams.microsoft.com", "teams.live.com", "teams.cloud.microsoft": return "teams"
    default: return nil
    }
}

private func callJoined(_ nodes: [CallNode], provider: String) -> Bool {
    let buttons = Set(nodes.lazy.filter { $0.role == kAXButtonRole as String }.map { $0.label.lowercased() })
    switch provider {
    case "zoom":
        let hasParticipants = nodes.contains { rosterHeading($0.label, provider: provider) }
        return hasParticipants && (buttons.contains("leave") || buttons.contains("end")
            || buttons.contains("leave meeting") || buttons.contains("end meeting"))
    case "teams":
        // "Leave" by itself can belong to a group chat, not a joined call.
        let hasMeetingPeople = buttons.contains("show participants") || nodes.contains {
            ["attendees", "in this meeting"].contains($0.label.lowercased())
        }
        return buttons.contains("hang up") || (buttons.contains("leave") && hasMeetingPeople)
    case "meet": return buttons.contains("leave call")
    default: return false
    }
}

private func rosterHeading(_ text: String, provider: String) -> Bool {
    let text = text.lowercased()
    switch provider {
    case "zoom": return text == "participants" || text.hasPrefix("participants (")
    case "teams": return text == "attendees" || text == "in this meeting" || text == "people"
    case "meet": return text == "people"
    default: return false
    }
}

private func callRoster(_ held: HeldCall, nodes: [CallNode]) -> CallRead {
    var children: [Int: [Int]] = [:]
    for (index, node) in nodes.enumerated() {
        if let parent = node.parent { children[parent, default: []].append(index) }
    }
    let talkingLabels = Set(nodes.lazy.filter { $0.role == kAXStaticTextRole as String && $0.label.hasPrefix("Talking: ") }.map(\.label))
    var rows: [(Int, Int, String)] = []
    for (index, node) in nodes.enumerated() {
        let subrole = callText(node.element, kAXSubroleAttribute as CFString)
        guard node.role == kAXRowRole as String || subrole == "AXListItem" || subrole == "AXOutlineRow" else { continue }
        var parent = node.parent
        var inRoster = false
        while let position = parent {
            if rosterHeading(nodes[position].label, provider: held.target.provider) { inRoster = true; break }
            parent = nodes[position].parent
        }
        guard inRoster else { continue }
        // Use a row's own text child, never text from its action menu or a
        // neighbouring tile. Composite status labels are not display names.
        let names = (children[index] ?? []).filter {
            nodes[$0].role == kAXStaticTextRole as String && !nodes[$0].label.isEmpty
        }
        guard let nameIndex = names.first, nodes[nameIndex].label.utf8.count <= 256 else { continue }
        rows.append((index, nameIndex, nodes[nameIndex].label))
    }
    guard !rows.isEmpty, rows.count <= 128 else {
        return CallRead(state: "unavailable", detail: "Open the call's people panel. This participant layout is not readable.")
    }
    var participants: [CallParticipant] = []
    var active: [String] = []
    var sawSpeakingSignal = false
    for (index, nameIndex, rawName) in rows {
        guard let id = held.identity(nodes[index].element) else {
            return CallRead(state: "unavailable", detail: "The call changed too many participant identities. Choose the call again.")
        }
        let local = rawName.hasSuffix(" (me)") || rawName.hasSuffix(" (You)") || rawName.hasSuffix(" (you)")
        var name = rawName
        for suffix in [" (me)", " (You)", " (you)", " (host)", " (Host)"] where name.hasSuffix(suffix) {
            name = String(name.dropLast(suffix.count))
        }
        guard !name.isEmpty else { continue }
        participants.append(CallParticipant(id: id, name: name, is_local: local))
        // Explicit status text only. AXSelected, tile order, focus, pinning,
        // spotlight and 'active speaker view' never imply somebody is talking.
        // Zoom's shipped English resources name 'Talking: %@' and 'is talking ...'.
        // Teams/Meet are accepted only if the row itself exposes a present-tense
        // speech status; without one their names remain manual roster choices.
        let childLabels = (children[index] ?? []).filter { $0 != nameIndex }.map { nodes[$0].label }
        let speaking = childLabels.contains("Speaking") || childLabels.contains("is speaking")
            || (held.target.provider == "zoom" && childLabels.contains("is talking ..."))
        let zoomTalking = held.target.provider == "zoom" && talkingLabels.contains("Talking: \(rawName)")
        if speaking || zoomTalking {
            sawSpeakingSignal = true
            active.append(id)
        }
    }
    guard !participants.isEmpty else { return CallRead(state: "unavailable", detail: "No participant names are exposed by this call.") }
    return CallRead(
        state: sawSpeakingSignal ? "reading" : "roster_only",
        detail: sawSpeakingSignal ? "Reading names and the call's speaking indicator." : "Names are available to pick later. No speaking indicator is exposed.",
        participants: participants.sorted { $0.id < $1.id }, active_ids: active.sorted())
}

private func callTargets() -> CallTargets {
    guard AXIsProcessTrusted() else {
        return CallTargets(state: "unavailable", detail: "Allow Sona in System Settings → Privacy & Security → Accessibility to read call names.")
    }
    // Discard expired AX objects before the bounded registry fills. A new
    // window or web root receives a new selection, never an old call's token.
    heldCalls = heldCalls.filter { _, held in
        guard let app = NSRunningApplication(processIdentifier: held.pid), !app.isTerminated else { return false }
        return !callText(held.root, kAXRoleAttribute as CFString).isEmpty && callURL(held.root) == held.url
    }
    var targets: [CallTarget] = []
    for app in NSWorkspace.shared.runningApplications {
        guard let bundle = app.bundleIdentifier?.lowercased(),
              callProvider(bundle, url: nil) != nil || callBrowsers.contains(bundle) else { continue }
        let application = AXUIElementCreateApplication(app.processIdentifier)
        AXUIElementSetMessagingTimeout(application, 0.1)
        let windows = callAttribute(application, kAXWindowsAttribute as CFString) as? [AXUIElement] ?? []
        for window in windows.prefix(16) {
            guard let nodes = callTree(window) else { continue }
            let roots: [(AXUIElement, String?)]
            if callBrowsers.contains(bundle) {
                roots = nodes.filter { $0.role == "AXWebArea" }.compactMap {
                    guard let url = callURL($0.element), callProvider(bundle, url: url) != nil else { return nil }
                    return ($0.element, url)
                }
            } else { roots = [(window, nil)] }
            for (root, url) in roots {
                guard let provider = callProvider(bundle, url: url), let tree = callTree(root), callJoined(tree, provider: provider) else { continue }
                let existing = heldCalls.values.first { $0.pid == app.processIdentifier && CFEqual($0.root, root) && $0.url == url }
                if let existing { targets.append(existing.target); continue }
                guard heldCalls.count < 64 else { continue }
                let title = callLabel(window)
                let target = CallTarget(id: UUID().uuidString, label: "\(provider == "meet" ? "Google Meet" : provider.capitalized) — \(title)", bundle_id: bundle, provider: provider)
                heldCalls[target.id] = HeldCall(target: target, window: window, root: root, url: url, pid: app.processIdentifier)
                targets.append(target)
            }
        }
    }
    return CallTargets(state: targets.isEmpty ? "unavailable" : "available", detail: targets.isEmpty ? "No joined Zoom, Teams or Google Meet call is readable. Open its people panel, then refresh." : "Choose the call this recording is capturing.", targets: targets)
}

private func callSample(_ id: String) -> CallRead {
    guard AXIsProcessTrusted() else { return CallRead(state: "unavailable", detail: "Accessibility permission is not available.") }
    guard let held = heldCalls[id], let app = NSRunningApplication(processIdentifier: held.pid), !app.isTerminated else {
        return CallRead(state: "unavailable", detail: "The selected call is no longer available.")
    }
    let application = AXUIElementCreateApplication(held.pid)
    AXUIElementSetMessagingTimeout(application, 0.1)
    let windows = callAttribute(application, kAXWindowsAttribute as CFString) as? [AXUIElement] ?? []
    guard windows.contains(where: { CFEqual($0, held.window) }), callURL(held.root) == held.url,
          let nodes = callTree(held.root), callJoined(nodes, provider: held.target.provider) else {
        return CallRead(state: "unavailable", detail: "The selected call ended, changed, or is not readable. Sona has stopped reading names.")
    }
    return callRoster(held, nodes: nodes)
}

private func callJSON<T: Encodable>(_ value: T) -> UnsafeMutablePointer<CChar>? {
    guard let data = try? JSONEncoder().encode(value), let text = String(data: data, encoding: .utf8) else { return nil }
    return strdup(text)
}

@_cdecl("sona_call_roster_target_json")
public func sonaCallRosterTargetJSON(_ target: UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>? {
    guard let target else { return nil }
    callLock.lock()
    defer { callLock.unlock() }
    guard let held = heldCalls[String(cString: target)] else { return nil }
    return callJSON(held.target)
}

@_cdecl("sona_call_roster_targets_json")
public func sonaCallRosterTargetsJSON() -> UnsafeMutablePointer<CChar>? {
    callLock.lock()
    defer { callLock.unlock() }
    return callJSON(callTargets())
}

@_cdecl("sona_call_roster_sample_json")
public func sonaCallRosterSampleJSON(_ target: UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>? {
    guard let target else { return nil }
    callLock.lock()
    defer { callLock.unlock() }
    return callJSON(callSample(String(cString: target)))
}

@_cdecl("sona_call_roster_release")
public func sonaCallRosterRelease(_ target: UnsafePointer<CChar>?) {
    guard let target else { return }
    callLock.lock()
    heldCalls.removeValue(forKey: String(cString: target))
    callLock.unlock()
}

@_cdecl("sona_call_roster_free_string")
public func sonaCallRosterFreeString(_ pointer: UnsafeMutablePointer<CChar>?) {
    free(pointer)
}
