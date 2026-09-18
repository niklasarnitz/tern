import SwiftUI

struct MessageRow: View {
    let message: MessageSummary

    private var sender: SenderIdentity {
        SenderIdentity(message.sender)
    }

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Circle()
                .fill(message.isRead ? .clear : Color.accentColor)
                .frame(width: 8, height: 8)
                .padding(.top, 6)

            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: 8) {
                    Text(sender.displayName)
                        .fontWeight(message.isRead ? .regular : .semibold)
                        .lineLimit(1)
                    Spacer(minLength: 8)
                    Text(message.date)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
                HStack(spacing: 5) {
                    Text(sender.address ?? "Sender address unavailable")
                        .lineLimit(1)
                    if let domain = sender.domain {
                        Text("· \(domain)")
                            .lineLimit(1)
                    }
                    if sender.mismatchWarning != nil {
                        Image(systemName: "exclamationmark.triangle.fill")
                            .foregroundStyle(.orange)
                            .accessibilityLabel("Suspicious sender")
                    }
                }
                .font(.caption)
                .foregroundStyle(.secondary)
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

struct MessageDetailView: View {
    let message: MessageSummary?

    var body: some View {
        if let message {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    Text(message.subject.isEmpty ? "(No subject)" : message.subject)
                        .font(.title2.weight(.semibold))
                    SenderDetails(message: message)
                    Divider()
                    UntrustedMessagePreview(text: message.snippet)
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

struct SenderDetails: View {
    let message: MessageSummary

    private var sender: SenderIdentity {
        SenderIdentity(message.sender)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            Text(sender.displayName)
                .font(.headline)
            LabeledContent("Address", value: sender.address ?? "Unavailable")
            LabeledContent("Domain", value: sender.domain ?? "Unavailable")
            Text(message.date)
                .font(.subheadline)
                .foregroundStyle(.secondary)
            if let warning = sender.mismatchWarning {
                Label(warning, systemImage: "exclamationmark.triangle.fill")
                    .foregroundStyle(.orange)
                    .font(.callout.weight(.medium))
                    .padding(.top, 5)
                    .accessibilityLabel("Suspicious sender. \(warning)")
            }
        }
    }
}

struct UntrustedMessagePreview: View {
    let text: String

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Label("Message preview · Untrusted content", systemImage: "shield.lefthalf.filled")
                .font(.caption.weight(.semibold))
                .foregroundStyle(.secondary)
            if text.isEmpty {
                Text("This message has no locally stored preview.")
                    .foregroundStyle(.secondary)
            } else {
                Text(text)
                    .textSelection(.enabled)
                ExternalLinksView(text: text)
            }
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.quaternary.opacity(0.35), in: RoundedRectangle(cornerRadius: 10))
        .overlay {
            RoundedRectangle(cornerRadius: 10)
                .stroke(.secondary.opacity(0.25), lineWidth: 1)
        }
    }
}

struct ExternalLinksView: View {
    @Environment(\.openURL) private var openURL
    @State private var pendingDestination: ExternalLinkDestination?

    let text: String

    private var destinations: [ExternalLinkDestination] {
        ExternalLinkDestination.detected(in: text)
    }

    var body: some View {
        Group {
            if !destinations.isEmpty {
                Divider()
                Text("External links")
                    .font(.caption.weight(.semibold))
                ForEach(destinations) { destination in
                    Button {
                        pendingDestination = destination
                    } label: {
                        HStack(alignment: .top, spacing: 8) {
                            Image(systemName: destination.warning == nil ?
                                "arrow.up.right.square" : "exclamationmark.triangle.fill")
                                .foregroundStyle(destination.warning == nil ? Color.accentColor : .orange)
                            VStack(alignment: .leading, spacing: 2) {
                                Text(destination.host)
                                    .fontWeight(.medium)
                                Text(destination.url.absoluteString)
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                                    .lineLimit(2)
                                    .textSelection(.enabled)
                                if let warning = destination.warning {
                                    Text(warning)
                                        .font(.caption)
                                        .foregroundStyle(.orange)
                                }
                            }
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .accessibilityHint("Shows a confirmation with the full destination before opening")
                }
            }
        }
        .alert(item: $pendingDestination) { destination in
            Alert(
                title: Text(destination.warning == nil ? "Open External Link?" : "Open Suspicious Link?"),
                message: Text(confirmationMessage(for: destination)),
                primaryButton: .cancel(),
                secondaryButton: .default(Text("Open")) {
                    _ = openURL(destination.url)
                }
            )
        }
    }

    private func confirmationMessage(for destination: ExternalLinkDestination) -> String {
        [destination.warning, destination.url.absoluteString]
            .compactMap(\.self)
            .joined(separator: "\n\n")
    }
}

struct ErrorOverlay: View {
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

struct UndoBanner: View {
    let actionLabel: String
    let undo: () -> Void

    var body: some View {
        HStack(spacing: 16) {
            Text("\(actionLabel).")
                .lineLimit(1)
            Button("Undo", action: undo)
                .keyboardShortcut("z", modifiers: [.command])
        }
        .padding(.horizontal, 18)
        .padding(.vertical, 12)
        .background(.regularMaterial, in: Capsule())
        .shadow(radius: 8, y: 3)
        .accessibilityElement(children: .combine)
    }
}
