#!/usr/bin/env swift

import CoreGraphics
import Foundation

struct Point {
    let x: CGFloat
    let y: CGFloat
}

struct IconGeometry {
    let center = Point(x: 512, y: 512)
    let visorRadius = Point(x: 330, y: 250)
    let visorExponent: CGFloat = 3.4
    let gutter: CGFloat = 22
    let paneRadius: CGFloat = 30
    let verticalSplit: CGFloat = 570
    let horizontalSplit: CGFloat = 500
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
    func crossing(_ start: Point, _ end: Point, _ a: Point, _ b: Point) -> Point {
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
                output.append(crossing(previous, current, a, b))
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

let geometry = IconGeometry()
let visor = superellipse(
    center: geometry.center,
    radius: geometry.visorRadius,
    exponent: geometry.visorExponent
)
let halfGutter = geometry.gutter / 2
let clips = [
    roundedRectangle(
        x: 64,
        y: 64,
        width: geometry.verticalSplit - halfGutter - 64,
        height: 896,
        radius: geometry.paneRadius
    ),
    roundedRectangle(
        x: geometry.verticalSplit + halfGutter,
        y: 64,
        width: 960 - geometry.verticalSplit - halfGutter,
        height: geometry.horizontalSplit - halfGutter - 64,
        radius: geometry.paneRadius
    ),
    roundedRectangle(
        x: geometry.verticalSplit + halfGutter,
        y: geometry.horizontalSplit + halfGutter,
        width: 960 - geometry.verticalSplit - halfGutter,
        height: 896 - geometry.horizontalSplit - halfGutter,
        radius: geometry.paneRadius
    ),
]
let panes = clips.map { intersect(visor, with: $0) }
let panePaths = panes.map { "    <path d=\"\(path($0))\"/>" }.joined(separator: "\n")
let paneSVG = """
<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">
  <g fill="#e4ecff">
\(panePaths)
  </g>
</svg>
"""
let output = URL(fileURLWithPath: CommandLine.arguments.dropFirst().first ?? "app/AppIcon.icon/Assets")
try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
try paneSVG.write(to: output.appendingPathComponent("Panes.svg"), atomically: true, encoding: .utf8)
