import Foundation
import SwiftUI

@main
struct TernApp: App {
    @StateObject private var store = MailStore()
    @State private var composeDraft: ComposeDraft?

    var body: some Scene {
        WindowGroup {
            MailRootView(store: store)
                .task {
                    await store.start()
                }
                .onOpenURL { url in
                    if let draft = MailtoURLParser.parse(url) {
                        composeDraft = draft
                    }
                }
                .sheet(item: $composeDraft) { draft in
                    ComposeView(draft: draft)
                }
        }
        .commands {
            CommandGroup(replacing: .undoRedo) {
                Button(store.pendingUndo.map { "Undo \($0.actionLabel)" } ?? "Undo") {
                    Task { await store.undoLastAction() }
                }
                .keyboardShortcut("z", modifiers: [.command])
                .disabled(store.pendingUndo == nil)
            }
            CommandGroup(replacing: .newItem) {
                Button("New Message") {
                    composeDraft = .empty
                }
                .keyboardShortcut("n", modifiers: [.command])
            }
            CommandGroup(after: .sidebar) {
                Button("Reload Cache") {
                    Task { await store.refresh() }
                }
                .keyboardShortcut("r", modifiers: [.command])
                .disabled(!store.canRefresh)
            }
        }
        Settings {
            WidgetSettingsView(store: store)
        }
    }
}

struct WidgetSettingsView: View {
    @ObservedObject var store: MailStore

    var body: some View {
        Form {
            Section("Privacy") {
                Picker("Show on widgets", selection: privacyBinding) {
                    ForEach(WidgetPrivacy.allCases) { privacy in
                        Text(privacy.title).tag(privacy)
                    }
                }
                Text(
                    "Counts only is the default. More private mail content is copied to the widget cache " +
                        "only when you allow it."
                )
                .font(.caption)
                .foregroundStyle(.secondary)
            }

            Section("Selected Mailboxes") {
                if store.mailboxes.isEmpty {
                    Text("Open an account to choose mailboxes.")
                        .foregroundStyle(.secondary)
                } else {
                    ForEach(store.mailboxes, id: \.id) { mailbox in
                        Toggle(
                            mailbox.displayName.isEmpty ? mailbox.remoteName : mailbox.displayName,
                            isOn: mailboxBinding(mailbox.id)
                        )
                    }
                }
                Text("Widgets read only the mailboxes selected here. No mailbox is shared by default.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            if let error = store.widgetErrorMessage {
                Section {
                    Text(error)
                        .foregroundStyle(.red)
                }
            }
        }
        .formStyle(.grouped)
        .frame(width: 480, height: 420)
    }

    private var privacyBinding: Binding<WidgetPrivacy> {
        Binding(
            get: { store.widgetPrivacy },
            set: { privacy in
                Task { await store.setWidgetPrivacy(privacy) }
            }
        )
    }

    private func mailboxBinding(_ mailboxID: String) -> Binding<Bool> {
        Binding(
            get: { store.isMailboxIncludedInWidgets(mailboxID) },
            set: { included in
                Task { await store.setMailbox(mailboxID, includedInWidgets: included) }
            }
        )
    }
}

struct MailRootView: View {
    @ObservedObject var store: MailStore

    var body: some View {
        NavigationSplitView {
            MailSidebar(store: store)
                .navigationSplitViewColumnWidth(min: 220, ideal: 260, max: 340)
        } content: {
            MessageListView(store: store)
                .navigationSplitViewColumnWidth(min: 300, ideal: 420, max: 620)
        } detail: {
            MessageDetailView(message: store.selectedMessage)
        }
        .overlay {
            if let error = store.errorMessage {
                ErrorOverlay(message: error) {
                    Task { await store.retry() }
                }
            }
        }
        .overlay(alignment: .bottom) {
            if let notice = store.pendingUndo {
                UndoBanner(actionLabel: notice.actionLabel) {
                    Task { await store.undoLastAction() }
                }
                .padding(.bottom, 16)
                .transition(.move(edge: .bottom).combined(with: .opacity))
            }
        }
        .animation(.easeInOut(duration: 0.2), value: store.pendingUndo)
    }
}

struct MailSidebar: View {
    @ObservedObject var store: MailStore

    var body: some View {
        List(selection: $store.selectedSidebarItem) {
            Section("Accounts") {
                ForEach(store.accounts, id: \.id) { account in
                    Label(account.displayName.isEmpty ? account.email : account.displayName,
                          systemImage: "person.crop.circle")
                        .tag(SidebarItem.account(account.id))
                        .lineLimit(1)
                        .help(account.email)
                }
            }

            Section("Mailboxes") {
                ForEach(store.mailboxes, id: \.id) { mailbox in
                    Label {
                        Text(mailbox.displayName.isEmpty ? mailbox.remoteName : mailbox.displayName)
                            .lineLimit(1)
                    } icon: {
                        Image(systemName: mailboxIcon(for: mailbox))
                    }
                    .tag(SidebarItem.mailbox(mailbox.id))
                }
            }
        }
        .listStyle(.sidebar)
        .onChange(of: store.selectedSidebarItem) { _, selection in
            Task { await store.handleSidebarSelection(selection) }
        }
        .overlay {
            if store.isLoadingAccounts, store.accounts.isEmpty {
                ProgressView("Loading accounts…")
            } else if !store.isLoading, store.accounts.isEmpty {
                ContentUnavailableView(
                    "No Accounts",
                    systemImage: "tray",
                    description: Text("Add an account to begin.")
                )
            }
        }
        .safeAreaInset(edge: .bottom) {
            if store.isLoadingMailboxes {
                ProgressView()
                    .controlSize(.small)
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 6)
            }
        }
    }

    private func mailboxIcon(for mailbox: Mailbox) -> String {
        switch mailbox.displayName.lowercased() {
        case let name where name.contains("sent"):
            "paperplane"
        case let name where name.contains("trash"), let name where name.contains("bin"):
            "trash"
        case let name where name.contains("draft"):
            "doc"
        case let name where name.contains("archive"):
            "archivebox"
        default:
            "tray"
        }
    }
}

struct MessageListView: View {
    @ObservedObject var store: MailStore

    var body: some View {
        Group {
            if store.isLoadingMessages, store.messages.isEmpty {
                ProgressView("Loading messages…")
            } else if store.mailboxes.isEmpty, !store.isSearching {
                ContentUnavailableView("Select an Account", systemImage: "sidebar.left")
            } else if store.messages.isEmpty, !store.isLoadingMessages {
                if store.isSearching {
                    ContentUnavailableView.search(text: store.activeSearchQuery ?? "")
                } else {
                    ContentUnavailableView("No Messages", systemImage: "tray")
                }
            } else {
                List(store.messages, id: \.id, selection: $store.selectedMessageID) { message in
                    MessageRow(message: message)
                        .tag(message.id as String?)
                        .contextMenu {
                            MessageActionButtons(store: store, message: message)
                        }
                }
                .listStyle(.inset)
            }
        }
        .navigationTitle(
            store.isSearching ? "Search Results" : store.selectedMailbox?.displayName ?? "Mail"
        )
        .searchable(
            text: $store.searchText,
            placement: .toolbar,
            prompt: "Search or use from:, before:, is:unread…"
        )
        .onSubmit(of: .search) {
            Task { await store.submitSearch() }
        }
        .onChange(of: store.searchText) { _, query in
            if query.isEmpty, store.isSearching {
                Task { await store.clearSearch() }
            }
        }
        .toolbar {
            ToolbarItemGroup {
                if let message = store.selectedMessage {
                    MessageActionButtons(store: store, message: message)
                }
            }
            ToolbarItem {
                Button {
                    Task { await store.refresh() }
                } label: {
                    Label("Reload Cache", systemImage: "arrow.clockwise")
                }
                .disabled(!store.canRefresh)
            }
        }
        .safeAreaInset(edge: .bottom) {
            if !store.messages.isEmpty || store.messageOffset > 0 {
                HStack(spacing: 12) {
                    Button {
                        Task { await store.previousMessagePage() }
                    } label: {
                        Label("Previous", systemImage: "chevron.left")
                    }
                    .disabled(store.messageOffset == 0 || store.isLoadingMessages)

                    Text(pageLabel)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .monospacedDigit()

                    Button {
                        Task { await store.nextMessagePage() }
                    } label: {
                        Label("Next", systemImage: "chevron.right")
                    }
                    .disabled(!store.hasNextMessagePage || store.isLoadingMessages)
                }
                .frame(maxWidth: .infinity)
                .padding(.vertical, 8)
                .background(.bar)
            }
        }
    }

    private var pageLabel: String {
        let first = store.messageOffset + 1
        let last = store.messageOffset + UInt32(store.messages.count)
        let noun = store.isSearching ? "Results" : "Messages"
        return "\(noun) \(first)–\(last)"
    }
}

struct MessageActionButtons: View {
    @ObservedObject var store: MailStore
    let message: MessageSummary

    var body: some View {
        if let archive = store.archiveMailbox {
            actionButton("Archive", systemImage: "archivebox", destination: archive, result: "Archived")
        }
        if let trash = store.trashMailbox {
            actionButton("Delete", systemImage: "trash", destination: trash, result: "Deleted")
        }
        if let spam = store.spamMailbox {
            actionButton(
                "Mark as Spam",
                systemImage: "exclamationmark.octagon",
                destination: spam,
                result: "Marked as Spam"
            )
        }
        if !store.moveDestinations.isEmpty {
            Menu {
                ForEach(store.moveDestinations, id: \.id) { mailbox in
                    Button(mailbox.displayName.isEmpty ? mailbox.remoteName : mailbox.displayName) {
                        move(to: mailbox, result: "Moved")
                    }
                }
            } label: {
                Label("Move to", systemImage: "folder")
            }
            .disabled(store.isPerformingAction)
        }
    }

    private func actionButton(
        _ title: String,
        systemImage: String,
        destination: Mailbox,
        result: String
    ) -> some View {
        Button {
            move(to: destination, result: result)
        } label: {
            Label(title, systemImage: systemImage)
        }
        .disabled(store.isPerformingAction)
    }

    private func move(to mailbox: Mailbox, result: String) {
        Task { await store.moveMessage(message, to: mailbox, actionLabel: result) }
    }
}
