import Combine
import Foundation

enum SidebarItem: Hashable {
    case account(String)
    case mailbox(String)
}

struct UndoNotice: Equatable {
    let operationID: String
    let messageID: String
    let sourceMailboxID: String
    let actionLabel: String
}

/// Keeps all calls into the Rust application API off the main actor.
actor MailCoreActor {
    let client: MailClient

    init(databasePath: String) throws {
        client = try MailClient(databasePath: databasePath)
    }

    func listAccounts() throws -> [Account] {
        try client.listAccounts()
    }

    func listMailboxes(accountID: String) throws -> [Mailbox] {
        try client.listMailboxes(accountId: accountID)
    }

    func listMessages(mailboxID: String, offset: UInt32, limit: UInt32) throws -> [MessageSummary] {
        try client.listMessages(mailboxId: mailboxID, offset: offset, limit: limit)
    }

    func searchMessages(query: String, offset: UInt32, limit: UInt32) throws -> [MessageSummary] {
        try client.searchMessages(query: query, offset: offset, limit: limit)
    }

    func widgetSnapshot(mailboxIDs: [String], importantLimit: UInt32) throws -> WidgetSnapshot {
        try client.widgetSnapshot(mailboxIds: mailboxIDs, importantLimit: importantLimit)
    }

    func queueMessageMove(
        mailboxID: String,
        messageID: String,
        destinationMailboxID: String,
        undoDeadlineMilliseconds: Int64
    ) throws -> String {
        try client.queueMessageMove(
            mailboxId: mailboxID,
            messageId: messageID,
            destinationMailboxId: destinationMailboxID,
            undoDeadlineMs: undoDeadlineMilliseconds
        )
    }

    func undoOperation(operationID: String) throws -> Bool {
        try client.undoOperation(operationId: operationID)
    }
}

@MainActor
final class MailStore: ObservableObject {
    @Published var accounts: [Account] = []
    @Published var mailboxes: [Mailbox] = []
    @Published var messages: [MessageSummary] = []
    @Published var isLoading = false
    @Published var isLoadingAccounts = false
    @Published var isLoadingMailboxes = false
    @Published var isLoadingMessages = false
    @Published var isPerformingAction = false
    @Published var errorMessage: String?
    @Published var widgetErrorMessage: String?
    @Published var widgetPrivacy = WidgetDataStore.privacy()
    @Published var widgetMailboxIDs = WidgetDataStore.selectedMailboxIDs()
    @Published var pendingUndo: UndoNotice?
    @Published var selectedAccountID: String?
    @Published var selectedMailboxID: String?
    @Published var selectedMessageID: String?
    @Published var selectedSidebarItem: SidebarItem?
    @Published var searchText = ""
    @Published var activeSearchQuery: String?

    var core: MailCoreActor?
    var started = false
    var requestGeneration: UInt64 = 0
    var undoDismissTask: Task<Void, Never>?

    let messagePageSize: UInt32 = 100
    let undoInterval: TimeInterval = 8
    @Published var messageOffset: UInt32 = 0
    @Published var hasNextMessagePage = false

    var canRefresh: Bool {
        core != nil && !isLoading
    }

    var selectedMailbox: Mailbox? {
        mailboxes.first { $0.id == selectedMailboxID }
    }

    var selectedMessage: MessageSummary? {
        messages.first { $0.id == selectedMessageID }
    }

    var isSearching: Bool {
        activeSearchQuery != nil
    }

    var moveDestinations: [Mailbox] {
        mailboxes.filter { $0.id != selectedMailboxID }
    }

    var archiveMailbox: Mailbox? {
        mailbox(matching: ["archive", "all mail"])
    }

    var trashMailbox: Mailbox? {
        mailbox(matching: ["trash", "deleted", "bin"])
    }

    var spamMailbox: Mailbox? {
        mailbox(matching: ["spam", "junk"])
    }

    func start() async {
        guard !started else { return }
        started = true
        isLoading = true
        defer { isLoading = false }
        errorMessage = nil

        let path = DatabaseLocation.path()
        let generation = requestGeneration
        do {
            let createdCore = try await Task.detached(priority: .userInitiated) {
                try MailCoreActor(databasePath: path)
            }.value
            guard generation == requestGeneration else { return }
            core = createdCore
            await loadAccounts()
        } catch {
            errorMessage = Self.message(for: error)
        }
    }

    func retry() async {
        errorMessage = nil
        if core == nil {
            started = false
            await start()
        } else {
            await refresh()
        }
    }

    func refresh() async {
        guard core != nil else { return }
        requestGeneration &+= 1
        let generation = requestGeneration
        isLoading = true
        defer {
            if generation == requestGeneration {
                isLoading = false
            }
        }
        errorMessage = nil
        await loadAccounts()
    }

    func selectAccount(_ account: Account, invalidateRequest: Bool = true) async {
        guard selectedAccountID != account.id else { return }
        if invalidateRequest {
            requestGeneration &+= 1
        }
        let generation = requestGeneration
        isLoading = true
        defer {
            if generation == requestGeneration {
                isLoading = false
            }
        }
        selectedAccountID = account.id
        selectedMailboxID = nil
        selectedMessageID = nil
        searchText = ""
        activeSearchQuery = nil
        messageOffset = 0
        hasNextMessagePage = false
        isLoadingAccounts = false
        isLoadingMailboxes = false
        isLoadingMessages = false
        messages = []
        setSidebarSelection(.account(account.id))
        await loadMailboxes(for: account)
    }

    func selectMailbox(_ mailbox: Mailbox, invalidateRequest: Bool = true) async {
        guard selectedMailboxID != mailbox.id else { return }
        if invalidateRequest {
            requestGeneration &+= 1
        }
        let generation = requestGeneration
        isLoading = true
        defer {
            if generation == requestGeneration {
                isLoading = false
            }
        }
        selectedMailboxID = mailbox.id
        selectedMessageID = nil
        searchText = ""
        activeSearchQuery = nil
        messageOffset = 0
        hasNextMessagePage = false
        isLoadingAccounts = false
        isLoadingMailboxes = false
        isLoadingMessages = false
        messages = []
        setSidebarSelection(.mailbox(mailbox.id))
        guard let core else { return }
        await loadMessages(mailboxID: mailbox.id, offset: 0, using: core)
    }

    func handleSidebarSelection(_ item: SidebarItem?) async {
        guard let item else { return }
        switch item {
        case let .account(accountID):
            if let account = accounts.first(where: { $0.id == accountID }) {
                await selectAccount(account)
            }
        case let .mailbox(mailboxID):
            if let mailbox = mailboxes.first(where: { $0.id == mailboxID }) {
                await selectMailbox(mailbox)
            }
        }
    }

    func nextMessagePage() async {
        guard !isLoadingMessages, hasNextMessagePage, let core,
              messageOffset <= UInt32.max - messagePageSize else { return }
        let offset = messageOffset + messagePageSize
        if let activeSearchQuery {
            await loadSearch(query: activeSearchQuery, offset: offset, using: core)
        } else if let mailboxID = selectedMailboxID {
            await loadMessages(mailboxID: mailboxID, offset: offset, using: core)
        }
    }

    func previousMessagePage() async {
        guard !isLoadingMessages, messageOffset > 0, let core else { return }
        let offset = messageOffset - messagePageSize
        if let activeSearchQuery {
            await loadSearch(query: activeSearchQuery, offset: offset, using: core)
        } else if let mailboxID = selectedMailboxID {
            await loadMessages(mailboxID: mailboxID, offset: offset, using: core)
        }
    }

    func submitSearch() async {
        let query = searchText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !query.isEmpty, let core else {
            await clearSearch()
            return
        }
        requestGeneration &+= 1
        activeSearchQuery = query
        selectedMessageID = nil
        messageOffset = 0
        hasNextMessagePage = false
        messages = []
        errorMessage = nil
        await loadSearch(query: query, offset: 0, using: core)
    }

    func clearSearch() async {
        guard activeSearchQuery != nil else { return }
        requestGeneration &+= 1
        activeSearchQuery = nil
        selectedMessageID = nil
        messageOffset = 0
        hasNextMessagePage = false
        messages = []
        errorMessage = nil
        guard let mailboxID = selectedMailboxID, let core else { return }
        await loadMessages(mailboxID: mailboxID, offset: 0, using: core)
    }

    func setWidgetPrivacy(_ privacy: WidgetPrivacy) async {
        widgetPrivacy = privacy
        WidgetDataStore.setPrivacy(privacy)
        await publishWidgetSnapshot()
    }

    func setMailbox(_ mailboxID: String, includedInWidgets: Bool) async {
        if includedInWidgets {
            widgetMailboxIDs.insert(mailboxID)
        } else {
            widgetMailboxIDs.remove(mailboxID)
        }
        WidgetDataStore.setSelectedMailboxIDs(widgetMailboxIDs)
        await publishWidgetSnapshot()
    }

    func isMailboxIncludedInWidgets(_ mailboxID: String) -> Bool {
        widgetMailboxIDs.contains(mailboxID)
    }
}

private enum DatabaseLocation {
    static func path() -> String {
        if let configured = ProcessInfo.processInfo.environment["TERN_DATABASE"] {
            let trimmed = configured.trimmingCharacters(in: .whitespacesAndNewlines)
            if !trimmed.isEmpty {
                return configured
            }
        }

        let applicationSupport = FileManager.default.urls(
            for: .applicationSupportDirectory,
            in: .userDomainMask
        ).first ?? URL(fileURLWithPath: NSTemporaryDirectory(), isDirectory: true)
        let directory = applicationSupport.appendingPathComponent("Tern", isDirectory: true)
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        return directory.appendingPathComponent("mail.sqlite").path
    }
}
