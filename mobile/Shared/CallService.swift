import Foundation
import Security

struct CallAccount: Codable {
    var accountSID: String
    var authToken: String
    var fromNumber: String
    var ownNumber: String
}

enum CallServiceError: LocalizedError {
    case setup, number, response, keychain, rejected(Int)
    var errorDescription: String? {
        switch self {
        case .setup: return NSLocalizedString("calls.setupRequired", comment: "")
        case .number: return NSLocalizedString("calls.invalidNumber", comment: "")
        case .response: return NSLocalizedString("calls.invalidResponse", comment: "")
        case .keychain: return NSLocalizedString("calls.keychainFailed", comment: "")
        case .rejected(let status): return String(format: NSLocalizedString("calls.rejected %d", comment: ""), status)
        }
    }
}

/// Separate from vault identity; disabling call service removes no pairing keys.
enum CallKeychain {
    private static var query: [String: Any] {
        [kSecClass as String: kSecClassGenericPassword,
         kSecAttrService as String: "com.aktanazat.sona.mobile.calls",
         kSecAttrAccount as String: "twilio-v1"]
    }
    static func load() throws -> CallAccount? {
        var query = query
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess, let bytes = result as? Data else { throw CallServiceError.keychain }
        return try JSONDecoder().decode(CallAccount.self, from: bytes)
    }
    static func save(_ account: CallAccount) throws {
        let attributes: [String: Any] = [kSecValueData as String: try JSONEncoder().encode(account),
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly]
        let status = SecItemUpdate(query as CFDictionary, attributes as CFDictionary)
        if status == errSecItemNotFound {
            let insert = query.merging(attributes) { _, replacement in replacement }
            guard SecItemAdd(insert as CFDictionary, nil) == errSecSuccess else { throw CallServiceError.keychain }
        } else if status != errSecSuccess { throw CallServiceError.keychain }
    }
    static func remove() throws {
        let status = SecItemDelete(query as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else { throw CallServiceError.keychain }
    }
}

struct ServiceCall: Codable, Identifiable {
    var sid: String
    var status: String
    var id: String { sid }
}
struct ServiceRecording: Decodable, Identifiable {
    var sid: String
    var status: String
    var duration: String?
    var id: String { sid }
}
struct ServiceRecordings: Decodable { var recordings: [ServiceRecording] }

struct CallService {
    private let account: CallAccount
    private static let session = URLSession(configuration: .ephemeral)
    init(account: CallAccount) throws {
        guard Self.validSID(account.accountSID, prefix: "AC"), !account.authToken.isEmpty else { throw CallServiceError.setup }
        _ = try Self.phoneNumber(account.fromNumber)
        _ = try Self.phoneNumber(account.ownNumber)
        self.account = account
    }

    static func validSID(_ value: String, prefix: String) -> Bool {
        value.count == 34 && value.hasPrefix(prefix) && value.dropFirst(2).allSatisfy { $0.isASCII && $0.isHexDigit }
    }
    static func phoneNumber(_ value: String) throws -> String {
        let number = value.filter { !" ()-.".contains($0) }
        guard number.first == "+", (8...16).contains(number.count),
              number.dropFirst().allSatisfy({ $0.isASCII && $0.isNumber }), number.dropFirst().first != "0"
        else { throw CallServiceError.number }
        return number
    }
    static func callParameters(account: CallAccount, target: String) throws -> [(String, String)] {
        let target = try phoneNumber(target)
        let from = try phoneNumber(account.fromNumber)
        let own = try phoneNumber(account.ownNumber)
        // Only validated E.164 digits enter XML. Recording consent is confirmed before this request.
        let xml = "<Response><Say>This call will be recorded. Tell everyone before discussing private information.</Say><Dial callerId=\"\(from)\" record=\"record-from-answer-dual\"><Number>\(target)</Number></Dial></Response>"
        return [("From", from), ("To", own), ("Twiml", xml), ("Timeout", "30")]
    }
    static func form(_ fields: [(String, String)]) throws -> Data {
        let allowed = CharacterSet(charactersIn: "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-._~")
        return Data(try fields.map { name, value in
            guard let key = name.addingPercentEncoding(withAllowedCharacters: allowed),
                  let value = value.addingPercentEncoding(withAllowedCharacters: allowed) else { throw CallServiceError.response }
            return "\(key)=\(value)"
        }.joined(separator: "&").utf8)
    }
    func start(target: String) async throws -> ServiceCall {
        let bytes = try await data(path: "Calls.json", method: "POST", fields: Self.callParameters(account: account, target: target))
        let call = try JSONDecoder().decode(ServiceCall.self, from: bytes)
        guard Self.validSID(call.sid, prefix: "CA") else { throw CallServiceError.response }
        return call
    }
    func call(_ sid: String) async throws -> ServiceCall {
        guard Self.validSID(sid, prefix: "CA") else { throw CallServiceError.response }
        return try JSONDecoder().decode(ServiceCall.self, from: await data(path: "Calls/\(sid).json"))
    }
    func end(_ sid: String) async throws {
        guard Self.validSID(sid, prefix: "CA") else { throw CallServiceError.response }
        _ = try await data(path: "Calls/\(sid).json", method: "POST", fields: [("Status", "completed")])
    }
    func recordings(_ sid: String) async throws -> [ServiceRecording] {
        guard Self.validSID(sid, prefix: "CA") else { throw CallServiceError.response }
        return try JSONDecoder().decode(ServiceRecordings.self, from: await data(path: "Calls/\(sid)/Recordings.json")).recordings
    }
    func download(_ sid: String) async throws -> URL {
        guard Self.validSID(sid, prefix: "RE") else { throw CallServiceError.response }
        let request = try request(path: "Recordings/\(sid).wav", method: "GET", fields: [])
        let (url, response) = try await Self.session.download(for: request)
        guard let response = response as? HTTPURLResponse, (200...299).contains(response.statusCode) else {
            try? FileManager.default.removeItem(at: url)
            throw CallServiceError.response
        }
        return url
    }
    private func request(path: String, method: String, fields: [(String, String)]) throws -> URLRequest {
        guard let base = URL(string: "https://api.twilio.com/2010-04-01/Accounts/\(account.accountSID)/") else { throw CallServiceError.setup }
        var request = URLRequest(url: base.appending(path: path))
        request.httpMethod = method
        request.setValue("Basic " + Data("\(account.accountSID):\(account.authToken)".utf8).base64EncodedString(), forHTTPHeaderField: "Authorization")
        if method == "POST" {
            request.httpBody = try Self.form(fields)
            request.setValue("application/x-www-form-urlencoded", forHTTPHeaderField: "Content-Type")
        }
        return request
    }
    private func data(path: String, method: String = "GET", fields: [(String, String)] = []) async throws -> Data {
        let (bytes, response) = try await Self.session.data(for: request(path: path, method: method, fields: fields))
        guard let response = response as? HTTPURLResponse else { throw CallServiceError.response }
        guard (200...299).contains(response.statusCode) else { throw CallServiceError.rejected(response.statusCode) }
        return bytes
    }
}
