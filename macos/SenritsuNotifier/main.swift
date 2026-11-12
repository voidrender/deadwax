import AppKit
import UserNotifications

// Exit codes: 0 posted, 1 other error, 2 not authorized.

func parseArgs() -> [String: String] {
    var out: [String: String] = [:]
    var it = CommandLine.arguments.dropFirst().makeIterator()
    while let key = it.next() {
        guard key.hasPrefix("--"), let value = it.next() else { continue }
        out[String(key.dropFirst(2))] = value
    }
    return out
}

/// UN moves attachment files, so hand it a private copy.
func attachment(for path: String?) -> UNNotificationAttachment? {
    guard let path = path, !path.isEmpty else { return nil }
    let src = URL(fileURLWithPath: path)
    let ext = src.pathExtension.isEmpty ? "jpg" : src.pathExtension
    let copy = URL(fileURLWithPath: NSTemporaryDirectory())
        .appendingPathComponent("\(UUID().uuidString).\(ext)")
    do {
        try FileManager.default.copyItem(at: src, to: copy)
        return try UNNotificationAttachment(identifier: "cover", url: copy, options: nil)
    } catch {
        return nil
    }
}

let args = parseArgs()
let center = UNUserNotificationCenter.current()
var exitCode: Int32 = 1
let done = DispatchSemaphore(value: 0)

center.requestAuthorization(options: [.alert]) { granted, _ in
    guard granted else {
        exitCode = 2
        done.signal()
        return
    }
    let content = UNMutableNotificationContent()
    content.title = args["title"] ?? "Senritsu"
    content.subtitle = args["subtitle"] ?? ""
    content.body = args["body"] ?? ""
    if let att = attachment(for: args["image"]) {
        content.attachments = [att]
    }
    let request = UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil)
    center.add(request) { error in
        exitCode = error == nil ? 0 : 1
        done.signal()
    }
}

if done.wait(timeout: .now() + 5) == .timedOut {
    exitCode = 1
}
exit(exitCode)
