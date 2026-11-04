// The app's state: the keys, every key's sessions, the open session models, and the
// library. Views read it from the environment.

import Foundation
import Observation
import SwiftData
import TamaleKit

/// One key on one hub.
struct Account: Identifiable, Hashable {
    var id: String { key.id }
    var name: String
    var hub: URL
    var key: TamaleKey
    var client: HubClient

    static func == (a: Account, b: Account) -> Bool { a.id == b.id && a.name == b.name && a.hub == b.hub }
    func hash(into hasher: inout Hasher) { hasher.combine(id) }

    var hubName: String { hub.host ?? hub.absoluteString }
}

/// A session in the list, with its label opened.
struct SessionRow: Identifiable {
    var id: String { session.sid }
    var account: Account
    var session: HubSession
    var label: SessionLabel
    var title: String
    var model: String

    init(account: Account, session: HubSession) {
        self.account = account
        self.session = session
        let shown = session.shown(with: account.key)
        label = shown.label
        title = shown.title
        model = shown.model
    }

    /// `host:~/dir`, as the hub's page shows it.
    var place: String {
        switch (label.host.isEmpty, label.cwd.isEmpty) {
        case (false, false): "\(label.host):\(label.cwd)"
        case (false, true): label.host
        case (true, false): label.cwd
        default: String(session.sid.prefix(8))
        }
    }

    var shownTitle: String { title.isEmpty ? "New session" : title }
}

enum Route: Hashable {
    case session(String)
    case library
    case keys
}

@MainActor
@Observable
final class AppModel {
    private(set) var accounts: [Account] = []
    private(set) var rows: [SessionRow] = []
    /// Problems listing each account's sessions, by account id.
    private(set) var problems: [String: String] = [:]
    private(set) var refreshing = false
    private(set) var loaded = false
    var path: [Route] = []
    /// A link that couldn't be opened, to explain.
    var linkProblem: String?
    /// A session link scanned in the Add sheet, opened once the sheet has gone: SwiftUI drops
    /// navigation and alerts started while a sheet is still dismissing.
    var pendingLink: SessionLink?
    /// Sessions being closed, by sid.
    private(set) var closing: Set<String> = []
    /// Why a session couldn't be closed, to explain.
    var closeProblem: String?
    /// Sessions closed from here, which a hub from before `end` still lists offline for a while.
    @ObservationIgnored private var closed: Set<String> = []
    @ObservationIgnored private var models: [String: SessionModel] = [:]
    @ObservationIgnored private var context: ModelContext?

    // MARK: - Keys

    func load(_ context: ModelContext) {
        guard !loaded else { return }
        self.context = context
        #if DEBUG
        if ProcessInfo.processInfo.environment["TAMALE_DEBUG_RESET"] != nil {
            for (key, _) in Keychain.all() { Keychain.delete(id: key.id) }
        }
        #endif
        accounts = Keychain.all().compactMap { key, record in
            guard let hub = URL(string: record.hub) else { return nil }
            return Account(name: record.name, hub: hub, key: key, client: HubClient(origin: hub, key: key))
        }
        loaded = true
        #if DEBUG
        // UI tests (apple/scripts/uitest.sh) start with a key for a local hub.
        let env = ProcessInfo.processInfo.environment
        if let text = env["TAMALE_DEBUG_KEY"], let key = TamaleKey(text),
           let hub = env["TAMALE_DEBUG_HUB"].flatMap(URL.init(string:)), !accounts.contains(where: { $0.id == key.id }) {
            Task {
                do { try await add(key, hub: hub, name: "Test hub") }
                catch { linkProblem = "Test key: \(error)" }
            }
        }
        #endif
    }

    /// Adds a key once the hub confirms it knows it.
    func add(_ key: TamaleKey, hub: URL, name: String) async throws {
        let hub = hub.origin ?? hub
        let client = HubClient(origin: hub, key: key)
        try await client.verify()
        try Keychain.save(key, record: StoredKeyRecord(name: name, hub: hub.absoluteString, added: .now))
        accounts.removeAll { $0.id == key.id }
        accounts.append(Account(name: name, hub: hub, key: key, client: client))
        await refresh()
    }

    func remove(_ account: Account) {
        Keychain.delete(id: account.id)
        accounts.removeAll { $0.id == account.id }
        for row in rows where row.account.id == account.id {
            models.removeValue(forKey: row.id)?.stop()
        }
        rows.removeAll { $0.account.id == account.id }
        problems[account.id] = nil
    }

    func rename(_ account: Account, to name: String) {
        guard let index = accounts.firstIndex(of: account) else { return }
        accounts[index].name = name
        let added = Keychain.all().first { $0.0.id == account.id }?.1.added ?? .now
        try? Keychain.save(account.key, record: StoredKeyRecord(name: name, hub: account.hub.absoluteString, added: added))
    }

    // MARK: - Sessions

    func refresh() async {
        refreshing = true
        defer { refreshing = false }
        var all: [SessionRow] = []
        await withTaskGroup(of: (Account, Result<SessionList, Error>).self) { group in
            for account in accounts {
                group.addTask {
                    do { return (account, .success(try await account.client.sessions())) }
                    catch { return (account, .failure(error)) }
                }
            }
            for await (account, result) in group {
                switch result {
                case .success(let list):
                    problems[account.id] = nil
                    all += list.sessions
                        .filter { $0.online || !closed.contains($0.sid) }
                        .map { SessionRow(account: account, session: $0) }
                case .failure(let error):
                    problems[account.id] = error.localizedDescription
                    // Keep showing what was there.
                    all += rows.filter { $0.account.id == account.id }
                }
            }
        }
        // The hub's order: waiting for approval, then busy, then online, then recent.
        rows = all.sorted {
            ($0.session.approval ? 0 : 1, $0.session.busy ? 0 : 1, $0.session.online ? 0 : 1, $0.session.idle)
                < ($1.session.approval ? 0 : 1, $1.session.busy ? 0 : 1, $1.session.online ? 0 : 1, $1.session.idle)
        }
    }

    func row(_ sid: String) -> SessionRow? { rows.first { $0.id == sid } }

    /// Ends a session on its machine, as `tamale kill` would there; it stays saved, so
    /// `tamale --continue` picks it back up. Leaves it if it's open here.
    func close(_ row: SessionRow) async {
        guard !closing.contains(row.id) else { return }
        closing.insert(row.id)
        defer { closing.remove(row.id) }
        let model = model(for: row)
        do {
            try await model.close()
        } catch {
            // Only connected for this: nothing to keep it going for.
            if !path.contains(.session(row.id)) { models.removeValue(forKey: row.id)?.stop() }
            closeProblem = error.localizedDescription
            return
        }
        closed.insert(row.id)
        path.removeAll { $0 == .session(row.id) }
        models.removeValue(forKey: row.id)?.stop()
        rows.removeAll { $0.id == row.id }
        await refresh()
    }

    /// The session's model, made on first use and kept so coming back is instant.
    func model(for row: SessionRow) -> SessionModel {
        if let model = models[row.id] { return model }
        let channel = SessionChannel(hub: row.account.client, sid: row.id)
        let model = SessionModel(channel: channel)
        model.onArtifact = { [weak self] reply in
            self?.save(reply, from: row, model: model)
        }
        models[row.id] = model
        return model
    }

    /// Whether a key on this iPhone is for `hub`.
    func hasKey(for hub: URL) -> Bool {
        accounts.contains { $0.hubName == hub.host }
    }

    /// Opens the link the Add sheet left, if any; call once the sheet is gone.
    func openPending() async {
        guard let link = pendingLink else { return }
        pendingLink = nil
        await open(link)
    }

    /// A terminal's `/open/<sid>` link: opens the session if one of the keys has it.
    func open(_ link: SessionLink) async {
        if row(link.sid) == nil { await refresh() }
        guard row(link.sid) != nil else {
            let hubs = Set(accounts.map(\.hubName))
            linkProblem = hubs.contains(link.hub.host ?? "")
                ? "None of your keys has that session, or it has ended."
                : "Add a key for \(link.hub.host ?? "that hub") first."
            return
        }
        path = [.session(link.sid)]
    }

    // MARK: - Library

    private func save(_ reply: ArtifactReply, from row: SessionRow, model: SessionModel) {
        guard let context, let html = reply.html else { return }
        let hash = reply.hash ?? Data(html.utf8).sha256Hex
        let title = model.entries.lazy.compactMap { $0.prose.artifacts.first { $0.id == reply.id }?.title }.first
            ?? model.gallery?.sessions.lazy.flatMap(\.items).first { $0.id == reply.id }?.title
            ?? "Artifact"
        let existing = try? context.fetch(FetchDescriptor<StoredArtifact>(predicate: #Predicate { $0.contentHash == hash })).first
        if existing != nil { return }
        context.insert(StoredArtifact(
            contentHash: hash, title: title, html: html, sessionTitle: row.shownTitle, host: row.label.host,
            cwd: row.label.cwd, sid: row.id, hub: row.account.hubName))
        do { try context.save() } catch { log.error("saving an artifact: \(error)") }
    }
}

import CryptoKit
import os

let log = Logger(subsystem: "com.radkey.tamale", category: "app")

extension Data {
    var sha256Hex: String { SHA256.hash(data: self).map { String(format: "%02x", $0) }.joined() }
}
