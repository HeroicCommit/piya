#!/usr/bin/env swift
import AppKit

let size = NSSize(width: 512, height: 512)
let image = NSImage(size: size)
image.lockFocus()

let outer = NSBezierPath(ovalIn: NSRect(x: 56, y: 56, width: 400, height: 400))
NSColor(calibratedRed: 0.925, green: 0.282, blue: 0.600, alpha: 1).setFill()
outer.fill()

let inner = NSBezierPath(ovalIn: NSRect(x: 156, y: 156, width: 200, height: 200))
NSColor(calibratedRed: 0.976, green: 0.659, blue: 0.831, alpha: 1).setFill()
inner.fill()

image.unlockFocus()

guard
    let tiff = image.tiffRepresentation,
    let rep = NSBitmapImageRep(data: tiff),
    let png = rep.representation(using: .png, properties: [:])
else {
    fputs("failed to encode icon\n", stderr)
    exit(1)
}

let out = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "assets/AppIcon.png"
try png.write(to: URL(fileURLWithPath: out))
print(out)
