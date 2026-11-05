import Foundation
import SwiftUI

enum TimeAgo {
    /// Human “time ago” that only changes on minute / hour / day boundaries
    /// (not every second like `Text(..., style: .relative)`).
    static func format(_ date: Date, relativeTo now: Date = Date()) -> String {
        let seconds = Int(now.timeIntervalSince(date))
        if seconds < 0 {
            return "just now"
        }
        if seconds < 60 {
            return "just now"
        }
        let minutes = seconds / 60
        if minutes < 60 {
            return minutes == 1 ? "1 min ago" : "\(minutes) min ago"
        }
        let hours = minutes / 60
        if hours < 24 {
            return hours == 1 ? "1 hr ago" : "\(hours) hr ago"
        }
        let days = hours / 24
        if days < 7 {
            return days == 1 ? "1 day ago" : "\(days) days ago"
        }
        let weeks = days / 7
        if weeks < 5 {
            return weeks == 1 ? "1 wk ago" : "\(weeks) wk ago"
        }
        let months = days / 30
        if months < 12 {
            return months == 1 ? "1 mo ago" : "\(months) mo ago"
        }
        let years = days / 365
        return years <= 1 ? "1 yr ago" : "\(years) yr ago"
    }
}

/// Owns its own minute timer so a tick cannot invalidate the chat layout graph.
struct TimeAgoLabel: View {
    let date: Date
    @State private var now = Date()

    var body: some View {
        Text(TimeAgo.format(date, relativeTo: now))
            .onReceive(Self.ticker) { now = $0 }
    }

    private static let ticker = Timer.publish(every: 60, on: .main, in: .common).autoconnect()
}
