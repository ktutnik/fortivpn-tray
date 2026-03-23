import Foundation

struct VpnProfile: Codable, Identifiable, Hashable {
    let id: String
    var name: String
    var host: String
    var port: Int
    var username: String
    var trustedCert: String
    var hasPassword: Bool = false

    enum CodingKeys: String, CodingKey {
        case id, name, host, port, username
        case trustedCert = "trusted_cert"
    }
}

struct IpcResponse: Codable {
    let ok: Bool
    let message: String
    let data: JSONValue?
}

struct StatusResponse: Codable {
    let status: String
    let profile: String?
}

// Generic JSON value for parsing arbitrary response data
enum JSONValue: Codable, Hashable {
    case string(String)
    case int(Int)
    case double(Double)
    case bool(Bool)
    case array([JSONValue])
    case object([String: JSONValue])
    case null

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if let v = try? container.decode(Bool.self) { self = .bool(v) }
        else if let v = try? container.decode(Int.self) { self = .int(v) }
        else if let v = try? container.decode(Double.self) { self = .double(v) }
        else if let v = try? container.decode(String.self) { self = .string(v) }
        else if let v = try? container.decode([JSONValue].self) { self = .array(v) }
        else if let v = try? container.decode([String: JSONValue].self) { self = .object(v) }
        else { self = .null }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .string(let v): try container.encode(v)
        case .int(let v): try container.encode(v)
        case .double(let v): try container.encode(v)
        case .bool(let v): try container.encode(v)
        case .array(let v): try container.encode(v)
        case .object(let v): try container.encode(v)
        case .null: try container.encodeNil()
        }
    }
}
