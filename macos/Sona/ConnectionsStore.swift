import Foundation
import Observation

enum SonaConnectionKind: String, CaseIterable, Codable, Identifiable {
    case slackWebhook = "slack_webhook", slackBot = "slack_bot"
    case notionPage = "notion_page", notionDatabase = "notion_database"
    case hubspot, attioPerson = "attio_person", attioCompany = "attio_company", affinity
    case publicWebhook = "public_webhook"
    var id: Self { self }
    var title: String {
        switch self {
        case .slackWebhook: "Slack · incoming webhook"
        case .slackBot: "Slack · app token"
        case .notionPage: "Notion · page"
        case .notionDatabase: "Notion · database"
        case .hubspot: "HubSpot"
        case .attioPerson: "Attio · person"
        case .attioCompany: "Attio · company"
        case .affinity: "Affinity"
        case .publicWebhook: "Webhook · Zapier or another service"
        }
    }
    var webhook: Bool { self == .slackWebhook || self == .publicWebhook }
    var targetLabel: String {
        switch self {
        case .slackBot: "Channel ID"
        case .notionPage: "Parent page ID"
        case .notionDatabase: "Database ID"
        case .hubspot: "Contact ID (optional)"
        case .attioPerson, .attioCompany: "Record ID (optional)"
        case .affinity: "Person ID (optional)"
        default: "Destination"
        }
    }
    var help: String {
        switch self {
        case .slackWebhook: "Create an incoming webhook in your Slack app. Testing posts a short test message to its channel. Webhook messages cannot be undone."
        case .slackBot: "Paste a Slack bot token with chat:write and channels:read (groups:read for a private channel). Invite the app to the channel."
        case .notionPage, .notionDatabase: "Paste a Notion internal integration token with content read, insert and update access. Share this page or database with the integration. Each send creates a new page."
        case .hubspot: "Use a private app token with contact read and note write access. Leave the ID blank to match existing contacts by attendee email. Sona never creates contacts."
        case .attioPerson, .attioCompany: "Use an Attio token with note:read-write, object_configuration:read and record_permission:read. Leave the ID blank to match exactly one existing person or company from attendee emails."
        case .affinity: "Use an Affinity API key from a plan with API access. Leave the ID blank to match existing people by attendee email. Sona never creates people."
        case .publicWebhook: "Use a public HTTPS address, such as a Zapier catch hook, and a signing secret of at least 16 bytes. Testing sends a test event. Failed deliveries retry up to three times with the same delivery ID. Webhook sends cannot be undone."
        }
    }
}

struct SonaConnection: Decodable, Identifiable, Equatable {
    var id: String
    var name: String
    var kind: SonaConnectionKind
    var target: String
    var enabled: Bool
    var json: JSONValue { ["id": .string(id), "name": .string(name), "kind": .string(kind.rawValue), "target": .string(target), "enabled": .bool(enabled)] }
}

struct ConnectionScope: Decodable, Hashable {
    let kind: String
    let id: String?
    var json: JSONValue {
        var value: [String: JSONValue] = ["kind": .string(kind)]
        if let id { value["id"] = .string(id) }
        return .object(value)
    }
}

struct ConnectionScopeOption: Decodable, Identifiable {
    let scope: ConnectionScope
    let name: String
    var id: ConnectionScope { scope }
}

struct ConnectionRule: Decodable, Identifiable {
    let id: String
    let connectionId: String
    let scope: ConnectionScope
    let enabled: Bool
    let createdAtUtcMs: Int64
}

struct ConnectionPreferences: Decodable {
    var emailDraftsEnabled: Bool
    var calendarActionsEnabled: Bool
}

struct ConnectionReceipt: Decodable, Identifiable {
    let id: String
    let connectionId: String
    let connectionName: String
    let sessionId: String?
    let state: String
    let detail: String
    let createdAtUtcMs: Int64
    let undo: JSONValue?
    var canUndo: Bool { state == "sent" && undo != nil }
    var date: Date { Date(timeIntervalSince1970: Double(createdAtUtcMs) / 1000) }
}

struct ConnectionsSnapshot: Decodable {
    let connections: [SonaConnection]
    let rules: [ConnectionRule]
    let preferences: ConnectionPreferences
    let receipts: [ConnectionReceipt]
    let scopes: [ConnectionScopeOption]
}

struct ConnectionNotesPreview: Decodable {
    let title: String
    let notes: String
}

@MainActor
@Observable
final class ConnectionsStore {
    private(set) var snapshot: ConnectionsSnapshot?
    private(set) var loading = false
    private(set) var busy = false
    private(set) var error: String?
    private(set) var notice: String?
    @ObservationIgnored private let core: Core
    init(core: Core) { self.core = core }

    var connections: [SonaConnection] { snapshot?.connections ?? [] }
    var enabled: [SonaConnection] { connections.filter(\.enabled) }

    func load() async {
        guard !loading else { return }
        loading = true
        defer { loading = false }
        do { snapshot = try await core.request("connections_snapshot"); error = nil }
        catch { self.error = error.localizedDescription }
    }

    func save(_ connection: SonaConnection, token: String, address: String) async -> Bool {
        await change {
            let _: JSONValue = try await self.core.request("connections_save", ["request": [
                "connection": connection.json, "token": .string(token), "webhook_url": .string(address)
            ]] as [String: JSONValue])
        }
    }

    func disconnect(_ connection: SonaConnection) async {
        _ = await change {
            let _: JSONValue = try await self.core.request("connections_disconnect", ["connectionId": .string(connection.id)] as [String: JSONValue])
        }
    }

    func test(_ connection: SonaConnection) async {
        _ = await change {
            self.notice = try await self.core.request("connections_test", ["connectionId": .string(connection.id)] as [String: JSONValue])
        }
    }

    func preferences(email: Bool, calendar: Bool) async {
        _ = await change {
            let _: JSONValue = try await self.core.request("connections_preferences_save", ["preferences": [
                "email_drafts_enabled": .bool(email), "calendar_actions_enabled": .bool(calendar)
            ]] as [String: JSONValue])
        }
    }

    func addRule(connectionId: String, scope: ConnectionScope) async -> Bool {
        await change {
            let _: JSONValue = try await self.core.request("connections_rule_save", ["rule": [
                "id": .string(UUID().uuidString.lowercased()), "connection_id": .string(connectionId),
                "scope": scope.json, "enabled": true, "created_at_utc_ms": 0
            ]] as [String: JSONValue])
        }
    }

    func removeRule(_ rule: ConnectionRule) async {
        _ = await change {
            let _: JSONValue = try await self.core.request("connections_rule_delete", ["ruleId": .string(rule.id)] as [String: JSONValue])
        }
    }

    func preview(sessionId: String) async throws -> ConnectionNotesPreview {
        try await core.request("connections_preview_notes", ["sessionId": .string(sessionId)] as [String: JSONValue])
    }

    func send(connection: SonaConnection, sessionId: String, operationId: String) async -> ConnectionReceipt? {
        var receipt: ConnectionReceipt?
        _ = await change {
            let result: ConnectionReceipt = try await self.core.request("connections_send_notes", ["request": [
                "connection_id": .string(connection.id), "session_id": .string(sessionId), "operation_id": .string(operationId)
            ]] as [String: JSONValue])
            self.notice = result.detail
            receipt = result
        }
        return receipt
    }

    func undo(_ receipt: ConnectionReceipt) async {
        _ = await change {
            let _: JSONValue = try await self.core.request("connections_undo_send", ["receiptId": .string(receipt.id)] as [String: JSONValue])
        }
    }

    private func change(_ work: () async throws -> Void) async -> Bool {
        guard !busy else { return false }
        busy = true; error = nil; notice = nil
        defer { busy = false }
        do { try await work(); await load(); return true }
        catch { self.error = error.localizedDescription; return false }
    }
}
