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
            MessageDetailView(
                message: store.selectedMessage,
                details: store.selectedMessageDetails,
                isLoadingDetails: store.isLoadingMessageDetails
            )
            .task(id: store.selectedMessageID) {
                await store.loadSelectedMessageDetails()
            }
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
    let details: MessageDetails?
    let isLoadingDetails: Bool
    @State private var detailsExpanded = false

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
                    DisclosureGroup(isExpanded: $detailsExpanded) {
                        Group {
                            if isLoadingDetails {
                                ProgressView("Loading message details…")
                                    .controlSize(.small)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                                    .padding(.vertical, 8)
                            } else if let details {
                                MessageDetailsPanel(message: message, details: details)
                            } else {
                                Text("No additional details are cached for this message.")
                                    .foregroundStyle(.secondary)
                                    .padding(.vertical, 8)
                            }
                        }
                        .padding(.top, 8)
                    } label: {
                        Label("Message Details", systemImage: "info.circle")
                            .font(.subheadline.weight(.medium))
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
            .id(message.id)
            .navigationTitle(message.subject.isEmpty ? "Message" : message.subject)
        } else {
            ContentUnavailableView("No Message Selected", systemImage: "envelope")
        }
    }
}

private struct MessageDetailsPanel: View {
    let message: MessageSummary
    let details: MessageDetails

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            detailSection("People") {
                DetailRow(label: "From", values: senderValues, emptyValue: "Unknown sender")
                DetailRow(label: "To", values: details.recipients)
                DetailRow(label: "Cc", values: details.cc)
                DetailRow(label: "Bcc", values: details.bcc)
                DetailRow(label: "Reply-To", values: details.replyTo)
            }

            detailSection("Timestamps") {
                DetailRow(label: "Received", values: [message.date])
                DetailRow(label: "Sent", values: sentTimestamp.map { [$0] } ?? [])
            }

            detailSection("Mailing List") {
                DetailRow(label: "List-ID", values: details.listId)
                DetailRow(label: "Post", values: details.listPost)
                DetailRow(label: "Unsubscribe", values: details.listUnsubscribe)
            }

            detailSection("Security") {
                SecurityStatus(details: details)
                DetailRow(label: "Authentication-Results", values: details.authenticationResults)
                DetailRow(label: "Received-SPF", values: details.receivedSpf)
            }

            detailSection("Attachments") {
                if details.attachments.isEmpty {
                    Text("None")
                        .foregroundStyle(.secondary)
                } else {
                    ForEach(details.attachments, id: \.id) { attachment in
                        HStack(spacing: 10) {
                            Image(systemName: "paperclip")
                                .foregroundStyle(.secondary)
                            VStack(alignment: .leading, spacing: 2) {
                                Text(attachment.filename)
                                    .textSelection(.enabled)
                                Text("\(attachment.mimeType) · \(formattedSize(attachment.size))")
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                        }
                    }
                }
            }

            detailSection("Headers") {
                DetailRow(
                    label: "Message-ID",
                    values: details.messageId.map { [$0] } ?? []
                )
                DetailRow(label: "In-Reply-To", values: details.inReplyTo)
                DetailRow(label: "References", values: details.references)
            }
        }
    }

    private var sentTimestamp: String? {
        details.sentAt.map {
            Date(timeIntervalSince1970: TimeInterval($0))
                .formatted(date: .abbreviated, time: .complete)
        }
    }

    private var senderValues: [String] {
        if !details.senders.isEmpty {
            return details.senders
        }
        return message.sender.isEmpty ? [] : [message.sender]
    }

    private func formattedSize(_ size: UInt64) -> String {
        ByteCountFormatter.string(fromByteCount: Int64(clamping: size), countStyle: .file)
    }

    private func detailSection<Content: View>(
        _ title: String,
        @ViewBuilder content: () -> Content
    ) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title)
                .font(.caption.weight(.semibold))
                .foregroundStyle(.secondary)
                .textCase(.uppercase)
            content()
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct DetailRow: View {
    let label: String
    let values: [String]
    var emptyValue = "Not available"

    var body: some View {
        Grid(alignment: .topLeading, horizontalSpacing: 12) {
            GridRow {
                Text(label)
                    .foregroundStyle(.secondary)
                    .frame(width: 112, alignment: .trailing)
                Text(displayValue)
                    .foregroundStyle(values.isEmpty ? .secondary : .primary)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .font(.subheadline)
    }

    private var displayValue: String {
        values.isEmpty ? emptyValue : values.joined(separator: "\n")
    }
}

private struct SecurityStatus: View {
    let details: MessageDetails

    var body: some View {
        Label(status.text, systemImage: status.icon)
            .foregroundStyle(status.color)
            .font(.subheadline.weight(.medium))
            .accessibilityLabel("Security status: \(status.text)")
    }

    private var status: (text: String, icon: String, color: Color) {
        let authentication = details.authenticationResults.joined(separator: " ").lowercased()
        let receivedSPF = details.receivedSpf.map { $0.lowercased().trimmingCharacters(in: .whitespaces) }
        let hasFailure = ["dkim=fail", "spf=fail", "dmarc=fail", "softfail"]
            .contains(where: { authentication.contains($0) }) || receivedSPF.contains(where: { value in
                value.hasPrefix("fail") || value.hasPrefix("softfail")
            })
        if hasFailure {
            return ("Headers report an authentication failure", "exclamationmark.shield", .red)
        }
        let hasPass = ["dkim=pass", "spf=pass", "dmarc=pass"]
            .contains(where: { authentication.contains($0) }) || receivedSPF.contains(where: { $0.hasPrefix("pass") })
        if hasPass {
            return ("Headers report authentication passed", "checkmark.shield", .green)
        }
        return ("Authentication status unavailable", "shield.lefthalf.filled", .secondary)
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
