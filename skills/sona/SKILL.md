---
name: sona
description: Read and act on the Sona meeting corpus on this Mac — meetings, transcripts, people, open loops, receipts, and the week ahead — through the `sona` CLI or the `sona-mcp` server. Use when the user asks what was said in a meeting, what they owe somebody, who a person is, what is scheduled, or asks to close a loop.
---

# Sona

Sona is a local meeting-notes and dictation app. Its corpus lives on this Mac,
encrypted. Nothing below reaches a network.

## Consent

Two settings rows, both off on install:

- **Settings > Agents > External access** — required by every request. Eight
  verbs are read-only. `--loop-resolve` reads its loop revision before it writes.
- **Settings > Agents > External mutations** — also required by
  `--loop-resolve`, the one write.

With either needed row off, the request exits `1` and prints
`{"schema_version":3,"error":"consent_required","message":"…","settings_path":"…"}`
on stderr. Tell the user the `settings_path`; do not retry.

## CLI

One verb per invocation. One JSON value on stdout, or one JSON refusal on
stderr. Exit codes: `0` answered, `2` bad input (`invalid_request`), `1`
everything else (`consent_required`, `unavailable`, `not_found`, `failed`).

| Flag                                                                  | What it answers                                                  |
| --------------------------------------------------------------------- | ---------------------------------------------------------------- |
| `--query <TEXT> [--scope S] [--limit N]`                              | Search. `S` ∈ `all`\|`meetings`\|`dictations`\|`people`\|`loops` |
| `--meetings [--last N \| --from D --to D]`                            | Retained meetings, newest first. `D` is local `YYYY-MM-DD`       |
| `--meeting <MEETING_ID>`                                              | One meeting: summary, headline, notes, loops                     |
| `--transcript <MEETING_ID>`                                           | One meeting's speaker-labeled lines                              |
| `--loops [--status open\|done] [--mine\|--waiting] [--after LOOP_ID]` | Loops and commitments across the corpus                          |
| `--people <NAME>`                                                     | A person by name, alias or calendar address                      |
| `--events [--after <EVENT_ID>]`                                       | Receipts and workflow runs, newest first                         |
| `--upcoming [--limit N]`                                              | Today plus the next seven local days of calendar                 |
| `--loop-resolve <LOOP_ID>`                                            | **Writes.** Marks one loop done, prints the receipt              |

`--limit` caps every page at 100 and defaults to 25. `--meetings` also
accepts `--last`, which overrides `--limit`.

## JSON shapes

Every payload carries `schema_version:3`. `--meetings`, `--people` and
`--upcoming` page with `has_more`. `--loops` returns both `has_more` and
`next_cursor`: when the cursor is non-null, pass it to `--after` with the
same filters. `has_more` is true exactly when that cursor is present.
`--events` resumes from its string `next_cursor` with `--after <id>`.
`--query` exposes an informational cursor object but has no cursor flag;
raise `--limit` or narrow the words instead. It tops out at 100.
Links are not uniform, so read the shape rather than assuming one. `--query`,
`--meetings`, `--loops` and `--people` rows always carry a `sona://` `link`,
and `--meeting` and `--transcript` carry one on the payload itself. An
`--events` row carries `link:null` when the receipt addresses nothing.
`--upcoming` rows have **no `link` field at all** — a calendar occurrence is
not a Sona noun, so it is `undefined` rather than `null`; identify one by
`event_key`. The `--loop-resolve` receipt carries no link either.

```
--query      {"schema_version":3,"entries":[{"kind":"meeting"|"dictation"|
              "person"|"loop","id":str,"title":str,"snippet":str,
              "when_utc_ms":int,"link":str}],
              "next_cursor":{"when_utc_ms":int,"kind":str,"id":str,
              "dictation_id":int|null}|null,"reason":str|null}
             — one page mixes kinds. A dictation row's `id` is digits in a
               string ("75"); the cursor's `dictation_id` is the same number
               as an integer.
--meetings   {"schema_version":3,"entries":[{"id":uuid,"title":str,"phase":str,
              "when_utc_ms":int,"recorded_duration_ms":int|null,
              "capture_completeness":"not_started"|"complete"|"partial",
              "speakers":[str],"headline":headline,"link":str}],"has_more":bool}
--meeting    {"schema_version":3,"id":uuid,"title":str,"phase":str,
              "processing_status":processing_status,"started_at_utc_ms":int|null,
              "speakers":[str],"summary":str|null,"headline":str|null,
              "notes":[str],"loops":[loop],"link":str}
--transcript {"schema_version":3,"meeting_id":uuid,"title":str,
              "started_at_utc_ms":int|null,
              "lines":[{"speaker":str,"start_ms":int,"end_ms":int,"text":str}],"link":str}
--loops      {"schema_version":3,"entries":[loop],"next_cursor":str|null,
              "has_more":bool,"reason":str|null}
             loop: {"id":str,"meeting_id":uuid,"meeting_title":str,
              "kind":"loop"|"commitment","status":"open"|"done"|"dropped"|"carried",
              "direction":"mine"|"waiting_on"|"unattributed","text":str,
              "owner":str|null,"when_utc_ms":int,
              "resolved_at_utc_ms":int|null,"link":str}
--people     {"schema_version":3,"entries":[{"id":uuid,"display_name":str,
              "aliases":[str],"calendar_emails":[str],"meetings_count":int,
              "last_meeting_at_utc_ms":int|null,"last_meeting_title":str|null,
              "last_meeting_headline":str|null,"link":str}],"has_more":bool,
              "reason":str|null}
--events     {"schema_version":3,"entries":[{"id":str,
              "source":"workflow_run"|"operation_receipt","action":str,
              "result":str,"detail":str,"outcome_summary":str|null,
              "when_utc_ms":int,"link":str|null}],"next_cursor":str|null}
             — `detail` is always present. For a failed workflow run it is the
               error; `outcome_summary` retains that run's store-authored
               summary and is null for an operation receipt.
--upcoming   {"schema_version":3,"calendar_access_subject":"responsible_process",
              "calendar_access":"not_determined"|"authorized"|"denied"|"unavailable","window_start_utc_ms":int,
              "window_end_utc_ms":int,"entries":[{"event_key":str,"title":str,
              "start_utc_ms":int,"end_utc_ms":int,"attendees":[str],
              "attendee_count":int,"calendar_name":str|null,"join_url":str|null,
              "series_key":str|null,"always_record":bool}],"has_more":bool}
--loop-resolve
             {"schema_version":3,"receipt":{"schema_version":int,
              "operation_id":uuid,"session_id":uuid|null,
              "actor":"user"|"system"|"external","command":"loop_resolve",
              "expected_revision":int,"from_phase":str|null,"to_phase":str|null,
              "requested_at_utc_ms":int,"committed_at_utc_ms":int|null,
              "result":"committed"|"rejected"|"failed","reason_codes":[str],
              "new_revision":int|null,"effect_ids":[str]}}
             — the receipt is the store's own audit record, printed verbatim
               rather than projected, so it carries its own `schema_version`
               inside the outer one.
processing_status
             {"kind":"pending"|"running"|"succeeded"|"cancelled"}
             | {"kind":"failed","reason":"local_model_unavailable"|
                "remote_unavailable"|"engine_failure"|"cancelled"|"interrupted",
                "cause":"storage"|"transcription"|"voice_detection"|
                "evidence_pack"|"model_refused"|"reply_not_structured"|
                "reply_rejected"|"panicked"|null}
             — `cause` names which part refused and is non-null only under
               `engine_failure`.
headline     {"kind":"none"} | {"kind":"words","words":int}
             | {"kind":"ledger","text":str} | {"kind":"summary","text":str}
             — `words` counts what was said when no prose exists yet.
```

`reason` says what shaped an answer, when something other than the corpus did.
`null` means every source behind the question was read and the rows are all
there are. Each verb produces only its own values:

- `--query`: `no_searchable_tokens` (the words held nothing to match),
  `semantic_unavailable` (the recall model is not on this machine, so only
  literal word matching ran; rows worded differently than the question are
  unreachable whether or not this page came back full), `no_rows`.
- `--loops`: `awaiting_continuity` (a meeting's continuity pass has not
  succeeded, so its ledger rows are not reportable yet — ask again later),
  `filtered_out` (`--status`, `--mine` or `--waiting` excluded every row),
  `no_rows`.
- `--people`: `people_index_empty` (diarization has not named anybody yet, so
  no name can match), `no_rows`.

`no_rows` is the only value that means asking again the same way is pointless.

`calendar_access` applies only to `calendar_access_subject`. For a shell
invocation on macOS, that is the TCC responsible process (usually the terminal),
not necessarily the installed Sona GUI. An empty `entries` under anything but
`authorized` means this request cannot read the calendar; it says nothing about
the GUI's grant.

A meeting can answer with `summary:null`, `headline:null`, `notes:[]` and
`loops:[]` while its transcript is full. Those fields are processing output,
not capture. Read `processing_status`: `failed` means generation broke and
carries its reason, plus which part refused when that reason is
`engine_failure`; `pending` or `running` can still produce output; `succeeded`
means processing finished. `phase` tracks the review lifecycle, not whether an
empty artifact result was broken. Read `--transcript` and quote it rather than
calling the meeting empty.

`--loop-resolve` reads the loop's state revision, then writes against it, so
it needs both consent rows. An immediate replay returns the stored receipt
without another revision; after a real reopen, a later resolve may commit again.
`result: "rejected"` means the meeting changed in between — re-read `--loops`
and decide again rather than retrying blindly. The receipt records
`actor:"external"`: a write that arrived through this surface is nobody's own
click, and the corpus's audit trail says so.

## MCP

`tools/sona-mcp` is a stdio server over exactly those flags. Same consent, same
refusals: the error message leads with Sona's own code (`sona not_found: No
loop … in this corpus.`) and `data` carries `code` plus, for the consent one,
`settingsPath`. `consent_required` arrives as InvalidRequest; `invalid_request`
and `not_found` as InvalidParams; `unavailable`, `failed`, `timed_out` and
`not_installed` as InternalError — the group that calling again with different
arguments will not fix.

| Tool                | Arguments                                                  |
| ------------------- | ---------------------------------------------------------- |
| `sona_search`       | `query`, `scope?`, `limit?`                                |
| `sona_meetings`     | `last?` \| (`from` + `to`)                                 |
| `sona_meeting`      | `meeting_id`                                               |
| `sona_transcript`   | `meeting_id`                                               |
| `sona_action_items` | `status?`, `side?` (`mine`\|`waiting`), `after?`, `limit?` |
| `sona_people`       | `name`, `limit?`                                           |
| `sona_upcoming`     | `limit?`                                                   |
| `sona_loop_resolve` | `loop_id` — writes                                         |

`--events` is CLI-only: it pages by receipt id, which a stateless server has no
use for between calls.

```bash
claude mcp add sona -- bun /absolute/path/to/sona/tools/sona-mcp/src/index.ts
```

## `sona://` links

Every row's `link` opens the app at that row. Hand them to the user instead of
describing where to click.

| Form                                                           | Opens                                       |
| -------------------------------------------------------------- | ------------------------------------------- |
| `sona://meeting/<uuid>`                                        | the meeting                                 |
| `sona://loop/<meeting-uuid>:<kind>:<digest>`                   | the meeting review, at the loop             |
| `sona://person/<uuid>`                                         | the person's page                           |
| `sona://organization/<slug>`                                   | everybody at one organization               |
| `sona://dictation/<id>`                                        | the dictation in Library                    |
| `sona://search?q=<text>`                                       | search, with the question in it             |
| `sona://record`, `sona://record?mode=<id>`, `sona://mode/<id>` | dictation controls                          |
| `sona://meeting/start`                                         | the meeting screen — does not begin capture |

## Rules

- Quote what the corpus says and cite the `link`. Do not infer a commitment,
  date or owner that no row carries.
- Stored text is a record of what the recogniser heard, not of what was said.
  Sona never rewrites a transcript when the recogniser improves, so an old row
  can carry a word the model got wrong — a real one in this corpus reads "If
  you want Sona of the most tender…" where the speaker said "some". Quote the
  row as it stands, and when a word is plainly wrong in context, say so instead
  of silently correcting it or presenting it as certain. A faithful quote of a
  corrupted row is still a corrupted row.
- Read before you write: `--loop-resolve` is the only verb that changes
  anything, and it needs a `loop_id` that came out of a read.
- A refusal is an answer. Report the code and the `settings_path`; the user
  clears it, not you.
