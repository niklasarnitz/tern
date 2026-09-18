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
private actor MailCoreActor {
    private let client: MailClient

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
    @Published private(set) var accounts: [Account] = []
    @Published private(set) var mailboxes: [Mailbox] = []
    @Published private(set) var messages: [MessageSummary] = []
    @Published private(set) var isLoading = false
    @Published private(set) var isLoadingAccounts = false
    @Published private(set) var isLoadingMailboxes = false
    @Published private(set) var isLoadingMessages = false
    @Published private(set) var isPerformingAction = false
    @Published private(set) var errorMessage: String?
    @Published private(set) var widgetErrorMessage: String?
    @Published private(set) var widgetPrivacy = WidgetDataStore.privacy()
    @Published private(set) var widgetMailboxIDs = WidgetDataStore.selectedMailboxIDs()
    @Published private(set) var pendingUndo: UndoNotice?
    @Published var selectedAccountID: String?
    @Published var selectedMailboxID: String?
    @Published var selectedMessageID: String?
    @Published var selectedSidebarItem: SidebarItem?
    @Published var searchText = ""
    @Published private(set) var activeSearchQuery: String?

    private var core: MailCoreActor?
    private var started = false
    private var requestGeneration: UInt64 = 0
    private var undoDismissTask: Task<Void, Never>?

    let messagePageSize: UInt32 = 100
    let undoInterval: TimeInterval = 8
    @Published private(set) var messageOffset: UInt32 = 0
    @Published private(set) var hasNextMessagePage = false

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

    func moveMessage(_ message: MessageSummary, to destination: Mailbox, actionLabel: String) async {
        guard !isPerformingAction, let core, let mailboxID = selectedMailboxID,
              message.mailboxId == mailboxID, destination.id != mailboxID else { return }
        isPerformingAction = true
        defer { isPerformingAction = false }
        errorMessage = nil

        let deadline = Date().addingTimeInterval(undoInterval).timeIntervalSince1970 * 1_000
        do {
            let operationID = try await core.queueMessageMove(
                mailboxID: mailboxID,
                messageID: message.id,
                destinationMailboxID: destination.id,
                undoDeadlineMilliseconds: Int64(deadline)
            )
            if selectedMessageID == message.id {
                selectedMessageID = nil
            }
            let notice = UndoNotice(
                operationID: operationID,
                messageID: message.id,
                sourceMailboxID: mailboxID,
                actionLabel: actionLabel
            )
            pendingUndo = notice
            scheduleUndoDismissal(for: notice)
            if selectedMailboxID == mailboxID {
                await loadMessages(mailboxID: mailboxID, offset: messageOffset, using: core)
            }
            await publishWidgetSnapshot()
        } catch {
            errorMessage = Self.message(for: error)
        }
    }

    func undoLastAction() async {
        guard let notice = pendingUndo, let core else { return }
        undoDismissTask?.cancel()
        do {
            guard try await core.undoOperation(operationID: notice.operationID) else {
                pendingUndo = nil
                errorMessage = "This action has already been sent to the mail server."
                return
            }
            pendingUndo = nil
            if selectedMailboxID == notice.sourceMailboxID {
                await loadMessages(mailboxID: notice.sourceMailboxID, offset: messageOffset, using: core)
                selectedMessageID = notice.messageID
            }
            await publishWidgetSnapshot()
        } catch {
            errorMessage = Self.message(for: error)
        }
    }

    private func loadAccounts() async {
        guard let core else { return }
        let generation = requestGeneration
        isLoadingAccounts = true
        do {
            let loadedAccounts = try await core.listAccounts()
            guard generation == requestGeneration else { return }
            accounts = loadedAccounts
            isLoadingAccounts = false
            guard let account = accounts.first(where: { $0.id == selectedAccountID }) ?? accounts.first else {
                selectedAccountID = nil
                selectedSidebarItem = nil
                mailboxes = []
                messages = []
                return
            }
            if selectedAccountID != account.id {
                await selectAccount(account, invalidateRequest: false)
            } else {
                await loadMailboxes(for: account)
            }
            await publishWidgetSnapshot()
        } catch {
            if generation == requestGeneration {
                isLoadingAccounts = false
                errorMessage = Self.message(for: error)
            }
        }
    }

    private func loadMailboxes(for account: Account) async {
        guard let core else { return }
        let generation = requestGeneration
        isLoadingMailboxes = true
        do {
            let loadedMailboxes = try await core.listMailboxes(accountID: account.id)
            guard generation == requestGeneration, selectedAccountID == account.id else { return }
            mailboxes = loadedMailboxes
            isLoadingMailboxes = false
            if let selectedMailboxID, let selected = mailboxes.first(where: { $0.id == selectedMailboxID }) {
                setSidebarSelection(.mailbox(selected.id))
                if let activeSearchQuery {
                    await loadSearch(query: activeSearchQuery, offset: messageOffset, using: core)
                } else {
                    await loadMessages(mailboxID: selected.id, offset: messageOffset, using: core)
                }
            } else if let initial = initialMailbox() {
                await selectMailbox(initial, invalidateRequest: false)
            } else {
                selectedMailboxID = nil
                selectedSidebarItem = .account(account.id)
                messages = []
            }
        } catch {
            if generation == requestGeneration {
                isLoadingMailboxes = false
                errorMessage = Self.message(for: error)
            }
        }
    }

    private func loadMessages(mailboxID: String, offset: UInt32, using core: MailCoreActor) async {
        let generation = requestGeneration
        isLoadingMessages = true
        defer {
            if generation == requestGeneration, selectedMailboxID == mailboxID {
                isLoadingMessages = false
            }
        }
        do {
            // Every request is deliberately bounded. Paging replaces the
            // current window rather than accumulating an entire mailbox.
            let loadedMessages = try await core.listMessages(
                mailboxID: mailboxID,
                offset: offset,
                limit: messagePageSize + 1
            )
            guard generation == requestGeneration, selectedMailboxID == mailboxID else { return }
            messages = Array(loadedMessages.prefix(Int(messagePageSize)))
            messageOffset = offset
            hasNextMessagePage = loadedMessages.count > Int(messagePageSize)
        } catch {
            if generation == requestGeneration {
                errorMessage = Self.message(for: error)
            }
        }
    }

    private func loadSearch(query: String, offset: UInt32, using core: MailCoreActor) async {
        let generation = requestGeneration
        isLoadingMessages = true
        defer {
            if generation == requestGeneration, activeSearchQuery == query {
                isLoadingMessages = false
            }
        }
        do {
            let loadedMessages = try await core.searchMessages(
                query: query,
                offset: offset,
                limit: messagePageSize + 1
            )
            guard generation == requestGeneration, activeSearchQuery == query else { return }
            messages = Array(loadedMessages.prefix(Int(messagePageSize)))
            messageOffset = offset
            hasNextMessagePage = loadedMessages.count > Int(messagePageSize)
        } catch {
            if generation == requestGeneration, activeSearchQuery == query {
                errorMessage = Self.message(for: error)
            }
        }
    }

    private func setSidebarSelection(_ item: SidebarItem) {
        selectedSidebarItem = item
    }

    private func publishWidgetSnapshot() async {
        guard let core else { return }
        do {
            let snapshot = try await core.widgetSnapshot(
                mailboxIDs: widgetMailboxIDs.sorted(),
                importantLimit: 5
            )
            try WidgetDataStore.cache(snapshot, privacy: widgetPrivacy)
            widgetErrorMessage = nil
        } catch {
            widgetErrorMessage = Self.message(for: error)
        }
    }

    private func mailbox(matching names: [String]) -> Mailbox? {
        mailboxes.first { mailbox in
            let name = "\(mailbox.displayName) \(mailbox.remoteName)".lowercased()
            return mailbox.id != selectedMailboxID && names.contains { name.contains($0) }
        }
    }

    private func initialMailbox() -> Mailbox? {
        mailboxes.first(where: { $0.remoteName.uppercased() == "INBOX" }) ?? mailboxes.first
    }

    private func scheduleUndoDismissal(for notice: UndoNotice) {
        undoDismissTask?.cancel()
        undoDismissTask = Task { [weak self] in
            try? await Task.sleep(for: .seconds(undoInterval))
            guard !Task.isCancelled, self?.pendingUndo == notice else { return }
            self?.pendingUndo = nil
        }
    }

    private static func message(for error: Error) -> String {
        let description = error.localizedDescription.trimmingCharacters(in: .whitespacesAndNewlines)
        return description.isEmpty ? "The local mail store could not be opened." : description
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
