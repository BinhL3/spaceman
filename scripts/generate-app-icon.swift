#!/usr/bin/env swift

import Foundation

let paneSVG = """
<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">
  <g fill="#e4ecff">
    <rect x="196" y="278" width="364" height="468" rx="72"/>
    <rect x="584" y="278" width="244" height="222" rx="58"/>
    <rect x="584" y="524" width="244" height="222" rx="58"/>
  </g>
</svg>
"""

let output = URL(fileURLWithPath: CommandLine.arguments.dropFirst().first ?? "app/AppIcon.icon/Assets")
try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
try paneSVG.write(to: output.appendingPathComponent("Panes.svg"), atomically: true, encoding: .utf8)
