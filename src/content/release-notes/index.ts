/**
 * Bundled release notes, keyed by the app version each one describes.
 *
 * These used to be `*.md` files collected by `import.meta.glob`. That was a
 * Vite API; Turbopack has no glob and cannot import a `.md` file at all
 * ("Unknown module type"), so registering a raw-text webpack loader would be
 * the only way back to loose Markdown files — a build-time dependency for one
 * string. The notes live here instead: same Markdown, still rendered by
 * `MarkdownContent`, one file to edit per release and no bundler machinery.
 *
 * The renderer supports paragraphs, headings, lists, links, code, quotes,
 * separators, hard line breaks and local images under `/release-notes/...`;
 * raw HTML is stripped before rendering. Keep entries to headline user-facing
 * changes. Escape any literal backtick as \` — these are template literals.
 */
export const RELEASE_NOTE_MARKDOWN = {
  "1.2.0": `# Sona 1.2.0

Dictation keeps the opening audio when recognition is busy. Microphone readiness follows the first real sample, including a Studio Display microphone, without scanning every supported recording format.

You can download an offline cleanup model for English dictation in Settings > Models on Mac, or Providers on Windows and Linux. Download progress, cancel, retry, and remove are built in. Light cleanup adds punctuation and removes fillers; it does not accept custom tones or spoken instructions. If the model changes words or numbers, or does not finish, Sona keeps the original.

Long meeting notes read the whole transcript in bounded parts and reconcile later decisions and deadlines. Notes become available before optional people and relationship summaries finish. Live microphone and call-audio voices have separate provisional labels, not guessed names; unresolved or unavailable voice evidence stays visible.

Optional cleanup stops at a deadline on desktop and iPhone and is cancelled with its recording. The iPhone saves a recoverable draft before optional history work.

New setup starts with your language, offers compatible speech models, and waits for a real practice transcript before it finishes.
`,
  "1.1.0": `# Sona 1.1.0

Meetings learn to listen sooner and to act. Catch-up and questions answer while a meeting is still running, from a provisional transcript Sona keeps in memory until the real one is written after the stop. FaceTime, Phone, and meeting-app calls can start a recording on their own once you grant each app in Settings, with a card to stop or revoke. A recording or a Granola, Otter, or Circleback export can be imported as a meeting.

Detection prompts on participation, not presence: a meeting app that is open but untouched since the microphone came on stays silent and says so on the status line. A call in Chrome, Safari, or Arc is read from the tab title on every tick with Accessibility alone, so a Meet you open after switching to the browser is still caught. A declined invitation never prompts, a meeting under way beats the next block on the calendar, and a capture stops only on its own evidence: its own event, its own microphone lane, a sleep that happened during it.

A finished meeting opens on its ledger: every thread, where it landed, and the verbatim receipt behind each claim, checked against the transcript before Sona shows it. The consent panel, the Prep and Wrap cards, and the recording pill share the app's type and tokens, enter and leave with one short motion, and work from the keyboard.

Saved prompts ask a question of a meeting, a person, or a series and can return JSON checked against a schema; three editable defaults ship. Follow-ups open in Mail, reminders carry the due day the notes named, the consent panel can announce the recording in the meeting's chat, and a deleted meeting stays restorable for thirty days.

The chat can now look things up. When you allow it to send matching quotes to your server, Sona searches your recordings, reads a meeting or a transcript, looks up a person, checks open loops and the calendar, and counts words and activity, at most three lookups per question, each shown as a step. The per-meeting Questions tab is gone; ask the chat instead. The chat can also offer changes: close a commitment, give it an owner, set a series template, add a vocabulary term, rename a speaker. Nothing applies without a press, and every change carries a receipt and an undo. The \`sona\` CLI and MCP server gain \`--upcoming\` and a consent-gated \`--loop-resolve\`. People pages gain organizations and a short relationship summary.

Dictation gains five spoken edits and a "Sona," cue for spoken instructions. Context capture works on Windows and Linux. Codex, Grok, and OMP hook events are typed, and their permission requests can be answered from Sona where the tool allows a reply.

A companion iPhone and Apple Watch recorder lives in \`mobile/\`; it pairs through the encrypted vault and hands in-person recordings to the Mac.

Upgraders keep their existing meeting-app list, so FaceTime and Phone detection is off until you tick them in Settings. The chat relay moves to turn version 2; pair the updated relay with this build.
`,
  "1.0.0": `# Sona 1.0.0

Sona introduces a new application identity, package name, data directory, log name, and agent-hook sidecar.

On the first launch, Sona can move settings, history, recordings, models, and configured provider keys from the Legacy app. Close the Legacy app before migration, then remove it using the normal uninstall flow for your platform. Sona needs new Microphone and Accessibility permissions because it is a new application identity.

Environment variables now use the \`SONA_\` prefix. The release does not add automatic updates, notarization, or distribution signing.
`,
};
