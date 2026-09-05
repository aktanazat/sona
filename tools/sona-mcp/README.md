# sona-mcp

An MCP server over Sona's meeting corpus. Eight tools, stdio transport, no
state: each call spawns the installed Sona binary with one of its headless
flags and hands the JSON back.

Everything it can reach is behind consent rows that are off on install.
**Settings > Agents > External access** gates every request: seven are
read-only, and `sona_loop_resolve` reads its loop revision before it writes.
That one writing tool also needs **Settings > Agents > External mutations**.
With either row off it returns `consent_required` naming the missing setting.
Nothing else on this surface writes.

## Install

```bash
cd tools/sona-mcp
bun install
```

Register it with your agent:

```bash
# Claude Code
claude mcp add sona -- bun /absolute/path/to/sona/tools/sona-mcp/src/index.ts

# Codex
codex mcp add sona -- bun /absolute/path/to/sona/tools/sona-mcp/src/index.ts
```

The server resolves Sona at `/Applications/Sona.app/Contents/MacOS/sona`.
Point `SONA_BIN` somewhere else for a dev build:

```bash
claude mcp add sona --env SONA_BIN=/path/to/sona/src-tauri/target/debug/sona \
  -- bun /absolute/path/to/sona/tools/sona-mcp/src/index.ts
```

## Tools

| Tool                | Arguments                              | Runs                                                                    |
| ------------------- | -------------------------------------- | ----------------------------------------------------------------------- |
| `sona_search`       | `query`, `scope?`, `limit?`            | `sona --query … [--scope …] [--limit …]`                                |
| `sona_meetings`     | `last?` \| (`from` + `to`)             | `sona --meetings [--last …] [--from … --to …]`                          |
| `sona_meeting`      | `meeting_id`                           | `sona --meeting <id>`                                                   |
| `sona_transcript`   | `meeting_id`                           | `sona --transcript <id>`                                                |
| `sona_action_items` | `status?`, `side?`, `after?`, `limit?` | `sona --loops [--status …] [--mine\|--waiting] [--after …] [--limit …]` |
| `sona_people`       | `name`, `limit?`                       | `sona --people <name> [--limit …]`                                      |
| `sona_upcoming`     | `limit?`                               | `sona --upcoming [--limit …]`                                           |
| `sona_loop_resolve` | `loop_id`                              | `sona --loop-resolve <loop_id>` — **writes**                            |

`scope` is one of `all`, `meetings`, `dictations`, `people`, `loops`. `status`
is `open` or `done`. `side` is `mine` (what you owe) or `waiting` (what
somebody else owes you). Pass an action-items `next_cursor` as `after` with
the same filters; `has_more` is true only when that cursor is present.

Sona's `--events` flag — the receipt and workflow-run stream — is deliberately
CLI-only. It pages by receipt id, which is a cursor an agent has no use for
between calls on a server that keeps no state.

## What comes back

One JSON object per call, as one text block. Every payload carries a
`schema_version`. Rows from `sona_upcoming` are the exception: calendar
occurrences have no `sona://` address. Other rows carry an address the app can
open: `sona://meeting/<uuid>`, `sona://loop/<id>`, `sona://person/<uuid>`,
`sona://dictation/<id>`.

An empty array is two different answers, so the three reads that can hide a
degraded source carry `reason` beside their rows. `null` means every source
behind the question was read and the rows are all there are.

| `reason`               | Means                                                                                                                                                     |
| ---------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `no_rows`              | Everything was read and nothing matched. Asking again the same way will not change that.                                                                  |
| `no_searchable_tokens` | The query held no word to match.                                                                                                                          |
| `semantic_unavailable` | The recall model is not on this machine, so only literal word matching ran. Rows worded differently than the question are missing and cannot be reported. |
| `filtered_out`         | The corpus holds loops; `status` or `side` excluded every one of them.                                                                                    |
| `awaiting_continuity`  | A meeting's continuity pass has not succeeded yet, so its ledger rows are not reportable. Ask again later.                                                |
| `people_index_empty`   | Diarization has not named anybody yet, so no name can match.                                                                                              |

Each read produces only its own values: `sona_search` produces
`no_searchable_tokens`, `semantic_unavailable` and `no_rows`;
`sona_action_items` produces `awaiting_continuity`, `filtered_out` and
`no_rows`; `sona_people` produces `people_index_empty` and `no_rows`.

## Refusals

Errors are MCP errors. The message leads with Sona's own token — `sona
not_found: No loop … in this corpus.` — and `data.code` carries the same token
for a caller that reads structured error data:

| `data.code`        | Means                                                                 |
| ------------------ | --------------------------------------------------------------------- |
| `consent_required` | External access is off. `data.settingsPath` names the row to turn on. |
| `unavailable`      | The corpus is not open — usually a locked login keychain.             |
| `invalid_request`  | A bad id, date or limit.                                              |
| `not_found`        | The id parsed but names nothing in this corpus.                       |
| `not_installed`    | No Sona binary where this server looked.                              |
| `timed_out`        | Sona did not answer in 30s.                                           |
| `failed`           | Sona reached the corpus and the read did not finish.                  |

## Tests

```bash
bun test          # argv mapping, refusal passthrough, and the server on an
                  # in-memory transport — all against a stub binary
bunx tsc --noEmit # the only check that sees a dead import
```

Both run from the repo root as `bun run test:mcp` and `bun run typecheck:mcp`,
which install this package first; CI runs them in `code-quality.yml`.
