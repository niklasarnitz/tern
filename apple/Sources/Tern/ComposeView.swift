import SwiftUI
import UniformTypeIdentifiers

struct ComposeView: View {
    @Environment(\.dismiss) private var dismiss
    @State private var draft: ComposeDraft
    @State private var isAddingAttachment = false

    init(draft: ComposeDraft) {
        _draft = State(initialValue: draft)
    }

    var body: some View {
        VStack(spacing: 0) {
            VStack(spacing: 8) {
                recipientField("To", recipients: $draft.recipients)
                recipientField("Cc", recipients: $draft.carbonCopyRecipients)
                recipientField("Bcc", recipients: $draft.blindCarbonCopyRecipients)
                Divider()
                TextField("Subject", text: $draft.subject)
                    .textFieldStyle(.plain)
            }
            .padding()

            Divider()

            TextEditor(text: $draft.body)
                .font(.body)
                .padding(12)
                .frame(maxWidth: .infinity, maxHeight: .infinity)

            if !draft.attachments.isEmpty {
                Divider()
                ScrollView(.horizontal) {
                    HStack {
                        ForEach(draft.attachments, id: \.self) { attachment in
                            attachmentLabel(attachment)
                        }
                    }
                    .padding(.horizontal)
                    .padding(.vertical, 8)
                }
            }

            Divider()
            HStack {
                Button {
                    isAddingAttachment = true
                } label: {
                    Label("Add Attachment", systemImage: "paperclip")
                }

                Spacer()

                Text("Sending is not available yet.")
                    .font(.caption)
                    .foregroundStyle(.secondary)

                Button("Close") {
                    dismiss()
                }
                .keyboardShortcut(.cancelAction)

                Button("Send") {}
                    .keyboardShortcut(.defaultAction)
                    .disabled(true)
            }
            .padding()
        }
        .frame(minWidth: 620, minHeight: 520)
        .navigationTitle("New Message")
        .fileImporter(
            isPresented: $isAddingAttachment,
            allowedContentTypes: [.data],
            allowsMultipleSelection: true
        ) { result in
            guard case let .success(urls) = result else { return }
            for url in urls where !draft.attachments.contains(url) {
                draft.attachments.append(url)
            }
        }
    }

    private func recipientField(_ title: String, recipients: Binding<[String]>) -> some View {
        LabeledContent(title) {
            TextField(title, text: Binding(
                get: { recipients.wrappedValue.joined(separator: ", ") },
                set: { recipients.wrappedValue = MailtoURLParser.recipients(from: $0) }
            ))
            .labelsHidden()
            .textFieldStyle(.plain)
        }
    }

    private func attachmentLabel(_ attachment: URL) -> some View {
        HStack(spacing: 6) {
            Image(systemName: "doc")
            Text(attachment.lastPathComponent)
                .lineLimit(1)
            Button {
                draft.attachments.removeAll { $0 == attachment }
            } label: {
                Image(systemName: "xmark.circle.fill")
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Remove \(attachment.lastPathComponent)")
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 6)
        .background(.quaternary, in: Capsule())
        .help(attachment.path)
    }
}
