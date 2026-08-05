#!/usr/bin/env swift

import AppKit
import CoreGraphics
import Foundation

struct Random {
    private var state: UInt64 = 0x5350_4143_454d_414e

    mutating func unit() -> CGFloat {
        state = state &* 6_364_136_223_846_793_005 &+ 1_442_695_040_888_963_407
        return CGFloat(state >> 11) / CGFloat(UInt64.max >> 11)
    }
}

private let size = 1024
private let colorSpace = CGColorSpace(name: CGColorSpace.displayP3)!
private let bitmapInfo = CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue)
guard let context = CGContext(
    data: nil,
    width: size,
    height: size,
    bitsPerComponent: 8,
    bytesPerRow: size * 4,
    space: colorSpace,
    bitmapInfo: bitmapInfo.rawValue
) else {
    fatalError("Could not create icon context")
}

func color(_ red: CGFloat, _ green: CGFloat, _ blue: CGFloat, _ alpha: CGFloat = 1) -> CGColor {
    CGColor(colorSpace: colorSpace, components: [red, green, blue, alpha])!
}

func radialGlow(center: CGPoint, radius: CGFloat, inner: CGColor, outer: CGColor) {
    let gradient = CGGradient(
        colorsSpace: colorSpace,
        colors: [inner, outer] as CFArray,
        locations: [0, 1]
    )!
    context.drawRadialGradient(
        gradient,
        startCenter: center,
        startRadius: 0,
        endCenter: center,
        endRadius: radius,
        options: [.drawsAfterEndLocation]
    )
}

context.setFillColor(color(0.003, 0.008, 0.035))
context.fill(CGRect(x: 0, y: 0, width: size, height: size))

context.setBlendMode(.screen)
radialGlow(
    center: CGPoint(x: 760, y: 260),
    radius: 590,
    inner: color(0.01, 0.20, 1.0, 0.9),
    outer: color(0.0, 0.01, 0.08, 0)
)
radialGlow(
    center: CGPoint(x: 620, y: 760),
    radius: 520,
    inner: color(0.02, 0.08, 0.42, 0.72),
    outer: color(0.0, 0.0, 0.05, 0)
)

var random = Random()
for index in 0..<90 {
    let progress = CGFloat(index) / 89
    let angle = progress * 2.1 + 0.15
    let distance = 90 + progress * 510
    let center = CGPoint(
        x: 735 - cos(angle) * distance + (random.unit() - 0.5) * 75,
        y: 240 + sin(angle) * distance * 0.56 + (random.unit() - 0.5) * 55
    )
    let radius = 34 + random.unit() * 110
    radialGlow(
        center: center,
        radius: radius,
        inner: color(0.02, 0.12 + random.unit() * 0.16, 0.72, 0.035 + random.unit() * 0.05),
        outer: color(0.0, 0.01, 0.08, 0)
    )
}

for _ in 0..<185 {
    let x = 105 + random.unit() * 814
    let y = 100 + random.unit() * 824
    let brightness = 0.45 + random.unit() * 0.55
    let radius = 0.7 + pow(random.unit(), 5) * 3.6
    context.setFillColor(color(0.55 + brightness * 0.35, 0.72 + brightness * 0.25, 1, brightness))
    context.fillEllipse(in: CGRect(x: x - radius, y: y - radius, width: radius * 2, height: radius * 2))

    if radius > 2.2 {
        context.setStrokeColor(color(0.55, 0.82, 1, brightness * 0.55))
        context.setLineWidth(1)
        context.move(to: CGPoint(x: x - radius * 3.5, y: y))
        context.addLine(to: CGPoint(x: x + radius * 3.5, y: y))
        context.move(to: CGPoint(x: x, y: y - radius * 3.5))
        context.addLine(to: CGPoint(x: x, y: y + radius * 3.5))
        context.strokePath()
    }
}

context.setBlendMode(.screen)
let sheen = CGGradient(
    colorsSpace: colorSpace,
    colors: [color(0.65, 0.79, 1, 0.23), color(0.14, 0.35, 0.8, 0)] as CFArray,
    locations: [0, 1]
)!
context.saveGState()
context.addPath(CGPath(roundedRect: CGRect(x: 85, y: 545, width: 850, height: 395), cornerWidth: 240, cornerHeight: 240, transform: nil))
context.clip()
context.drawLinearGradient(
    sheen,
    start: CGPoint(x: 130, y: 920),
    end: CGPoint(x: 650, y: 550),
    options: []
)
context.restoreGState()

guard let image = context.makeImage() else {
    fatalError("Could not render icon")
}
let output = URL(fileURLWithPath: CommandLine.arguments.dropFirst().first ?? "app/AppIcon.icon/Assets/Space.png")
let png = NSBitmapImageRep(cgImage: image).representation(using: .png, properties: [:])!
try png.write(to: output)
