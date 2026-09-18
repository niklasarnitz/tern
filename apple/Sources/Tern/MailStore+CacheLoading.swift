import Foundation

extension MailStore {
    func loadAccounts() async {
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

    func loadMailboxes(for account: Account) async {
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

    func loadMessages(mailboxID: String, offset: UInt32, using core: MailCoreActor) async {
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

    func loadSearch(query: String, offset: UInt32, using core: MailCoreActor) async {
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

    func setSidebarSelection(_ item: SidebarItem) {
        selectedSidebarItem = item
    }

    func publishWidgetSnapshot() async {
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

    func mailbox(matching names: [String]) -> Mailbox? {
        mailboxes.first { mailbox in
            let name = "\(mailbox.displayName) \(mailbox.remoteName)".lowercased()
            return mailbox.id != selectedMailboxID && names.contains { name.contains($0) }
        }
    }

    func initialMailbox() -> Mailbox? {
        mailboxes.first(where: { $0.remoteName.uppercased() == "INBOX" }) ?? mailboxes.first
    }

    func scheduleUndoDismissal(for notice: UndoNotice) {
        undoDismissTask?.cancel()
        undoDismissTask = Task { [weak self] in
            try? await Task.sleep(for: .seconds(undoInterval))
            guard !Task.isCancelled, self?.pendingUndo == notice else { return }
            self?.pendingUndo = nil
        }
    }

    static func message(for error: Error) -> String {
        let description = error.localizedDescription.trimmingCharacters(in: .whitespacesAndNewlines)
        return description.isEmpty ? "The local mail store could not be opened." : description
    }
}
