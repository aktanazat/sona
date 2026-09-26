import SwiftUI

/// A question of your own, on the right in a bubble: the well it was typed
/// in, filled a shade deeper, as Halcyon's thread draws it.
struct ChatQuestion: View {
    let message: String

    var body: some View {
        HStack(spacing: 0) {
            Spacer(minLength: 56)
            Text(message)
                .bodyText(14)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.horizontal, 14)
                .padding(.vertical, 9)
                .background(Theme.selection, in: RoundedRectangle(cornerRadius: Theme.radiusCard))
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("You asked: \(message)")
    }
}

/// An answer is prose on the surface, not a card and not a bubble: it is the
/// thing the sheet exists to show, and putting a container around it would
/// make it look like an aside to something else. Only the addresses inside it
/// are chrome, because only they are pressable. The answer that lands while
/// the chat is open types itself out; history is simply there.
struct ChatAnswer: View, Equatable {
    let message: String
    /// When this answer landed, for the one that arrived while the chat was
    /// on screen.
    var landed: Date?
    let store: ChatStore

    var body: some View {
        ChatRevealText(text: prose, landed: landed)
            .font(TypeScale.body(14))
            .foregroundStyle(Theme.ink)
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: .infinity, alignment: .leading)
            .environment(
                \.openURL,
                OpenURLAction { url in
                    store.openLink(url.absoluteString)
                    return .handled
                })
    }

    /// The addresses are links in the run of text rather than buttons beside
    /// it, so the sentence still reads as a sentence. The prose between them
    /// is read as inline Markdown, because that is how an assistant writes
    /// emphasis and code, and a reader should see the word, not the stars
    /// around it. Lines and paragraphs are kept as the answer laid them out.
    private var prose: AttributedString {
        var result = AttributedString()
        for segment in ChatSegment.scan(message) {
            switch segment {
            case let .text(text):
                result += Self.inline(text)
            case let .link(link):
                var run = AttributedString(link)
                run.underlineStyle = .single
                if let url = URL(string: link) {
                    run.link = url
                }
                result += run
            }
        }
        return result
    }

    /// Text that does not parse as Markdown is still an answer; it is shown
    /// as written rather than lost to a parser's opinion of it.
    private static func inline(_ text: String) -> AttributedString {
        (try? AttributedString(
            markdown: text,
            options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace)))
            ?? AttributedString(text)
    }

    /// Two answers are the same answer when their words and their landing
    /// are. A status read hands every row a fresh copy of the same words,
    /// and saying so here is what lets SwiftUI skip the Markdown above for
    /// every row that did not change.
    nonisolated static func == (lhs: ChatAnswer, rhs: ChatAnswer) -> Bool {
        lhs.message == rhs.message && lhs.landed == rhs.landed
    }
}

/// How long a landed answer takes to arrive on screen.
enum ChatReveal {
    /// Letters fade in over this many places behind the leading edge.
    static let fade = 12.0
    /// After the last word the caret blinks once, then goes.
    static let blink: TimeInterval = 1.0

    /// About 160 letters a second, taking no less than 0.6 seconds and no
    /// more than 2.4.
    static func typing(_ count: Int) -> TimeInterval {
        min(2.4, max(0.6, Double(count) / 160))
    }

    static func duration(_ count: Int) -> TimeInterval {
        typing(count) + blink
    }
}

/// A landed answer arriving, as Halcyon's does: the words appear from the
/// start with a soft leading edge and a caret, then the caret blinks and
/// goes. The text is laid out whole from the first frame, so no word jumps
/// lines as it arrives and VoiceOver reads all of it at once. Once it has
/// arrived, and always under Reduce Motion, it is plain selectable text.
struct ChatRevealText: View {
    let text: AttributedString
    let landed: Date?
    @State private var arrived = false
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        let count = text.characters.count
        if let landed, !arrived, !reduceMotion,
           Date.now < landed.addingTimeInterval(ChatReveal.duration(count)) {
            TimelineView(.animation) { context in
                Text(text)
                    .textRenderer(ChatRevealRenderer(
                        count: count, elapsed: context.date.timeIntervalSince(landed)))
            }
            .task {
                let left = landed.addingTimeInterval(ChatReveal.duration(count)).timeIntervalSinceNow
                try? await Task.sleep(for: .seconds(max(0, left)))
                guard !Task.isCancelled else { return }
                arrived = true
            }
        } else {
            Text(text).textSelection(.enabled)
        }
    }
}

/// Draws the letters that have arrived by `elapsed` seconds, each fading in
/// over a short run, and the caret just past the newest one.
private struct ChatRevealRenderer: TextRenderer {
    let count: Int
    let elapsed: TimeInterval

    func draw(layout: Text.Layout, in context: inout GraphicsContext) {
        let typing = ChatReveal.typing(count)
        let shown = elapsed >= typing ? Double.infinity : elapsed / typing * (Double(count) + ChatReveal.fade)
        var start = 0.0
        var caret: CGRect?
        /* The caret stays inside the text's own bounds. Reserving room past
         * them makes SwiftUI draw the lines wider than it measured them,
         * which pushes words past the margin. */
        let right = context.clipBoundingRect.maxX
        for line in layout {
            let row = line.typographicBounds.rect
            func mark(after edge: CGFloat) -> CGRect {
                CGRect(x: min(edge + 1, right - 2), y: row.minY, width: 2, height: row.height)
            }
            for run in line {
                let first = start
                start += Double(run.count)
                // Most of the time a run has either wholly arrived or not begun.
                if shown - (start - 1) >= ChatReveal.fade {
                    context.draw(run)
                    caret = mark(after: run.typographicBounds.rect.maxX)
                    continue
                }
                for offset in run.indices {
                    let alpha = min(1, (shown - first - Double(offset)) / ChatReveal.fade)
                    // Each letter is fainter than the one before it, so none after this shows.
                    guard alpha > 0 else { break }
                    let letter = run[offset]
                    var faded = context
                    faded.opacity = alpha
                    faded.draw(letter)
                    caret = mark(after: letter.typographicBounds.rect.maxX)
                }
            }
        }
        let afterwards = elapsed - typing
        let lit = afterwards < 0 || (afterwards >= ChatReveal.blink / 2 && afterwards < ChatReveal.blink)
        guard lit, let caret else { return }
        context.fill(Path(roundedRect: caret, cornerRadius: 1), with: .foreground)
    }
}

/// The agent's mark, from Halcyon: a four-pointed star with a small one above
/// it, filled with whatever colour it is given.
struct ChatSparkle: Shape {
    func path(in rect: CGRect) -> Path {
        let side = min(rect.width, rect.height)
        let large = side * 0.8
        let small = side * 0.38
        var path = Path()
        Self.addStar(in: CGRect(x: rect.minX, y: rect.maxY - large, width: large, height: large), to: &path)
        Self.addStar(in: CGRect(x: rect.minX + side - small, y: rect.minY, width: small, height: small), to: &path)
        return path
    }

    /// Four points on the middle of the box's edges, joined by curves that
    /// pinch in toward the centre.
    private static func addStar(in box: CGRect, to path: inout Path) {
        let centre = CGPoint(x: box.midX, y: box.midY)
        let pinch = box.width * 0.14
        path.move(to: CGPoint(x: centre.x, y: box.minY))
        path.addQuadCurve(to: CGPoint(x: box.maxX, y: centre.y), control: CGPoint(x: centre.x + pinch, y: centre.y - pinch))
        path.addQuadCurve(to: CGPoint(x: centre.x, y: box.maxY), control: CGPoint(x: centre.x + pinch, y: centre.y + pinch))
        path.addQuadCurve(to: CGPoint(x: box.minX, y: centre.y), control: CGPoint(x: centre.x - pinch, y: centre.y + pinch))
        path.addQuadCurve(to: CGPoint(x: centre.x, y: box.minY), control: CGPoint(x: centre.x - pinch, y: centre.y - pinch))
        path.closeSubpath()
    }
}

/// The one curve every change in the thread moves on. With Reduce Motion
/// nothing moves: new parts fade in where they stand, or are simply there.
enum ChatMotion {
    static let curve: Animation = .easeOut(duration: 0.3)

    static func arrival(_ reduceMotion: Bool) -> Animation? {
        reduceMotion ? nil : curve
    }
}

/// How everything new enters the thread, as in Halcyon: out of a light blur,
/// rising a little into place. The entry carries its own curve, so nothing
/// already on screen moves with it, and a status read needs no animation of
/// its own. Reduce Motion keeps the fade and drops the blur and the rise.
/// Leaving is immediate, because an entry that goes has been replaced, and
/// fading it out would show both at once.
struct ChatEntrance: ViewModifier {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    func body(content: Content) -> some View {
        let arrival: AnyTransition = reduceMotion ? .opacity : AnyTransition(ChatArrival())
        return content.transition(
            .asymmetric(insertion: arrival.animation(ChatMotion.curve), removal: .identity))
    }
}

private struct ChatArrival: Transition {
    func body(content: Content, phase: TransitionPhase) -> some View {
        content
            .opacity(phase.isIdentity ? 1 : 0)
            .blur(radius: phase.isIdentity ? 0 : 6)
            .offset(y: phase.isIdentity ? 0 : 8)
    }
}

/// Which ends of the thread have more beyond them, so only those ends fade:
/// the settings tab strip's overflow, stood on its end.
struct ChatEdges: Equatable {
    var above = false
    var below = false

    init() {}

    init(_ geometry: ScrollGeometry) {
        let insets = geometry.contentInsets
        let seenTop = geometry.contentOffset.y + insets.top
        let seenBottom = geometry.contentOffset.y + geometry.containerSize.height - insets.bottom
        above = seenTop > 1
        below = seenBottom < geometry.contentSize.height - 1
    }
}

/// The mask over the thread: a soft fade at an end with more beyond it, and
/// solid everywhere else, so the thread runs under the header and into the
/// composer instead of stopping at a line.
struct ChatFades: View {
    let edges: ChatEdges
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        VStack(spacing: 0) {
            fade(edges.above, from: .top)
            Rectangle()
            fade(edges.below, from: .bottom)
        }
        .animation(ChatMotion.arrival(reduceMotion), value: edges)
    }

    /// Solid while nothing is past this end; a clear-to-solid ramp once
    /// something is. The solid layer fades out over the ramp rather than the
    /// ramp's colours changing, because an opacity is what animates cleanly.
    private func fade(_ on: Bool, from edge: UnitPoint) -> some View {
        ZStack {
            Color.black.opacity(on ? 0 : 1)
            LinearGradient(colors: [.clear, .black], startPoint: edge, endPoint: edge == .top ? .bottom : .top)
        }
        .frame(height: 28)
    }
}

/// An empty chat, as Halcyon's opens: the agent's mark, the one question the
/// sheet is for, and a line on what it can be asked. The questions to press
/// sit on the composer, where the next one is typed.
struct ChatEmptyState: View {
    let workspace: AgentPanelWorkspace

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            ChatSparkle()
                .fill(Theme.accent)
                .frame(width: 18, height: 18)
                .padding(.bottom, 4)
                .accessibilityHidden(true)
            Text(workspace.greeting).headlineText()
            Text(workspace.reach)
                .metaText(Theme.inkSecondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// The questions an empty chat offers, one press each, wrapping onto a second
/// row when the sheet is too narrow for them side by side.
struct ChatSuggestionPills: View {
    let suggestions: [String]
    let ask: (String) -> Void

    var body: some View {
        ChatFlowRow {
            ForEach(suggestions, id: \.self) { suggestion in
                ChatSuggestionPill(title: suggestion) { ask(suggestion) }
            }
        }
    }
}

/// One suggestion: a question in a hairline capsule, the unchosen pill of
/// Halcyon's composer in Sona's surface and border.
private struct ChatSuggestionPill: View {
    let title: String
    let action: () -> Void
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            Text(title)
                .font(TypeScale.body(13))
                .foregroundStyle(hovering ? Theme.ink : Theme.inkSecondary)
                .lineLimit(1)
                .truncationMode(.middle)
                .padding(.horizontal, 12)
                .frame(height: 28)
                .background(hovering ? Theme.selection : Theme.surface, in: Capsule())
                .overlay(Capsule().strokeBorder(Theme.border, lineWidth: 1))
                .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .help(title)
    }
}

/// Halcyon's wrapping row for pills. A pill wider than the row is offered
/// the row's width, so a long meeting title truncates instead of running
/// past the edge.
private struct ChatFlowRow: Layout {
    var spacing: CGFloat = 6

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let width = proposal.width ?? .infinity
        var x: CGFloat = 0, y: CGFloat = 0, rowHeight: CGFloat = 0, widest: CGFloat = 0
        for view in subviews {
            let size = view.sizeThatFits(ProposedViewSize(width: proposal.width, height: nil))
            if x + size.width > width, x > 0 {
                y += rowHeight + spacing
                x = 0
                rowHeight = 0
            }
            x += size.width + spacing
            widest = max(widest, x - spacing)
            rowHeight = max(rowHeight, size.height)
        }
        /* Claims the full proposed width: placing rows in exactly the widest
         * row's width lets floating-point drift wrap one more pill than was
         * measured, overlapping what follows. */
        return CGSize(width: proposal.width ?? widest, height: y + rowHeight)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        var x = bounds.minX, y = bounds.minY, rowHeight: CGFloat = 0
        for view in subviews {
            let size = view.sizeThatFits(ProposedViewSize(width: bounds.width, height: nil))
            if x + size.width > bounds.maxX, x > bounds.minX {
                y += rowHeight + spacing
                x = bounds.minX
                rowHeight = 0
            }
            view.place(at: CGPoint(x: x, y: y), proposal: ProposedViewSize(size))
            x += size.width + spacing
            rowHeight = max(rowHeight, size.height)
        }
    }
}
