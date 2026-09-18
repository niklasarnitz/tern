import Foundation
import SwiftUI

@main
struct TernApp: App {
    @StateObject private var store = MailStore()

    var body: some Scene {
        WindowGroup {
            MailRootView(store: store)
                .task {
                    await store.start()
                }
        }
        .commands {
            CommandGroup(after: .sidebar) {
                Button("Reload Cache") {
                    Task { await store.refresh() }
                }
                .keyboardShortcut("r", modifiers: [.command])
                .disabled(!store.canRefresh)
            }
        }
    }
}

private struct MailRootView: View {
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
    }
}

private struct MailSidebar: View {
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

private struct MessageListView: View {
    @ObservedObject var store: MailStore

    var body: some View {
        Group {
            if store.isLoadingMessages, store.messages.isEmpty {
                ProgressView("Loading messages…")
            } else if store.mailboxes.isEmpty {
                ContentUnavailableView("Select an Account", systemImage: "sidebar.left")
            } else if store.messages.isEmpty, !store.isLoadingMessages {
                ContentUnavailableView("No Messages", systemImage: "tray")
            } else {
                List(store.messages, id: \.id, selection: $store.selectedMessageID) { message in
                    MessageRow(message: message)
                        .tag(message.id as String?)
                }
                .listStyle(.inset)
            }
        }
        .navigationTitle(store.selectedMailbox?.displayName ?? "Mail")
        .toolbar {
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
        return "Messages \(first)–\(last)"
    }
}

private struct MessageRow: View {
    let message: MessageSummary

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Circle()
                .fill(message.isRead ? .clear : Color.accentColor)
                .frame(width: 8, height: 8)
                .padding(.top, 6)

            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: 8) {
                    Text(message.sender.isEmpty ? "Unknown sender" : message.sender)
                        .fontWeight(message.isRead ? .regular : .semibold)
                        .lineLimit(1)
                    Spacer(minLength: 8)
                    Text(message.date)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
                HStack(spacing: 6) {
                    Text(message.subject.isEmpty ? "(No subject)" : message.subject)
                        .fontWeight(message.isRead ? .regular : .semibold)
                        .lineLimit(1)
                    if message.isStarred {
                        Image(systemName: "star.fill")
                            .foregroundStyle(.yellow)
                            .font(.caption)
                    }
                    if message.hasAttachments {
                        Image(systemName: "paperclip")
                            .foregroundStyle(.secondary)
                            .font(.caption)
                    }
                }
                if !message.snippet.isEmpty {
                    Text(message.snippet)
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                }
            }
        }
        .padding(.vertical, 4)
        .accessibilityElement(children: .combine)
    }
}

private struct MessageDetailView: View {
    let message: MessageSummary?

    var body: some View {
        if let message {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    Text(message.subject.isEmpty ? "(No subject)" : message.subject)
                        .font(.title2.weight(.semibold))
                    VStack(alignment: .leading, spacing: 4) {
                        Text(message.sender.isEmpty ? "Unknown sender" : message.sender)
                            .font(.headline)
                        Text(message.date)
                            .font(.subheadline)
                            .foregroundStyle(.secondary)
                    }
                    Divider()
                    if message.snippet.isEmpty {
                        Text("This message has no locally stored preview.")
                            .foregroundStyle(.secondary)
                    } else {
                        Text(message.snippet)
                            .textSelection(.enabled)
                    }
                }
                .frame(maxWidth: 760, alignment: .leading)
                .padding(32)
            }
            .navigationTitle(message.subject.isEmpty ? "Message" : message.subject)
        } else {
            ContentUnavailableView("No Message Selected", systemImage: "envelope")
        }
    }
}

private struct ErrorOverlay: View {
    let message: String
    let retry: () -> Void

    var body: some View {
        VStack(spacing: 12) {
            Image(systemName: "exclamationmark.triangle")
                .font(.title2)
            Text(message)
                .multilineTextAlignment(.center)
                .foregroundStyle(.secondary)
            Button("Retry", action: retry)
                .keyboardShortcut(.defaultAction)
        }
        .padding(24)
        .frame(maxWidth: 360)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 12))
        .shadow(radius: 12)
    }
}
