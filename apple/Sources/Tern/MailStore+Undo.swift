import Foundation

extension MailStore {
    func moveMessage(_ message: MessageSummary, to destination: Mailbox, actionLabel: String) async {
        guard !isPerformingAction, let core, let mailboxID = selectedMailboxID,
              message.mailboxId == mailboxID, destination.id != mailboxID else { return }
        isPerformingAction = true
        defer { isPerformingAction = false }
        errorMessage = nil

        let deadline = Date().addingTimeInterval(undoInterval).timeIntervalSince1970 * 1000
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
}
