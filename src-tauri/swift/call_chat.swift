import AppKit
import ApplicationServices
import Foundation

// English Zoom labels are shipped in Zoom's en.lproj/Localizable.strings.
// Unknown apps, locales and layouts are copy-only, never guessed selectors.
struct CallChatEvidence {
    var joinedCalls = 0
    var chatPanes = 0
    var composers = 0
    var sendButtons = 0
    var everyone = false
    var value: String?
    var insertionAllowed = false
    var sendAction = false

    var refusal: String? {
        guard joinedCalls == 1 else { return "A single joined meeting could not be identified." }
        guard chatPanes == 1 else { return "The meeting's own chat is not unambiguously open." }
        guard composers == 1 else { return "A single meeting chat composer could not be identified." }
        guard sendButtons == 1, sendAction else { return "The meeting chat's Send action could not be identified." }
        guard everyone else { return "The chat is not clearly addressed to everyone in the meeting." }
        guard let value, value.isEmpty else { return "Your chat draft was left untouched." }
        guard insertionAllowed else { return "The meeting chat does not allow safe text insertion." }
        return nil
    }
}

private struct CallChatResult: Encodable {
    let outcome: String
    let reason: String
}

private struct ChatNode {
    let element: AXUIElement
    let role: String
    let labels: [String]

    func named(_ label: String) -> Bool { labels.contains(label) }
}

private final class ChatWalk {
    private let deadline = CFAbsoluteTimeGetCurrent() + 2
    private var remaining = 6_000
    private(set) var complete = true

    func nodes(_ root: AXUIElement, depth: Int = 0) -> [ChatNode] {
        guard remaining > 0, depth < 40, CFAbsoluteTimeGetCurrent() < deadline else {
            complete = false
            return []
        }
        remaining -= 1
        let role: String = attribute(root, kAXRoleAttribute) ?? ""
        let labels = [kAXTitleAttribute, kAXDescriptionAttribute, kAXHelpAttribute, kAXPlaceholderValueAttribute]
            .compactMap { attribute(root, $0) as String? }
        var result = [ChatNode(element: root, role: role, labels: labels)]
        var rawChildren: CFTypeRef?
        let read = AXUIElementCopyAttributeValue(root, kAXChildrenAttribute as CFString, &rawChildren)
        if read == .success {
            guard let children = rawChildren as? [AXUIElement] else {
                complete = false
                return result
            }
            for child in children { result.append(contentsOf: nodes(child, depth: depth + 1)) }
        } else if read != .attributeUnsupported && read != .noValue {
            // A timed-out subtree can hide a second composer or a different
            // recipient. Partial inspection is not evidence of uniqueness.
            complete = false
        }
        return result
    }
}

private func attribute<T>(_ element: AXUIElement, _ name: String) -> T? {
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, name as CFString, &value) == .success else { return nil }
    return value as? T
}

private func settable(_ element: AXUIElement, _ name: String) -> Bool {
    var result = DarwinBoolean(false)
    return AXUIElementIsAttributeSettable(element, name as CFString, &result) == .success && result.boolValue
}

private func canPress(_ element: AXUIElement) -> Bool {
    var actions: CFArray?
    guard AXUIElementCopyActionNames(element, &actions) == .success else { return false }
    return (actions as? [String])?.contains(kAXPressAction) == true
}

private func emptySelection(_ element: AXUIElement) -> Bool {
    var raw: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, kAXSelectedTextRangeAttribute as CFString, &raw) == .success,
          let raw, CFGetTypeID(raw) == AXValueGetTypeID() else { return false }
    let value = unsafeBitCast(raw, to: AXValue.self)
    var range = CFRange(location: 0, length: 0)
    return AXValueGetValue(value, .cfRange, &range) && range.location == 0 && range.length == 0
}

private struct ResolvedChat {
    let window: AXUIElement
    let pane: AXUIElement
    let composer: AXUIElement
    let send: AXUIElement
}

private enum Resolution {
    case ready(ResolvedChat)
    case refused(String)
    case alreadyPresent
}

private func resolve(_ application: AXUIElement, text: String) -> Resolution {
    let walk = ChatWalk()
    let windows: [AXUIElement] = attribute(application, kAXWindowsAttribute) ?? []
    var joined: [(AXUIElement, [ChatNode])] = []
    for window in windows {
        let title: String = attribute(window, kAXTitleAttribute) ?? ""
        guard title == "Zoom Meeting" else { continue }
        let nodes = walk.nodes(window)
        let buttons = nodes.filter { $0.role == kAXButtonRole }
        let leave = buttons.contains { $0.named("Leave") || $0.named("End") || $0.named("Leave meeting") || $0.named("End meeting") }
        let audio = buttons.contains { $0.named("Mute") || $0.named("Unmute") || $0.named("Join audio") }
        let video = buttons.contains { $0.named("Start video") || $0.named("Stop video") }
        let waiting = nodes.contains { $0.named("Waiting room") || $0.named("Waiting for the host to start this meeting") }
        if leave && audio && video && !waiting { joined.append((window, nodes)) }
    }
    guard walk.complete, joined.count == 1, let (window, nodes) = joined.first else {
        return .refused("A single joined Zoom meeting could not be identified.")
    }
    // A pane must belong to the joined window. A detached chat or Zoom Chat
    // outside this hierarchy cannot establish which meeting receives a send.
    let panes = nodes.filter { [kAXGroupRole, kAXSplitGroupRole].contains($0.role) && $0.named("Meeting chat") }
    guard panes.count == 1, let pane = panes.first else {
        return .refused("Open Meeting chat inside the joined Zoom meeting. No notice was sent.")
    }
    let chat = walk.nodes(pane.element)
    guard walk.complete else { return .refused("The meeting chat could not be fully inspected.") }
    let composers = chat.filter {
        [kAXTextAreaRole, kAXTextFieldRole].contains($0.role) && ($0.named("Message...") || $0.named("Chat with everyone"))
    }
    let buttons = chat.filter { $0.role == kAXButtonRole && $0.named("Send message") }
    let everyone = chat.contains {
        [kAXPopUpButtonRole, kAXComboBoxRole].contains($0.role)
            && ($0.named("Everyone") || (attribute($0.element, kAXValueAttribute) as String?) == "Everyone")
    }
    let composer = composers.first?.element
    let evidence = CallChatEvidence(
        joinedCalls: joined.count, chatPanes: panes.count, composers: composers.count,
        sendButtons: buttons.count, everyone: everyone,
        value: composer.flatMap { attribute($0, kAXValueAttribute) as String? },
        insertionAllowed: composer.map { settable($0, kAXSelectedTextAttribute) && emptySelection($0) } ?? false,
        sendAction: buttons.first.map { canPress($0.element) } ?? false)
    if let refusal = evidence.refusal { return .refused(refusal) }
    if chat.contains(where: { node in
        node.role == kAXStaticTextRole && (attribute(node.element, kAXValueAttribute) as String?) == text
    }) { return .alreadyPresent }
    guard let composer, let send = buttons.first?.element else {
        return .refused("The chat controls changed. No notice was sent.")
    }
    return .ready(ResolvedChat(window: window, pane: pane.element, composer: composer, send: send))
}

private func post(bundle: String, text: String) -> CallChatResult {
    guard bundle == "us.zoom.xos" else {
        return CallChatResult(outcome: "not_posted", reason: "This meeting app has no verified automatic chat route. Copy the notice and send it yourself.")
    }
    guard AXIsProcessTrusted() else {
        return CallChatResult(outcome: "not_posted", reason: "Accessibility access is required to identify the meeting chat.")
    }
    let apps = NSRunningApplication.runningApplications(withBundleIdentifier: bundle)
    guard apps.count == 1, let app = apps.first else {
        return CallChatResult(outcome: "not_posted", reason: "The meeting app is not unambiguously running.")
    }
    let application = AXUIElementCreateApplication(app.processIdentifier)
    AXUIElementSetMessagingTimeout(application, 0.2)
    let target: ResolvedChat
    switch resolve(application, text: text) {
    case let .refused(reason): return CallChatResult(outcome: "not_posted", reason: reason)
    case .alreadyPresent: return CallChatResult(outcome: "already_present", reason: "The notice is already visible in this meeting's chat. Nothing was sent again.")
    case let .ready(chat): target = chat
    }
    // Re-resolve identity before inserting. No focus change, AXValue replacement,
    // clipboard or synthetic key events: insert only into an empty selection.
    guard case let .ready(current) = resolve(application, text: text),
          CFEqual(current.window, target.window), CFEqual(current.pane, target.pane),
          CFEqual(current.composer, target.composer), CFEqual(current.send, target.send),
          (attribute(target.composer, kAXValueAttribute) as String?) == "",
          emptySelection(target.composer) else {
        return CallChatResult(outcome: "not_posted", reason: "The meeting chat changed. Your draft was left untouched.")
    }
    let written = AXUIElementSetAttributeValue(target.composer, kAXSelectedTextAttribute as CFString, text as CFString)
    guard written == .success,
          (attribute(target.composer, kAXValueAttribute) as String?) == text else {
        return CallChatResult(outcome: "draft_only", reason: "Sona could not confirm the draft. Check the chat before sending the notice yourself.")
    }
    // The Send control is the same element in the same joined window. Verify
    // the recipient and text again; a user's edit is never sent by Sona.
    let finalWalk = ChatWalk()
    let chat = finalWalk.nodes(target.pane)
    let everyone = chat.contains {
        [kAXPopUpButtonRole, kAXComboBoxRole].contains($0.role)
            && ($0.named("Everyone") || (attribute($0.element, kAXValueAttribute) as String?) == "Everyone")
    }
    let leave = finalWalk.nodes(target.window).contains {
        $0.role == kAXButtonRole && ($0.named("Leave") || $0.named("End") || $0.named("Leave meeting") || $0.named("End meeting"))
    }
    guard finalWalk.complete, !app.isTerminated, everyone, leave,
          (attribute(target.send, kAXEnabledAttribute) as Bool?) == true,
          canPress(target.send),
          (attribute(target.composer, kAXValueAttribute) as String?) == text else {
        return CallChatResult(outcome: "draft_only", reason: "The notice was not sent. Check the meeting chat draft and send it yourself.")
    }
    let pressed = AXUIElementPerformAction(target.send, kAXPressAction as CFString)
    return CallChatResult(outcome: "send_requested", reason: pressed == .success
        ? "Send was requested in the meeting chat. Check that the notice appeared before sending anything yourself."
        : "The Send action could not be confirmed. Check the meeting chat before sending anything yourself.")
}

@_cdecl("sona_call_chat_announce_json")
public func sonaCallChatAnnounceJSON(_ bundle: UnsafePointer<CChar>, _ text: UnsafePointer<CChar>) -> UnsafeMutablePointer<CChar>? {
    let result = post(bundle: String(cString: bundle), text: String(cString: text))
    guard let data = try? JSONEncoder().encode(result), let json = String(data: data, encoding: .utf8) else { return nil }
    return strdup(json)
}

@_cdecl("sona_call_chat_free_string")
public func sonaCallChatFreeString(_ value: UnsafeMutablePointer<CChar>?) { free(value) }
