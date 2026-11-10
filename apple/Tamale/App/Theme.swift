// The web UI's palette (src/web/app.css), so the app and the page feel like one product.

import SwiftUI

enum Theme {
    static let background = Color(hex: 0x17181D)
    static let surface = Color(hex: 0x1E2026)
    static let surface2 = Color(hex: 0x24262E)
    static let surface3 = Color(hex: 0x2C2F38)
    static let border = Color(hex: 0x33363F)
    static let foreground = Color(hex: 0xD4D8E0)
    static let dim = Color(hex: 0x9AA0B0)
    static let faint = Color(hex: 0x848A9C)
    static let bright = Color(hex: 0xE8ECF4)
    static let accent = Color(hex: 0x5C9CF5)
    static let added = Color(hex: 0x4FBF7A)
    static let warning = Color(hex: 0xE2B34C)
    static let error = Color(hex: 0xE06C75)
    static let violet = Color(hex: 0xA78BFA)
    /// The tamale chef's amber, as on the web.
    static let chef = Color(hex: 0xF2A93B)
}

extension Color {
    init(hex: UInt32) {
        self.init(
            .sRGB,
            red: Double((hex >> 16) & 0xff) / 255,
            green: Double((hex >> 8) & 0xff) / 255,
            blue: Double(hex & 0xff) / 255)
    }
}

extension Font {
    static let code = Font.system(.footnote, design: .monospaced)
}

/// "12s", "3m 4s", "1h 2m", as the web UI writes durations.
func formatElapsed(_ seconds: Int) -> String {
    if seconds < 60 { return "\(seconds)s" }
    let m = seconds / 60
    return m < 60 ? "\(m)m \(seconds % 60)s" : "\(m / 60)h \(m % 60)m"
}

/// "950", "12.3k", "4.5M".
func formatTokens(_ n: Int) -> String {
    if n < 1000 { return "\(n)" }
    if n < 1_000_000 { return n < 10_000 ? String(format: "%.1fk", Double(n) / 1000) : "\(n / 1000)k" }
    return String(format: "%.1fM", Double(n) / 1_000_000)
}

/// "just now", "5m ago", "3h ago", "2d ago".
func formatIdle(_ seconds: Double) -> String {
    let s = Int(seconds)
    if s < 60 { return "just now" }
    if s < 3600 { return "\(s / 60)m ago" }
    if s < 86400 { return "\(s / 3600)h ago" }
    return "\(s / 86400)d ago"
}
