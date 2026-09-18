import Foundation
import SwiftUI
import WidgetKit

struct TernWidgetEntry: TimelineEntry {
    let date: Date
    let snapshot: CachedWidgetSnapshot
}

struct TernWidgetProvider: TimelineProvider {
    func placeholder(in _: Context) -> TernWidgetEntry {
        TernWidgetEntry(date: Date(), snapshot: .empty)
    }

    func getSnapshot(in _: Context, completion: @escaping (TernWidgetEntry) -> Void) {
        completion(TernWidgetEntry(date: Date(), snapshot: WidgetDataStore.snapshot()))
    }

    func getTimeline(in _: Context, completion: @escaping (Timeline<TernWidgetEntry>) -> Void) {
        let entry = TernWidgetEntry(date: Date(), snapshot: WidgetDataStore.snapshot())
        completion(Timeline(entries: [entry], policy: .never))
    }
}

private struct WidgetBackground: ViewModifier {
    func body(content: Content) -> some View {
        content.containerBackground(.background, for: .widget)
    }
}

private struct UnreadCountView: View {
    let entry: TernWidgetEntry

    var body: some View {
        VStack(alignment: .leading) {
            Label("Unread", systemImage: "envelope.badge")
                .font(.caption)
                .foregroundStyle(.secondary)
            Spacer()
            Text(entry.snapshot.unreadCount, format: .number)
                .font(.system(.largeTitle, design: .rounded, weight: .bold))
                .contentTransition(.numericText())
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)
        .modifier(WidgetBackground())
        .widgetURL(URL(string: "tern://mail"))
    }
}

private struct ImportantMailView: View {
    let entry: TernWidgetEntry

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Label("Important", systemImage: "star.fill")
                .font(.caption)
                .foregroundStyle(.secondary)
            if entry.snapshot.importantMessages.isEmpty {
                Text("No message details shared")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            } else {
                ForEach(entry.snapshot.importantMessages.prefix(3), id: \.id) { message in
                    VStack(alignment: .leading, spacing: 2) {
                        Text(message.sender.isEmpty ? "Unknown sender" : message.sender)
                            .font(.caption.weight(.semibold))
                            .lineLimit(1)
                        Text(message.subject.isEmpty ? "(No subject)" : message.subject)
                            .font(.caption)
                            .lineLimit(1)
                        if let snippet = message.snippet, !snippet.isEmpty {
                            Text(snippet)
                                .font(.caption2)
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                        }
                    }
                }
            }
            Spacer(minLength: 0)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)
        .modifier(WidgetBackground())
        .widgetURL(URL(string: "tern://important"))
    }
}

private struct SelectedMailboxesView: View {
    let entry: TernWidgetEntry

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Label("Mailboxes", systemImage: "tray.full")
                .font(.caption)
                .foregroundStyle(.secondary)
            if entry.snapshot.mailboxes.isEmpty {
                Text("Choose mailboxes in Tern Settings")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            } else {
                ForEach(entry.snapshot.mailboxes.prefix(5), id: \.id) { mailbox in
                    HStack {
                        Text(mailbox.displayName)
                            .lineLimit(1)
                        Spacer()
                        Text(mailbox.unreadCount, format: .number)
                            .monospacedDigit()
                            .foregroundStyle(.secondary)
                    }
                    .font(.callout)
                }
            }
            Spacer(minLength: 0)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)
        .modifier(WidgetBackground())
        .widgetURL(URL(string: "tern://mailboxes"))
    }
}

struct UnreadCountWidget: Widget {
    let kind = "TernUnreadCount"

    var body: some WidgetConfiguration {
        StaticConfiguration(kind: kind, provider: TernWidgetProvider()) { entry in
            UnreadCountView(entry: entry)
        }
        .configurationDisplayName("Unread Mail")
        .description("See the unread count for selected mailboxes.")
        .supportedFamilies([.systemSmall])
    }
}

struct ImportantMailWidget: Widget {
    let kind = "TernImportantMail"

    var body: some WidgetConfiguration {
        StaticConfiguration(kind: kind, provider: TernWidgetProvider()) { entry in
            ImportantMailView(entry: entry)
        }
        .configurationDisplayName("Important Mail")
        .description("See recent starred mail using your privacy setting.")
        .supportedFamilies([.systemMedium, .systemLarge])
    }
}

struct SelectedMailboxesWidget: Widget {
    let kind = "TernSelectedMailboxes"

    var body: some WidgetConfiguration {
        StaticConfiguration(kind: kind, provider: TernWidgetProvider()) { entry in
            SelectedMailboxesView(entry: entry)
        }
        .configurationDisplayName("Selected Mailboxes")
        .description("See unread counts for mailboxes selected in Tern.")
        .supportedFamilies([.systemMedium])
    }
}

@main
struct TernWidgetBundle: WidgetBundle {
    var body: some Widget {
        UnreadCountWidget()
        ImportantMailWidget()
        SelectedMailboxesWidget()
    }
}
