#!/usr/bin/env swift

import AppKit
import CoreGraphics
import Foundation

struct Point {
    let x: CGFloat
    let y: CGFloat
}

struct IconGeometry {
    let center = Point(x: 512, y: 500)
    let visorRadius = Point(x: 260, y: 220)
    let visorExponent: CGFloat = 2.8
    let gutter: CGFloat = 18
    let paneRadius: CGFloat = 30
    let rimThickness: CGFloat = 18
}

struct Random {
    private var state: UInt64 = 0x5350_4143_454d_414e

    mutating func unit() -> CGFloat {
        state = state &* 6_364_136_223_846_793_005 &+ 1_442_695_040_888_963_407
        return CGFloat(state >> 11) / CGFloat(UInt64.max >> 11)
    }
}

func superellipse(center: Point, radius: Point, exponent: CGFloat, samples: Int = 256) -> [Point] {
    (0..<samples).map { index in
        let angle = CGFloat(index) * 2 * .pi / CGFloat(samples)
        let cosine = cos(angle)
        let sine = sin(angle)
        return Point(
            x: center.x + radius.x * copysign(pow(abs(cosine), 2 / exponent), cosine),
            y: center.y + radius.y * copysign(pow(abs(sine), 2 / exponent), sine)
        )
    }
}

func roundedRectangle(x: CGFloat, y: CGFloat, width: CGFloat, height: CGFloat, radius: CGFloat) -> [Point] {
    let corners = [
        (Point(x: x + width - radius, y: y + radius), -CGFloat.pi / 2),
        (Point(x: x + width - radius, y: y + height - radius), 0),
        (Point(x: x + radius, y: y + height - radius), CGFloat.pi / 2),
        (Point(x: x + radius, y: y + radius), .pi),
    ]
    return corners.flatMap { center, start in
        (0...10).map { step in
            let angle = start + CGFloat(step) * .pi / 20
            return Point(x: center.x + cos(angle) * radius, y: center.y + sin(angle) * radius)
        }
    }
}

func intersect(_ subject: [Point], with clip: [Point]) -> [Point] {
    func cross(_ a: Point, _ b: Point, _ point: Point) -> CGFloat {
        (b.x - a.x) * (point.y - a.y) - (b.y - a.y) * (point.x - a.x)
    }
    func intersection(_ start: Point, _ end: Point, _ a: Point, _ b: Point) -> Point {
        let subjectX = end.x - start.x
        let subjectY = end.y - start.y
        let clipX = b.x - a.x
        let clipY = b.y - a.y
        let denominator = subjectX * clipY - subjectY * clipX
        let ratio = ((a.x - start.x) * clipY - (a.y - start.y) * clipX) / denominator
        return Point(x: start.x + ratio * subjectX, y: start.y + ratio * subjectY)
    }

    var output = subject
    for index in clip.indices {
        let a = clip[index]
        let b = clip[(index + 1) % clip.count]
        let input = output
        output = []
        guard var previous = input.last else { break }
        for current in input {
            let currentInside = cross(a, b, current) >= -0.001
            let previousInside = cross(a, b, previous) >= -0.001
            if currentInside != previousInside {
                output.append(intersection(previous, current, a, b))
            }
            if currentInside {
                output.append(current)
            }
            previous = current
        }
    }
    return output
}

func path(_ points: [Point]) -> String {
    guard let first = points.first else { return "" }
    let remainder = points.dropFirst().map { String(format: "L%.1f %.1f", $0.x, $0.y) }.joined()
    return String(format: "M%.1f %.1f", first.x, first.y) + remainder + "Z"
}

func writeVectorAssets(to directory: URL) throws {
    let geometry = IconGeometry()
    let visor = superellipse(
        center: geometry.center,
        radius: geometry.visorRadius,
        exponent: geometry.visorExponent
    )
    let insetVisor = superellipse(
        center: geometry.center,
        radius: Point(
            x: geometry.visorRadius.x - geometry.rimThickness,
            y: geometry.visorRadius.y - geometry.rimThickness
        ),
        exponent: geometry.visorExponent
    ).reversed()
    let paneVisor = Array(insetVisor.reversed())
    let halfGutter = geometry.gutter / 2
    let clips = [
        roundedRectangle(x: 64, y: 64, width: geometry.center.x - halfGutter - 64, height: geometry.center.y - halfGutter - 64, radius: geometry.paneRadius),
        roundedRectangle(x: geometry.center.x + halfGutter, y: 64, width: 960 - geometry.center.x - halfGutter, height: geometry.center.y - halfGutter - 64, radius: geometry.paneRadius),
        roundedRectangle(x: 64, y: geometry.center.y + halfGutter, width: geometry.center.x - halfGutter - 64, height: 896 - geometry.center.y - halfGutter, radius: geometry.paneRadius),
        roundedRectangle(x: geometry.center.x + halfGutter, y: geometry.center.y + halfGutter, width: 960 - geometry.center.x - halfGutter, height: 896 - geometry.center.y - halfGutter, radius: geometry.paneRadius),
    ]
    let panes = clips.map { intersect(paneVisor, with: $0) }
    let panePaths = panes.map { "  <path d=\"\(path($0))\"/>" }.joined(separator: "\n")
    let paneSVG = """
    <svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">
      <g fill="#315b9d">
    \(panePaths)
      </g>
    </svg>
    """
    let visorSVG = """
    <svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">
      <path fill="#c9dcfb" fill-rule="evenodd" d="\(path(visor))\(path(Array(insetVisor)))"/>
    </svg>
    """
    let helmetSVG = """
    <svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">
      <path fill="#c4d5f3" fill-rule="evenodd" d="M512 132c-198 0-342 139-342 334v239c0 41 20 77 51 100l-13 52c-7 27 14 53 42 53h524c28 0 49-26 42-53l-13-52c31-23 51-59 51-100V466c0-195-144-334-342-334zm0 68c159 0 274 111 274 266v218c0 50-40 90-90 90H328c-50 0-90-40-90-90V466c0-155 115-266 274-266z"/>
      <path fill="#c4d5f3" d="M227 752q285 50 570 0l-15 70q-270 43-540 0z"/>
      <path fill="#d9e7ff" d="M242 831q270 43 540 0l9 35q-279 48-558 0z"/>
      <path fill="#c4d5f3" d="M152 482h44v170h-44c-25 0-46-21-46-46v-78c0-25 21-46 46-46zM828 482h44c25 0 46 21 46 46v78c0 25-21 46-46 46h-44z"/>
    </svg>
    """
    try paneSVG.write(to: directory.appendingPathComponent("Panes.svg"), atomically: true, encoding: .utf8)
    try visorSVG.write(to: directory.appendingPathComponent("Visor.svg"), atomically: true, encoding: .utf8)
    try helmetSVG.write(to: directory.appendingPathComponent("Helmet.svg"), atomically: true, encoding: .utf8)
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
for index in 0..<48 {
    let progress = CGFloat(index) / 47
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

for _ in 0..<26 {
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

guard let image = context.makeImage() else {
    fatalError("Could not render icon")
}
let output = URL(fileURLWithPath: CommandLine.arguments.dropFirst().first ?? "app/AppIcon.icon/Assets/Space.png")
let png = NSBitmapImageRep(cgImage: image).representation(using: .png, properties: [:])!
try png.write(to: output)
try writeVectorAssets(to: output.deletingLastPathComponent())
