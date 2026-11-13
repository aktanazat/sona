# Attachments: core, CLI and bridge contract

Files, images and links on a task. Core, `sona-core work` and the native
bridge are in place; the Swift UI (drag and drop, paste, file picker,
thumbnails, Quick Look) and the MCP tools build on this file.

## Storage

- `planning_task_attachments`: `id, task_id, kind (file|image|link), title,
  subtitle, url, stored_name, original_name, mime, size, sha256,
  created_at_ms, created_by_kind, created_by_name`. A link has `url` and no
  stored file; a file or image has `stored_name`, `sha256`, `size`, `mime`
  and no `url`.
- Files are copied, never referenced: `<app data>/task-attachments/<task
  id>/<attachment id>-<safe name>` (app data is
  `~/Library/Application Support/com.aktanazat.sona`). The copy is staged
  under `task-attachments/.staging/` while it is hashed and moved in only
  when the row commits.
- One per task: unique `(task_id, sha256)` for files and `(task_id, url)` for
  links. Attaching the same bytes or link again answers the attachment
  already there, under its first title.
- `kind` is `image` when the file's extension maps to an `image/*` type
  (png, jpg, jpeg, gif, heic, heif, webp, tif, tiff, bmp, svg), else `file`.
- Archiving a task keeps its attachments. A hard delete of the task row drops
  its attachment rows (trigger `planning_task_attachments_delete`).
- Detach deletes the row, then the stored file, then the task's folder once
  it is empty.
- Activity kind gains `attachment`: `to` is the title when attached, `from`
  is the title when removed; an agent's name is kept like other agent rows.
- Migration `ATTACHMENTS_MIGRATION`, after `THREADS_MIGRATION`. It rebuilds
  `planning_task_activity` to allow the new kind.
- `QUERY_SCHEMA_VERSION` stays 6: fields were added, none renamed or removed.

## Rules

- A file must be a regular file. The path the caller gives is followed (a
  symlink the user hands over is read as its target); a folder is refused
  with "Attach a file, not a folder." and a missing path with "No file is
  there to attach."
- At most 50 MB, checked before and while copying: "Attachments can be at
  most 50 MB."
- The stored name keeps only the last path part, replaces anything but
  letters, digits, `.`, `-`, `_` and space with `_`, drops leading dots and
  is capped at 120 characters, so `../../evil.sh` lands as
  `<id>-evil.sh` inside the task's folder.
- A link must parse as a URL with scheme `http`, `https`, `mailto` or `sona`:
  "A link must start with http://, https://, mailto: or sona://." It is
  stored normalised (`https://Example.com` becomes `https://example.com/`).
- Title defaults to the file's original name or the link; subtitle is
  optional. Blank strings count as absent.
- Anyone may attach. The user may detach or rename any attachment; Sona and
  agents only their own: "Only whoever attached it can change it." Attach and
  detach are not gated by the agent's Apply/Suggest setting, like comments.

## Bridge methods (params camelCase; reply is the JSON shown)

| method | params | reply |
|---|---|---|
| `work_attachments` | `itemId` | `[WorkAttachment]`, oldest first |
| `work_attachment_attach_file` | `operationId`, `itemId`, `path?` or (`data` base64 + `fileName`), `title?`, `subtitle?` | `WorkAttachment` |
| `work_attachment_attach_link` | `operationId`, `itemId`, `url`, `title?`, `subtitle?` | `WorkAttachment` |
| `work_attachment_detach` | `operationId`, `id` | `WorkAttachment` (as it was) |
| `work_attachment_rename` | `operationId`, `id`, `title` | `WorkAttachment` |

- `attach_file` needs exactly one of `path` (a file picker or a drop) or
  `data` + `fileName` (a pasted image: send the PNG/TIFF bytes and a name
  such as `Pasted image.png`; the extension decides `kind` and `mime`).
  Both or neither: "Attach a file by its path, or pasted data with its
  name." Data that is not base64: "The pasted file could not be read."
- A repeated `operationId` with the same request returns the first answer;
  with a different request it fails with "This change was already used."
  Make a fresh `operationId` per user action.
- Every write tells open windows the task changed, like other task writes.

### Shapes

```
WorkAttachmentKind = "file" | "image" | "link"
WorkCommentAuthor  { kind: "me" | "sona" | "agent", name: string | null }
WorkAttachment     { id, itemId, kind: WorkAttachmentKind, title,
                     subtitle: string|null, url: string|null,
                     path: string|null,          // absolute path of Sona's copy
                     originalName: string|null, mime: string|null,
                     size: u64|null, sha256: string|null,
                     createdAtMs, createdBy: WorkCommentAuthor }
```

- `WorkItemRow` (in `work_list`) gains `attachmentCount: u32`.
- `WorkItemDetail` (`work_item`) gains `attachments: [WorkAttachment]`,
  oldest first.

## Swift work this needs (not done here)

- `macos/Sona/WorkTypes.swift`: `WorkActivityKind` needs `attachment`. Until
  then a task whose history holds an attachment row fails to decode in the
  app. `WorkItemRow` and `WorkItemDetail` gain the fields above.
- Thumbnails and previews come from Quick Look on the Swift side
  (`QLThumbnailGenerator` on `path`, `QLPreviewPanel` to open); Rust makes
  none and returns no thumbnail path.
- Drag and drop and the file picker send `path`; paste sends `data` +
  `fileName`; a dropped or pasted URL goes to `work_attachment_attach_link`.

## CLI (`sona-core work ...`)

Every write verb takes `--json`, `--agent NAME`, `--operation-id ID` and
`--dry-run`; a dry run and `attachments` need only the read scope.

- `attach <TASK> <PATH|URL> [--title T] [--subtitle S]`: a value containing
  `://` or starting `mailto:` is a link, anything else a path, resolved
  against the current folder. A dry run checks the file or link and writes
  nothing.
- `attachments <TASK>`
- `detach <TASK> <ID>`: the id comes from `attachments`; an id on another
  task is "No attachment ID on S-N."
- `list` rows gain `attachment_count`; text shows `· 3 attachments`.
- `show` gains `attachment_count` on the task and `attachments`; text shows an
  "Attachments" section.

CLI JSON for an attachment: `{id, task_id, kind, title, subtitle, url, path,
original_name, mime, size, sha256, created_at_ms, created_by: {kind, name}}`.
A write answers `{schema_version, action: attached|detached, dry_run,
operation_id, task, attachment, would_attach, replayed}`; `would_attach` is
the file name or link on a dry run of attach, where `attachment` is null.
`replayed` is true when the same `--operation-id` ran before or the task
already held the same file or link.

## MCP verbs to add

`attach`, `attachments`, `detach` map one to one onto the CLI verbs above,
with the same JSON.

## Verified

- `cargo check --lib --tests` (target aarch64-apple-darwin): clean, no new
  warnings.
- `cargo test --lib -- attach`: 6 passed, 3 of them new in
  `planning::work::attachments` (`a_pasted_name_cannot_leave_the_tasks_folder`,
  `the_same_file_twice_is_one_attachment`,
  `only_web_mail_and_sona_links_attach`). With the name cleaning (both the
  last-part cut and `safe_name`), the same-bytes lookup and the link scheme
  list each broken on purpose, the matching test failed; restored
  afterwards.
- Scratch smoke, 2026-11-10: a debug `sona` built from this tree, run as
  `HOME=/private/var/tmp/sona-scratch-attach SONA_SCRATCH_RUN=1 sona work ...`
  against a fresh tracker, from a folder holding `notes.txt` and
  `screen shot.png`. Long JSON trimmed (`…`).

```
$ work attach S-1 notes.txt --dry-run
Would attach to S-1 Launch the harbor site
  notes.txt
$ work attach S-1 notes.txt --subtitle "From the kickoff" --json
{"action":"attached","dry_run":false,"operation_id":"ffad809b-…","attachment":{"id":"cadd4c46-…",
  "kind":"file","title":"notes.txt","subtitle":"From the kickoff",
  "path":"…/com.aktanazat.sona/task-attachments/428eab5d-…/cadd4c46-…-notes.txt",
  "original_name":"notes.txt","mime":"text/plain","size":46,"sha256":"01c2827f…",
  "created_by":{"kind":"agent","name":"Agent"}},"would_attach":null,"replayed":false}
$ work attach S-1 notes.txt
Already attached to S-1 Launch the harbor site
  notes.txt  (file, 46 B)  Agent  2026-11-10 15:58 -08:00  id cadd4c46-…
$ work attach S-1 "screen shot.png" --title "Venue map" --agent claude
Attached to S-1 Launch the harbor site
  Venue map  (image, 67 B)  Claude Code  …  id f77fab27-…
$ work attach S-1 "https://example.com/venue?id=7" --title "Venue booking page" --json
{"action":"attached","attachment":{"id":"4004cd5a-…","kind":"link","title":"Venue booking page",
  "url":"https://example.com/venue?id=7","path":null, …},"replayed":false}
$ work attach S-1 ftp://example.com/x
invalid_request: A link must start with http://, https://, mailto: or sona://.
$ work attach S-1 "a folder"
invalid_request: Attach a file, not a folder.
$ work attach S-1 missing.pdf
invalid_request: No file is there to attach.
$ work list
S-1    Todo         -       Launch the harbor site  · 3 attachments
$ work show S-1 --json
{"task":{… "attachment_count":3}, … "activity":[… {"kind":"attachment","to":"notes.txt","agent":"Agent"}, …],
 "attachments":[{… notes.txt …},{… Venue map …},{… Venue booking page …}]}

$ work detach S-1 cadd4c46-… --dry-run
Would remove from S-1 Launch the harbor site
  notes.txt  (file, 46 B)  Agent  …
$ ls <task folder>
cadd4c46-…-notes.txt   f77fab27-…-screen shot.png
$ work detach S-1 f77fab27-…                 # attached by Claude Code, run as Agent
invalid_request: Only whoever attached it can change it.
$ work detach S-1 cadd4c46-… --operation-id 33333333-… --json
{"action":"detached","dry_run":false,"operation_id":"33333333-…","attachment":{"id":"cadd4c46-…", …},"replayed":false}
$ (same command again)
{"action":"detached","operation_id":"33333333-…","attachment":{"id":"cadd4c46-…", …},"replayed":true}
$ ls <task folder>
f77fab27-…-screen shot.png
$ work detach S-1 cadd4c46-… --operation-id 55555555-…
not_found: No attachment cadd4c46-… on S-1.
$ work attach S-1 mailto:dana@example.com --title "Email Dana" --operation-id 44444444-… --json
{"action":"attached","attachment":{"id":"37bc74e6-…","kind":"link","url":"mailto:dana@example.com", …},"replayed":false}
$ (same command again)
{"action":"attached","attachment":{"id":"37bc74e6-…", …},"replayed":true}
$ work attach S-1 mailto:dana@example.com --title Changed --operation-id 44444444-…
invalid_request: Operation id 44444444-… was already used for a different change. …
$ work detach S-1 f77fab27-… --agent claude
Removed from S-1 Launch the harbor site
$ ls task-attachments                        # the task's folder went once empty
.staging
$ work show S-1
S-1    Todo         -       Launch the harbor site  · 2 attachments
Attachments
  Venue booking page  (link)  Agent  …
  Email Dana  (link)  Agent  …
History
  … Agent  attachment: notes.txt
  … Claude Code  attachment: Venue map
  … Agent  attachment: Venue booking page
  … Agent  attachment: removed notes.txt
  … Agent  attachment: Email Dana
  … Claude Code  attachment: removed Venue map
```

Not verified: the bridge methods (including base64 paste and rename) were
compiled but not called from the app; no CLI verb renames. A hard delete of
a task drops its rows but leaves `task-attachments/<task id>/` on disk; no
flow hard-deletes tasks today. The 50 MB cap was not run against a real
50 MB file.
