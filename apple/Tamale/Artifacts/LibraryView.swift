// Every artifact saved on this iPhone (readable offline, kept after sessions end), and a
// session's gallery: what tamale saved for this project, across sessions.

import SwiftData
import SwiftUI
import TamaleKit

struct LibraryView: View {
    @Query(sort: \StoredArtifact.created, order: .reverse) private var artifacts: [StoredArtifact]
    @Environment(\.modelContext) private var context
    @State private var search = ""
    @State private var showing: ArtifactSource?

    private var groups: [(String, [StoredArtifact])] {
        let shown = search.isEmpty ? artifacts : artifacts.filter {
            $0.title.localizedCaseInsensitiveContains(search) || $0.sessionTitle.localizedCaseInsensitiveContains(search)
                || $0.project.localizedCaseInsensitiveContains(search)
        }
        var order: [String] = []
        var byProject: [String: [StoredArtifact]] = [:]
        for artifact in shown {
            if byProject[artifact.project] == nil { order.append(artifact.project) }
            byProject[artifact.project, default: []].append(artifact)
        }
        return order.map { ($0, byProject[$0]!) }
    }

    var body: some View {
        List {
            if artifacts.isEmpty {
                ContentUnavailableView(
                    "No artifacts yet", systemImage: "sparkles",
                    description: Text("Artifacts your sessions make are saved here as you open them, and stay readable offline."))
                    .listRowBackground(Color.clear)
            }
            ForEach(groups, id: \.0) { project, items in
                Section(project) {
                    ForEach(items) { artifact in
                        Button {
                            showing = ArtifactSource(title: artifact.title, html: artifact.html, hash: artifact.contentHash)
                        } label: {
                            VStack(alignment: .leading, spacing: 3) {
                                Text(artifact.title).font(.body.weight(.medium)).foregroundStyle(Theme.bright)
                                HStack {
                                    Text(artifact.sessionTitle).lineLimit(1)
                                    Spacer()
                                    Text(artifact.created, format: .relative(presentation: .named))
                                }
                                .font(.caption)
                                .foregroundStyle(Theme.faint)
                            }
                        }
                        .listRowBackground(Theme.surface)
                    }
                    .onDelete { offsets in
                        for i in offsets { context.delete(items[i]) }
                        try? context.save()
                    }
                }
            }
        }
        .scrollContentBackground(.hidden)
        .background(Theme.background)
        .searchable(text: $search, prompt: "Search artifacts")

        .navigationTitle("Library")
        .fullScreenCover(item: $showing) { source in
            ArtifactViewer(source: source, revise: nil)
        }
    }
}

/// The `artifacts` gallery for one session's project.
struct GalleryView: View {
    var model: SessionModel
    /// Puts a prompt in the session's composer.
    var revise: (String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var showing: (ArtifactSource, Gallery.Item, Bool)?
    @State private var waiting: (Gallery.Item, Bool)?

    var body: some View {
        NavigationStack {
            List {
                if let gallery = model.gallery {
                    if gallery.sessions.isEmpty {
                        ContentUnavailableView("No saved artifacts", systemImage: "sparkles",
                                               description: Text("This project's sessions haven't made any artifacts yet."))
                    }
                    ForEach(Array(gallery.sessions.enumerated()), id: \.offset) { _, group in
                        Section(group.current ? "This session" : (group.title.isEmpty ? "Earlier session" : group.title)) {
                            ForEach(group.items, id: \.self) { item in
                                Button {
                                    open(item, current: group.current)
                                } label: {
                                    HStack {
                                        VStack(alignment: .leading, spacing: 3) {
                                            Text(item.title).foregroundStyle(Theme.bright)
                                            Text(Date(timeIntervalSince1970: item.modified), format: .relative(presentation: .named))
                                                .font(.caption).foregroundStyle(Theme.faint)
                                        }
                                        Spacer()
                                        if waiting?.0 == item { ProgressView() }
                                    }
                                }
                            }
                        }
                    }
                } else {
                    HStack {
                        Spacer()
                        ProgressView()
                        Spacer()
                    }
                    .listRowBackground(Color.clear)
                }
            }
            .navigationTitle("Gallery")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Done") { dismiss() } }
            }
            .onAppear { model.requestGallery() }
            .onChange(of: model.pages) {
                guard let (item, current) = waiting, let page = model.pages[item.id], let html = page.html else { return }
                waiting = nil
                showing = (ArtifactSource(title: item.title, html: html, hash: page.hash ?? item.hash), item, current)
            }
            .fullScreenCover(isPresented: Binding(get: { showing != nil }, set: { if !$0 { showing = nil } })) {
                if let (source, item, current) = showing {
                    ArtifactViewer(
                        source: source,
                        revise: { prompt in
                            revise(prompt)
                            dismiss()
                        },
                        revisePrompt: current ? nil : Prompts.reviseSaved(title: item.title, file: item.file))
                }
            }
        }
    }

    private func open(_ item: Gallery.Item, current: Bool) {
        if let page = model.pages[item.id], let html = page.html {
            showing = (ArtifactSource(title: item.title, html: html, hash: page.hash ?? item.hash), item, current)
        } else {
            waiting = (item, current)
            model.requestArtifact(item.id)
        }
    }
}
