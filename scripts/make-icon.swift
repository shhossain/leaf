// Draws Leaf's app icon: a page with text lines and a leaf, on a green tile.
// Usage: swift scripts/make-icon.swift assets/icon-1024.png
import AppKit

let size: CGFloat = 1024
let out = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "icon-1024.png"
let image = NSImage(size: NSSize(width: size, height: size))
image.lockFocus()
let ctx = NSGraphicsContext.current!.cgContext

// macOS icon grid: ~824px tile centred in 1024 with a soft shadow.
let tile = CGRect(x: 100, y: 100, width: 824, height: 824)
ctx.saveGState()
ctx.setShadow(offset: CGSize(width: 0, height: -12), blur: 28, color: NSColor.black.withAlphaComponent(0.35).cgColor)
let tilePath = NSBezierPath(roundedRect: tile, xRadius: 185, yRadius: 185)
NSColor(calibratedRed: 0.10, green: 0.42, blue: 0.28, alpha: 1).setFill()
tilePath.fill()
ctx.restoreGState()
NSGradient(starting: NSColor(calibratedRed: 0.22, green: 0.66, blue: 0.43, alpha: 1),
           ending: NSColor(calibratedRed: 0.07, green: 0.36, blue: 0.24, alpha: 1))!
    .draw(in: tilePath, angle: -90)

// The page.
let page = NSBezierPath(roundedRect: CGRect(x: 262, y: 210, width: 500, height: 604), xRadius: 44, yRadius: 44)
NSColor(calibratedWhite: 0.98, alpha: 1).setFill()
page.fill()

// Text lines.
NSColor(calibratedRed: 0.62, green: 0.70, blue: 0.66, alpha: 1).setFill()
for (i, width) in [360.0, 300, 380, 250, 330].enumerated() {
    NSBezierPath(roundedRect: CGRect(x: 322, y: 712 - Double(i) * 62, width: width, height: 26), xRadius: 13, yRadius: 13).fill()
}

// The leaf, overlapping the page's lower right corner.
ctx.saveGState()
ctx.translateBy(x: 640, y: 330)
ctx.rotate(by: .pi / 5)
let leaf = NSBezierPath()
leaf.move(to: CGPoint(x: 0, y: -170))
leaf.curve(to: CGPoint(x: 0, y: 170), controlPoint1: CGPoint(x: 150, y: -110), controlPoint2: CGPoint(x: 150, y: 110))
leaf.curve(to: CGPoint(x: 0, y: -170), controlPoint1: CGPoint(x: -150, y: 110), controlPoint2: CGPoint(x: -150, y: -110))
NSGradient(starting: NSColor(calibratedRed: 0.55, green: 0.85, blue: 0.35, alpha: 1),
           ending: NSColor(calibratedRed: 0.20, green: 0.62, blue: 0.25, alpha: 1))!.draw(in: leaf, angle: 90)
let vein = NSBezierPath()
vein.move(to: CGPoint(x: 0, y: -210))
vein.line(to: CGPoint(x: 0, y: 140))
vein.lineWidth = 14
vein.lineCapStyle = .round
NSColor(calibratedRed: 0.10, green: 0.40, blue: 0.18, alpha: 0.8).setStroke()
vein.stroke()
ctx.restoreGState()

image.unlockFocus()
let rep = NSBitmapImageRep(data: image.tiffRepresentation!)!
try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: out))
