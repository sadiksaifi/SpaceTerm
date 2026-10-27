// Keystroke and wheel-scroll to pixels probe for the bench:latency:*:macos tasks.
//
// It posts CGEvent keys or wheel events to the frontmost on-screen window of one app and times the
// first ScreenCaptureKit frame, at up to 240 fps, whose strip around the typed text changes. It
// finds the strip itself: it clears the focused Pane, forces a steady cursor, types a short
// calibration word and diffs the window. It prints one JSON object per line on stdout.
//
//   latency-pixels check
//   latency-pixels measure --app NAME --mode keystroke|scroll --samples N [--workload COMMAND]
//
// With --workload, SpaceTerm splits the focused Pane to the right, measures the left Pane idle,
// runs COMMAND in the right Pane, measures the left Pane again and closes the right Pane.
// Key codes assume a US keyboard layout.
import AppKit
import CoreGraphics
import CoreMedia
import Foundation
import ScreenCaptureKit

struct Failure: Error { let message: String }

func emit(_ object: [String: Any]) {
    let data = try! JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])
    FileHandle.standardOutput.write(data + Data("\n".utf8))
}

func progress(_ text: String) {
    FileHandle.standardError.write(Data("latency-pixels: \(text)\n".utf8))
}

func pause(_ seconds: Double) { usleep(useconds_t(seconds * 1_000_000)) }

/// Keystrokes echoed on one line before Ctrl-U clears it.
let keysPerLine = 12

var timebase = mach_timebase_info_data_t()
func nowNs() -> UInt64 { mach_absolute_time() * UInt64(timebase.numer) / UInt64(timebase.denom) }

// MARK: Permissions

func requirePermissions() throws {
    var missing: [String] = []
    if !CGPreflightPostEventAccess() { missing.append("Accessibility") }
    if !CGPreflightScreenCaptureAccess() { missing.append("Screen Recording") }
    if !missing.isEmpty {
        throw Failure(message: "missing \(missing.joined(separator: " and ")) permission. Grant it to the app that runs this task, such as Terminal, in System Settings > Privacy & Security, then quit and reopen that app")
    }
}

// MARK: Target window

struct Target {
    let pid: pid_t
    let window: CGWindowID
    let app: String
}

func windowList() -> [[String: Any]] {
    (CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]]) ?? []
}

func bounds(_ info: [String: Any]) -> CGRect? {
    guard let dictionary = info[kCGWindowBounds as String] as? NSDictionary else { return nil }
    return CGRect(dictionaryRepresentation: dictionary)
}

/// The app's frontmost normal window large enough to hold a terminal.
func findTarget(app: String) throws -> Target {
    for info in windowList() {
        guard (info[kCGWindowLayer as String] as? Int) == 0,
              (info[kCGWindowOwnerName as String] as? String) == app,
              let frame = bounds(info), frame.width >= 300, frame.height >= 200,
              let pid = info[kCGWindowOwnerPID as String] as? Int32,
              let window = info[kCGWindowNumber as String] as? UInt32
        else { continue }
        return Target(pid: pid, window: window, app: app)
    }
    throw Failure(message: "no on-screen \(app) window; open one with a shell prompt in its focused Pane")
}

func windowFrame(_ target: Target) throws -> CGRect {
    for info in windowList() where (info[kCGWindowNumber as String] as? UInt32) == target.window {
        if let frame = bounds(info) { return frame }
    }
    throw Failure(message: "the \(target.app) window closed")
}

func frontmost() -> pid_t { NSWorkspace.shared.frontmostApplication?.processIdentifier ?? -1 }

func requireFront(_ target: Target) throws {
    if frontmost() != target.pid {
        throw Failure(message: "\(target.app) is no longer the frontmost app; keep the screen untouched while the probe runs")
    }
}

func activate(_ target: Target) throws {
    NSRunningApplication(processIdentifier: target.pid)?.activate(options: [])
    let deadline = Date().addingTimeInterval(3)
    while frontmost() != target.pid && Date() < deadline { pause(0.05) }
    pause(0.3)
    try requireFront(target)
}

// MARK: Input

let source = CGEventSource(stateID: .hidSystemState)

func postKey(_ code: CGKeyCode, flags: CGEventFlags = []) {
    var modifiers: [(CGKeyCode, CGEventFlags)] = []
    if flags.contains(.maskControl) { modifiers.append((59, .maskControl)) }
    if flags.contains(.maskCommand) { modifiers.append((55, .maskCommand)) }
    if flags.contains(.maskShift) { modifiers.append((56, .maskShift)) }
    if flags.contains(.maskAlternate) { modifiers.append((58, .maskAlternate)) }
    var held: CGEventFlags = []
    for (key, flag) in modifiers {
        held.insert(flag)
        let event = CGEvent(keyboardEventSource: source, virtualKey: key, keyDown: true)!
        event.flags = held
        event.post(tap: .cghidEventTap)
        usleep(15_000)
    }
    for down in [true, false] {
        let event = CGEvent(keyboardEventSource: source, virtualKey: code, keyDown: down)!
        event.flags = flags
        event.post(tap: .cghidEventTap)
    }
    for (key, flag) in modifiers.reversed() {
        usleep(15_000)
        held.remove(flag)
        let event = CGEvent(keyboardEventSource: source, virtualKey: key, keyDown: false)!
        event.flags = held
        event.post(tap: .cghidEventTap)
    }
}

let keyCodes: [Character: CGKeyCode] = [
    "a": 0, "s": 1, "d": 2, "f": 3, "h": 4, "g": 5, "z": 6, "x": 7, "c": 8, "v": 9, "b": 11,
    "q": 12, "w": 13, "e": 14, "r": 15, "y": 16, "t": 17, "o": 31, "u": 32, "i": 34, "p": 35,
    "l": 37, "j": 38, "k": 40, "n": 45, "m": 46, " ": 49, "1": 18, "2": 19, "3": 20, "4": 21,
    "6": 22, "5": 23, "9": 25, "7": 26, "8": 28, "0": 29, "-": 27, "=": 24, ";": 41, "'": 39,
    ",": 43, ".": 47, "/": 44, "\\": 42, "[": 33, "]": 30, "`": 50,
]
let shifted: [Character: Character] = [
    "~": "`", "!": "1", "@": "2", "#": "3", "$": "4", "%": "5", "^": "6", "&": "7", "*": "8",
    "(": "9", ")": "0", "_": "-", "+": "=", "{": "[", "}": "]", "|": "\\", ":": ";", "\"": "'",
    "<": ",", ">": ".", "?": "/",
]

func typeText(_ target: Target, _ text: String) throws {
    for character in text {
        try requireFront(target)
        if character == "\n" {
            postKey(36)
        } else if let code = keyCodes[character] {
            postKey(code)
        } else if let base = shifted[character] ?? (character.isUppercase ? Character(character.lowercased()) : nil),
                  let code = keyCodes[base] {
            postKey(code, flags: .maskShift)
        } else {
            throw Failure(message: "cannot type a character outside the US keyboard layout")
        }
        usleep(15_000)
    }
}

func control(_ letter: Character) { postKey(keyCodes[letter]!, flags: .maskControl) }

// MARK: Capture

struct Image {
    let width: Int
    let height: Int
    let pixels: [UInt8]
}

/// Captures a rectangle in global display points at twice its size.
func capture(_ rect: CGRect) async throws -> Image {
    let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
    guard let display = content.displays.first(where: { $0.frame.contains(CGPoint(x: rect.midX, y: rect.midY)) }) ?? content.displays.first
    else { throw Failure(message: "no display") }
    let configuration = SCStreamConfiguration()
    configuration.sourceRect = rect.offsetBy(dx: -display.frame.minX, dy: -display.frame.minY)
    configuration.width = Int(rect.width * 2)
    configuration.height = Int(rect.height * 2)
    configuration.showsCursor = false
    let image = try await SCScreenshotManager.captureImage(
        contentFilter: SCContentFilter(display: display, excludingWindows: []), configuration: configuration)
    let width = image.width, height = image.height
    var pixels = [UInt8](repeating: 0, count: width * height * 4)
    pixels.withUnsafeMutableBytes { buffer in
        let context = CGContext(
            data: buffer.baseAddress, width: width, height: height, bitsPerComponent: 8, bytesPerRow: width * 4,
            space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
        context.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
    }
    return Image(width: width, height: height, pixels: pixels)
}

func changed(_ a: [UInt8], _ b: [UInt8], _ index: Int) -> Bool {
    abs(Int(a[index]) - Int(b[index])) + abs(Int(a[index + 1]) - Int(b[index + 1])) + abs(Int(a[index + 2]) - Int(b[index + 2])) > 60
}

/// Pixel indices that differ between two captures of the same rectangle.
func differences(_ a: Image, _ b: Image) -> Set<Int> {
    var result = Set<Int>()
    for index in stride(from: 0, to: min(a.pixels.count, b.pixels.count), by: 4) where changed(a.pixels, b.pixels, index) {
        result.insert(index / 4)
    }
    return result
}

/// Finds the row the focused Pane echoes into, then returns a strip of about 14 cells there.
func calibrate(_ target: Target) async throws -> CGRect {
    let window = try windowFrame(target)
    var idle = try await capture(window)
    var noise = Set<Int>()
    for _ in 0..<3 {
        pause(0.25)
        let next = try await capture(window)
        noise.formUnion(differences(idle, next))
        idle = next
    }
    try typeText(target, "asdf")
    pause(0.5)
    let typed = try await capture(window)
    try requireFront(target)
    control("u")
    pause(0.3)
    let change = differences(idle, typed).subtracting(noise)
    guard change.count >= 20 else {
        throw Failure(message: "typed text did not appear; give the focused Pane a shell prompt")
    }
    var minX = Int.max, minY = Int.max, maxX = 0, maxY = 0
    for pixel in change {
        let x = pixel % typed.width, y = pixel / typed.width
        minX = min(minX, x); maxX = max(maxX, x); minY = min(minY, y); maxY = max(maxY, y)
    }
    // Captures are at twice the point size.
    let typedRect = CGRect(x: Double(minX) / 2, y: Double(minY) / 2, width: Double(maxX - minX + 1) / 2, height: Double(maxY - minY + 1) / 2)
    guard typedRect.height <= 80, typedRect.width <= 200 else {
        throw Failure(message: "the window changed outside the typed text; close overlays and keep the screen idle")
    }
    // Four letters and the moved cursor cover about five cells. The strip holds one line of
    // echoed keys and the cursor, so a narrow split Pane still contains it.
    let cells = Double(keysPerLine + 2)
    let width = min(typedRect.width / 5 * cells, window.width - typedRect.minX - 4)
    let strip = CGRect(x: window.minX + typedRect.minX - 2, y: window.minY + typedRect.minY - 3, width: width + 2, height: typedRect.height + 6)
    progress(String(format: "measuring a %.0fx%.0f point strip at %.0f,%.0f", strip.width, strip.height, strip.minX, strip.minY))
    return strip
}

/// Waits until the window changes outside the strip, which shows the workload has started.
/// Fails if the strip changes too, because it would then time the workload instead of the echo.
func awaitActivity(_ target: Target, outside strip: CGRect) async throws {
    let window = try windowFrame(target)
    let local = strip.offsetBy(dx: -window.minX, dy: -window.minY)
    let deadline = Date().addingTimeInterval(60)
    var previous = try await capture(window)
    // Comparisons still to make after the workload starts, to catch overlap with the strip.
    var remaining: Int?
    while Date() < deadline && remaining != 0 {
        pause(0.3)
        let next = try await capture(window)
        let (inside, outside) = differences(previous, next).reduce(into: (0, 0)) { counts, pixel in
            if local.contains(CGPoint(x: Double(pixel % next.width) / 2, y: Double(pixel / next.width) / 2)) {
                counts.0 += 1
            } else {
                counts.1 += 1
            }
        }
        if inside > 0 { throw Failure(message: "the workload Pane overlaps the measured strip; widen the window") }
        if let count = remaining {
            remaining = count - 1
        } else if outside > 200 {
            remaining = 5
        }
        previous = next
    }
    if remaining != 0 { throw Failure(message: "the workload produced no visible output within 60 seconds") }
}

// MARK: Measurement

final class Grabber: NSObject, SCStreamOutput {
    let lock = NSLock()
    var latest: [UInt8]?
    var baseline: [UInt8]?
    var armedAt: UInt64 = 0
    var changeAt: UInt64?
    var frames = 0

    func stream(_ stream: SCStream, didOutputSampleBuffer buffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen, let pixels = buffer.imageBuffer else { return }
        if let attachments = CMSampleBufferGetSampleAttachmentsArray(buffer, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
           let raw = attachments.first?[.status] as? Int, let status = SCFrameStatus(rawValue: raw), status != .complete { return }
        // Presentation timestamps use the host clock, like mach_absolute_time.
        let host = UInt64(CMTimeGetSeconds(CMSampleBufferGetPresentationTimeStamp(buffer)) * 1e9)
        let bytes = snapshot(pixels)
        lock.lock(); defer { lock.unlock() }
        frames += 1
        if let base = baseline, changeAt == nil, host >= armedAt {
            var count = 0
            for index in stride(from: 0, to: min(base.count, bytes.count), by: 4) where changed(base, bytes, index) {
                count += 1
                if count >= 6 { changeAt = host; break }
            }
        }
        latest = bytes
    }

    /// Takes the latest frame as the baseline and waits for a change after `start`.
    func arm(_ start: UInt64) -> Bool {
        lock.withLock {
            baseline = latest
            changeAt = nil
            armedAt = start
            return baseline != nil
        }
    }

    func change() -> UInt64? { lock.withLock { changeAt } }

    func frameCount() -> Int { lock.withLock { frames } }

    func snapshot(_ buffer: CVPixelBuffer) -> [UInt8] {
        CVPixelBufferLockBaseAddress(buffer, .readOnly); defer { CVPixelBufferUnlockBaseAddress(buffer, .readOnly) }
        let height = CVPixelBufferGetHeight(buffer), width = CVPixelBufferGetWidth(buffer)
        let stride = CVPixelBufferGetBytesPerRow(buffer)
        let base = CVPixelBufferGetBaseAddress(buffer)!.assumingMemoryBound(to: UInt8.self)
        var out = [UInt8](repeating: 0, count: width * height * 4)
        for row in 0..<height { memcpy(&out[row * width * 4], base + row * stride, width * 4) }
        return out
    }
}

enum Mode: String { case keystroke, scroll }

func measure(_ target: Target, strip: CGRect, mode: Mode, samples: Int, phase: String?) async throws {
    let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
    guard let display = content.displays.first(where: { $0.frame.contains(strip.origin) }) ?? content.displays.first
    else { throw Failure(message: "no display") }
    let configuration = SCStreamConfiguration()
    configuration.sourceRect = strip.offsetBy(dx: -display.frame.minX, dy: -display.frame.minY)
    configuration.width = Int(strip.width * 2)
    configuration.height = Int(strip.height * 2)
    configuration.minimumFrameInterval = CMTime(value: 1, timescale: 240)
    configuration.queueDepth = 8
    configuration.showsCursor = false
    configuration.pixelFormat = kCVPixelFormatType_32BGRA
    let grabber = Grabber()
    let stream = SCStream(filter: SCContentFilter(display: display, excludingWindows: []), configuration: configuration, delegate: nil)
    try stream.addStreamOutput(grabber, type: .screen, sampleHandlerQueue: DispatchQueue(label: "frames", qos: .userInteractive))
    try await stream.startCapture()
    pause(0.8)
    let letters = Array("asdfjkl")
    var latencies: [Double] = []
    var misses = 0
    do {
        for index in 0..<samples {
            try requireFront(target)
            if mode == .keystroke && index > 0 && index % keysPerLine == 0 {
                control("u")
                pause(0.4)
            }
            // The baseline is the latest frame of an idle strip.
            pause(Double.random(in: 0.12...0.22))
            let start = nowNs()
            if !grabber.arm(start) { throw Failure(message: "ScreenCaptureKit delivered no frames") }
            switch mode {
            case .scroll:
                let event = CGEvent(scrollWheelEvent2Source: source, units: .line, wheelCount: 1, wheel1: index % 2 == 0 ? 3 : -3, wheel2: 0, wheel3: 0)!
                event.location = CGPoint(x: strip.midX, y: strip.midY)
                event.post(tap: .cghidEventTap)
            case .keystroke:
                postKey(keyCodes[letters[index % letters.count]]!)
            }
            let deadline = start + 1_000_000_000
            var seen: UInt64?
            while nowNs() < deadline {
                seen = grabber.change()
                if seen != nil { break }
                usleep(1000)
            }
            if let seen { latencies.append(Double(Int64(seen) - Int64(start)) / 1e6) } else { misses += 1 }
        }
    } catch {
        try? await stream.stopCapture()
        throw error
    }
    try await stream.stopCapture()
    var result: [String: Any] = [
        "measurement": mode == .keystroke ? "keystroke to pixels" : "wheel scroll to pixels",
        "app": target.app, "unit": "ms", "samples": latencies, "misses": misses, "frames": grabber.frameCount(),
    ]
    if let phase { result["phase"] = phase }
    emit(result)
}

// MARK: Commands

func run(app: String, mode: Mode, samples: Int, workload: String?) async throws {
    try requirePermissions()
    let target = try findTarget(app: app)
    try activate(target)
    if workload != nil {
        postKey(keyCodes["d"]!, flags: .maskCommand)  // Split Right
        pause(2)
        postKey(123, flags: [.maskCommand, .maskAlternate])  // Focus Pane Left
        pause(0.5)
    }
    try typeText(target, "clear; printf '\\e[2 q'; cat\n")
    pause(1)
    let strip = try await calibrate(target)
    if mode == .scroll {
        control("c")
        pause(0.3)
        try typeText(target, "clear; seq 1 3000\n")
        pause(1.5)
    }
    try await measure(target, strip: strip, mode: mode, samples: samples, phase: workload == nil ? nil : "idle")
    if let workload {
        postKey(124, flags: [.maskCommand, .maskAlternate])  // Focus Pane Right
        pause(0.5)
        try typeText(target, workload + "\n")
        postKey(123, flags: [.maskCommand, .maskAlternate])
        pause(0.5)
        do {
            try await awaitActivity(target, outside: strip)
            pause(1)
            try await measure(target, strip: strip, mode: mode, samples: samples, phase: "workload")
        } catch {
            try? closeWorkload(target)
            throw error
        }
        try closeWorkload(target)
        try activate(target)
    }
    try requireFront(target)
    if mode == .keystroke { control("c") }
    pause(0.3)
    try typeText(target, "printf '\\e[0 q'; clear\n")
}

/// Stops the workload in the right Pane and exits its shell, which closes that Pane.
func closeWorkload(_ target: Target) throws {
    try requireFront(target)
    postKey(124, flags: [.maskCommand, .maskAlternate])
    pause(0.5)
    control("c")
    pause(1)
    try typeText(target, "exit\n")
    pause(1.5)
}

func option(_ arguments: [String], _ name: String) -> String? {
    guard let index = arguments.firstIndex(of: name), index + 1 < arguments.count else { return nil }
    return arguments[index + 1]
}

mach_timebase_info(&timebase)
let arguments = Array(CommandLine.arguments.dropFirst())
let done = DispatchSemaphore(value: 0)
var status: Int32 = 0
Task {
    do {
        switch arguments.first {
        case "check":
            try requirePermissions()
        case "measure":
            guard let app = option(arguments, "--app"),
                  let mode = option(arguments, "--mode").flatMap(Mode.init(rawValue:)),
                  let samples = option(arguments, "--samples").flatMap(Int.init), samples > 0
            else { throw Failure(message: "usage: measure --app NAME --mode keystroke|scroll --samples N [--workload COMMAND]") }
            try await run(app: app, mode: mode, samples: samples, workload: option(arguments, "--workload"))
        default:
            throw Failure(message: "usage: check | measure --app NAME --mode keystroke|scroll --samples N [--workload COMMAND]")
        }
    } catch let failure as Failure {
        progress(failure.message)
        status = 2
    } catch {
        progress("a capture or input operation failed")
        status = 1
    }
    done.signal()
}
done.wait()
exit(status)
