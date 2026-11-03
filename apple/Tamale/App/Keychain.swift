// Keys live only in the Keychain, readable on this device only while it's unlocked
// (`kSecAttrAccessibleWhenUnlockedThisDeviceOnly`), never in backups or iCloud. Each item
// also carries the key's name and hub, so nothing else needs to survive for the app to work.

import Foundation
import Security
import TamaleKit

struct StoredKeyRecord: Codable {
    var name: String
    var hub: String
    var added: Date
}

enum Keychain {
    static let service = "com.radkey.tamale.keys"

    static func save(_ key: TamaleKey, record: StoredKeyRecord) throws {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: key.id,
        ]
        SecItemDelete(query as CFDictionary)
        var add = query
        add[kSecValueData as String] = Data(key.text.utf8)
        add[kSecAttrGeneric as String] = try JSONEncoder().encode(record)
        add[kSecAttrAccessible as String] = kSecAttrAccessibleWhenUnlockedThisDeviceOnly
        let status = SecItemAdd(add as CFDictionary, nil)
        guard status == errSecSuccess else { throw KeychainError(status: status) }
    }

    /// Every key, oldest first.
    static func all() -> [(TamaleKey, StoredKeyRecord)] {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecReturnData as String: true,
            kSecReturnAttributes as String: true,
            kSecMatchLimit as String: kSecMatchLimitAll,
        ]
        var out: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &out) == errSecSuccess,
              let items = out as? [[String: Any]]
        else { return [] }
        return items.compactMap { item -> (TamaleKey, StoredKeyRecord)? in
            guard let data = item[kSecValueData as String] as? Data,
                  let key = TamaleKey(String(decoding: data, as: UTF8.self)),
                  let generic = item[kSecAttrGeneric as String] as? Data,
                  let record = try? JSONDecoder().decode(StoredKeyRecord.self, from: generic)
            else { return nil }
            return (key, record)
        }
        .sorted { $0.1.added < $1.1.added }
    }

    static func delete(id: String) {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: id,
        ]
        SecItemDelete(query as CFDictionary)
    }
}

struct KeychainError: LocalizedError {
    var status: OSStatus
    var errorDescription: String? {
        "The Keychain refused the key (\(SecCopyErrorMessageString(status, nil) as String? ?? "\(status)"))."
    }
}
