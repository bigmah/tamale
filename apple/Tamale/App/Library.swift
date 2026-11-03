// The artifact library (SwiftData, in the app's container, which the data-protection
// entitlement keeps encrypted while the phone is locked). It's a cache: keys and their names
// live in the Keychain, so the store can be rebuilt from scratch if it ever won't open.

import Foundation
import SwiftData

enum LibraryStore {
    static func open() -> ModelContainer {
        do {
            return try ModelContainer(for: StoredArtifact.self)
        } catch {
            log.error("opening the library: \(error); starting it again")
        }
        // The store couldn't open (a schema change that can't migrate, say): start afresh.
        let config = ModelConfiguration()
        for suffix in ["", "-shm", "-wal"] {
            try? FileManager.default.removeItem(at: URL(fileURLWithPath: config.url.path + suffix))
        }
        if let container = try? ModelContainer(for: StoredArtifact.self, configurations: config) {
            return container
        }
        return try! ModelContainer(for: StoredArtifact.self, configurations: ModelConfiguration(isStoredInMemoryOnly: true))
    }
}

@Model
final class StoredArtifact {
    /// SHA-256 of the document as the model wrote it (tamale's `hash`; not named `hash`,
    /// which NSObject has).
    @Attribute(.unique) var contentHash: String
    var title: String
    /// The page as tamale served it.
    var html: String
    var sessionTitle: String
    var host: String
    var cwd: String
    var sid: String
    var hub: String
    var created: Date

    init(contentHash: String, title: String, html: String, sessionTitle: String, host: String, cwd: String,
         sid: String, hub: String, created: Date = .now) {
        self.contentHash = contentHash
        self.title = title
        self.html = html
        self.sessionTitle = sessionTitle
        self.host = host
        self.cwd = cwd
        self.sid = sid
        self.hub = hub
        self.created = created
    }

    /// Where it came from, for grouping: `host:cwd`, or the hub when unknown.
    var project: String {
        if cwd.isEmpty { return host.isEmpty ? hub : host }
        return host.isEmpty ? cwd : "\(host):\(cwd)"
    }
}
