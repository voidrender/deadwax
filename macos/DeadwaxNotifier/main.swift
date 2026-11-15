import AppKit
import UniformTypeIdentifiers
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

func warn(_ message: String) {
    FileHandle.standardError.write(Data("DeadwaxNotifier: \(message)\n".utf8))
}

/// Cached covers use a `.img` extension, so detect the real type from magic bytes.
func imageType(of data: Data) -> (ext: String, uti: String)? {
    let b = [UInt8](data.prefix(12))
    if b.starts(with: [0xFF, 0xD8, 0xFF]) { return ("jpg", UTType.jpeg.identifier) }
    if b.starts(with: [0x89, 0x50, 0x4E, 0x47]) { return ("png", UTType.png.identifier) }
    if b.starts(with: [0x47, 0x49, 0x46, 0x38]) { return ("gif", UTType.gif.identifier) }
    if b.count >= 12, b.starts(with: [0x52, 0x49, 0x46, 0x46]), Array(b[8..<12]) == [0x57, 0x45, 0x42, 0x50] {
        return ("webp", UTType.webP.identifier)
    }
    return nil
}

/// UN moves attachment files, so hand it a private copy.
func attachment(for path: String?) -> UNNotificationAttachment? {
    guard let path = path, !path.isEmpty else { return nil }
    let src = URL(fileURLWithPath: path)
    var copy: URL?
    do {
        let data = try Data(contentsOf: src)
        guard let type = imageType(of: data) else {
            warn("unrecognized image format: \(path)")
            return nil
        }
        let dest = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("\(UUID().uuidString).\(type.ext)")
        copy = dest
        try data.write(to: dest)
        return try UNNotificationAttachment(
            identifier: "cover", url: dest,
            options: [UNNotificationAttachmentOptionsTypeHintKey: type.uti])
    } catch {
        warn("attachment failed: \(error)")
        if let copy = copy { try? FileManager.default.removeItem(at: copy) }
        return nil
    }
}

let args = parseArgs()
let center = UNUserNotificationCenter.current()
var exitCode: Int32 = 1
let done = DispatchSemaphore(value: 0)

// The first run shows the permission prompt, and the notification can only
// be added once the user answers it. Give that run long enough to answer.
// Deadwax waits for this process on a background thread, so playback
// doesn't wait with it.
var timeout: Double = 5
let checked = DispatchSemaphore(value: 0)
center.getNotificationSettings { settings in
    if settings.authorizationStatus == .notDetermined {
        timeout = 120
    }
    checked.signal()
}
if checked.wait(timeout: .now() + 5) == .timedOut {
    exit(1)
}

center.requestAuthorization(options: [.alert]) { granted, _ in
    guard granted else {
        exitCode = 2
        done.signal()
        return
    }
    let content = UNMutableNotificationContent()
    content.title = args["title"] ?? "Deadwax"
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

if done.wait(timeout: .now() + timeout) == .timedOut {
    exitCode = 1
}
exit(exitCode)
