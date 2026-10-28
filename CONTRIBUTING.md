# Contributing to Sona

Thanks for helping improve Sona, a local-first Mac and iPhone app for dictation and meeting notes.

## ⚠️ Feature Freeze

**Sona is currently undergoing a feature freeze.** If you are submitting a PR which is a new feature that the community has not asked for, it will be rejected. If the community has asked for it, or you have explicitly gathered support, it may still be considered.

**Bug fixes are the top priority.** There are 60+ issues to fix. Please focus your contributions on fixing bugs and improving stability.

## 📖 Philosophy

Sona aims to be the most forkable speech-to-text app. The goal is to create both a useful tool and a foundation for others to build upon—a well-patterned, simple codebase that serves the community. We prioritize:

- **Simplicity**: Clear, maintainable code over clever solutions
- **Extensibility**: Make it easy for others to fork and customize
- **Privacy**: Keep everything local and offline
- **Accessibility**: Free tooling that belongs in everyone's hands

## 🚀 Getting Started

### Prerequisites

Before you begin, ensure you have the following installed:

- [Rust](https://rustup.rs/) (current stable)
- [Bun](https://bun.sh/) and CMake for the Mac recording engine
- Xcode 26 and the macOS, iOS, and watchOS SDKs (see [BUILD.md](BUILD.md))

### Setting Up Your Development Environment

1. **Fork the repository** on GitHub

2. **Clone your fork**:

   ```bash
   git clone git@github.com:YOUR_USERNAME/sona.git
   cd sona
   ```

3. **Add upstream remote**:

   ```bash
   git remote add upstream git@github.com:aktanazat/sona.git
   ```

4. **Install script dependencies**:

   ```bash
   bun install --frozen-lockfile
   ```

5. **Build the app you are changing**: Follow [BUILD.md](BUILD.md) for the native Mac app, its Rust engine and helpers, or the iPhone simulator app. Model resources needed at startup are checked in; speech models are downloaded through Sona when selected.

### Understanding the Codebase

- `macos/Sona/` contains the native SwiftUI Mac interface. It launches the Rust engine as a private helper and communicates over a local socket.
- `src-tauri/src/` contains recording, transcription, meeting, sync, command-line, and native-bridge code. Tauri remains an engine dependency; it no longer supplies a web desktop interface.
- `mobile/Sona/` contains the iPhone app. Its Watch companion and dictation keyboard are embedded in the phone package.
- `cloudflare/sona-companion/` provides the paired sync service.

See [BUILD.md](BUILD.md) for the current build and check commands.

## 🐛 Reporting Bugs

### Before Submitting a Bug Report

1. **Search existing issues** at [github.com/aktanazat/sona/issues](https://github.com/aktanazat/sona/issues)
2. **Check discussions** at [github.com/aktanazat/sona/discussions](https://github.com/aktanazat/sona/discussions)
3. **Check current source** to see whether the issue has already been fixed

### Submitting a Bug Report

When creating a bug report, please include:

**System Information:**

- App version and build, from the app's About screen
- Mac or iPhone model and operating-system version
- Audio input or output device, if the report concerns recording or playback

**Bug Details:**

- Clear description of the bug
- Steps to reproduce
- Expected behavior
- Actual behavior
- Screenshots or logs if applicable
- Information from debug mode if relevant

Use the [Bug Report template](.github/ISSUE_TEMPLATE/bug_report.md) when creating an issue.

## 💡 Suggesting Features

We use GitHub Discussions for feature requests rather than issues. This keeps issues focused on bugs and actionable tasks while allowing more open-ended conversations about features.

### Before Suggesting a Feature

1. **Search existing discussions** at [github.com/aktanazat/sona/discussions](https://github.com/aktanazat/sona/discussions)
2. **Check common feature requests** (these live in the upstream cjpais/Handy project Sona was forked from):
   - [Post-processing / Editing Transcripts](https://github.com/cjpais/Handy/discussions/168)
   - [Keyboard Shortcuts / Hotkeys](https://github.com/cjpais/Handy/discussions/211)

### Submitting a Feature Request

1. Go to [Discussions](https://github.com/aktanazat/sona/discussions)
2. Click "New discussion"
3. Choose the appropriate category (Ideas, Feature Requests, etc.)
4. Describe your feature idea including:
   - The problem you're trying to solve
   - Your proposed solution
   - Any alternatives you've considered
   - How it fits with Sona's philosophy

## 🔧 Making Code Contributions

### Before You Start

**This is critical:** Before writing any code, please do the following:

1. **Search existing issues and PRs** - Check both open AND closed issues and pull requests. Someone may have already addressed this, or there may be a reason it was closed.
   - [Open issues](https://github.com/aktanazat/sona/issues)
   - [Closed issues](https://github.com/aktanazat/sona/issues?q=is%3Aissue+is%3Aclosed)
   - [Open PRs](https://github.com/aktanazat/sona/pulls)
   - [Closed PRs](https://github.com/aktanazat/sona/pulls?q=is%3Apr+is%3Aclosed)

2. **If something was previously closed** - If you want to revisit a closed issue or PR, you need to:
   - Provide a strong argument for why it should be reconsidered
   - Gather community feedback first via [Discussions](https://github.com/aktanazat/sona/discussions)
   - Link to that discussion in your PR

3. **Get community feedback for features** - PRs with demonstrated community interest are **much more likely to be merged**. Start a discussion, get feedback, and link to it in your PR. This helps ensure Sona stays focused and useful for the most people without becoming bloated.

Community feedback is essential to keeping Sona the best it can be for everyone. It helps prioritize what matters most and prevents feature creep.

### Development Workflow

1. **Create a feature branch**:

   ```bash
   git checkout -b feature/your-feature-name
   # or
   git checkout -b fix/your-bug-fix
   ```

2. **Make your changes**:
   - Follow the existing code style and keep commits focused
   - Explain only constraints the code cannot make clear

3. **Test the affected app**:
   - Build the native Mac or iPhone target you changed
   - Exercise the changed path and its relevant consent or error state
   - Run the applicable checks in [BUILD.md](BUILD.md)

4. **Commit your changes**:

   ```bash
   git add .
   git commit -m "macos: describe the change"
   ```

   Use a short scope and an imperative title, such as `macos:`, `mobile:`, `core:`, or `docs:`. Explain why in the body when the title is not enough.

5. **Keep your fork updated**:

   ```bash
   git fetch upstream
   git rebase upstream/main
   ```

6. **Push to your fork**:

   ```bash
   git push origin feature/your-feature-name
   ```

7. **Create a Pull Request**:
   - Go to the [Sona repository](https://github.com/aktanazat/sona)
   - Click "New Pull Request"
   - Select your fork and branch
   - Fill out the PR template completely, including:
     - Clear description of changes
     - Links to related issues or discussions
     - **Community feedback** (especially important for features)
     - How you tested the changes
     - Screenshots/videos if applicable
     - Breaking changes (if any)

   **Remember:** PRs with community support are prioritized. If you haven't already, start a [discussion](https://github.com/aktanazat/sona/discussions) to gather feedback before or alongside your PR. It is not explicitly required to gather feedback, but it certainly helps your PR get merged faster.

### AI Assistance Disclosure

**AI-assisted PRs are welcome!** Use whatever tools help you contribute, just be upfront about it.

In your PR description, please include:

- Whether AI was used (yes/no)
- Which tools were used (e.g., "Claude Code", "GitHub Copilot", "ChatGPT")
- How extensively it was used (e.g., "generated boilerplate", "helped debug", "wrote most of the code")

### Code Style Guidelines

**Rust:**

- Follow standard Rust formatting (`bun run format:backend`)
- Run `bun run lint:backend` (`cargo clippy`) and address warnings
- Keep errors explicit and public contracts stable

**SwiftUI:**

- Use native controls and the existing Mac or phone theme
- Keep recording, permission, and consent actions directly reachable
- Check both light and dark appearance and larger text sizes

**TypeScript scripts and services:**

- Keep types strict and run the relevant script or service checks

**General:**

- Write self-documenting code
- Comment only when an external constraint is not clear from the code
- Keep functions small and single-purpose
- Prioritize readability over cleverness

### Testing Your Changes

Build and run the changed native app using [BUILD.md](BUILD.md). Check the affected flow on an isolated simulator or Mac profile. For recording work, test the actual device and consent path when you have access; a successful simulator build does not prove microphone, Watch transfer, or pairing behavior.

For a Mac package, prepare the engine and both private helpers before building the Xcode app. Inspect the resulting bundle with `macos/scripts/inspect-package.sh`. An unsigned development bundle is not a signed release and should not replace an installed signed app.

## 📝 Documentation Contributions

Documentation improvements are highly valued! You can contribute by:

- Improving README.md, BUILD.md, or this CONTRIBUTING.md
- Keeping public API documentation current
- Creating tutorials or guides
- Improving error messages
- Updating the project website content

## 🤝 Community Guidelines

- **Be respectful and inclusive** - We welcome contributors of all skill levels
- **Be patient** - This is maintained by a small team, responses may take time
- **Be constructive** - Focus on solutions and improvements
- **Be collaborative** - Help others and share knowledge
- **Search first** - Check existing issues/discussions before creating new ones

## 🎯 Good First Issues

Look for issues labeled `good first issue` or `help wanted` if you're new to the project. These are typically:

- Well-defined and scoped
- Good for learning the codebase
- Mentor support available

## 📜 License

By contributing to Sona, you agree that your contributions will be licensed under the MIT License. See [LICENSE](LICENSE) for details.

---

**Thank you for contributing to Sona!** Your efforts help make speech-to-text technology more accessible, private, and extensible for everyone.
