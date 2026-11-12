import AppKit

// Usage: swift make-icon.swift <output.png>
// Draws a 1024x1024 dark rounded square with the glyph in the accent color.

let size = 1024
let outPath = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "icon.png"
let accent = NSColor(srgbRed: 0.77, green: 0.65, blue: 0.91, alpha: 1.0)
let background = NSColor(srgbRed: 0.09, green: 0.09, blue: 0.12, alpha: 1.0)

guard let rep = NSBitmapImageRep(
    bitmapDataPlanes: nil, pixelsWide: size, pixelsHigh: size, bitsPerSample: 8,
    samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
    bytesPerRow: 0, bitsPerPixel: 0)
else { exit(1) }

NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)

let rect = NSRect(x: 0, y: 0, width: size, height: size)
background.setFill()
NSBezierPath(roundedRect: rect, xRadius: 230, yRadius: 230).fill()

let glyph = "旋" as NSString
let attrs: [NSAttributedString.Key: Any] = [
    .font: NSFont.systemFont(ofSize: 640, weight: .medium),
    .foregroundColor: accent,
]
let textSize = glyph.size(withAttributes: attrs)
glyph.draw(
    at: NSPoint(x: (CGFloat(size) - textSize.width) / 2, y: (CGFloat(size) - textSize.height) / 2),
    withAttributes: attrs)

NSGraphicsContext.restoreGraphicsState()

guard let png = rep.representation(using: .png, properties: [:]) else { exit(1) }
do { try png.write(to: URL(fileURLWithPath: outPath)) } catch { exit(1) }
