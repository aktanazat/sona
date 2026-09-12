#!/usr/bin/env python3
"""Generate the native shell's command dispatch from the Tauri command list.

Reads the `collect_commands!` and `collect_events!` lists in
`src-tauri/src/lib.rs`, finds each command's signature in its source file,
and writes `src-tauri/src/native_bridge/dispatch.rs`: one match arm per
command that reads the JSON arguments under the names the webview bindings
used (lower camel case), injects `AppHandle` and `State` the way Tauri's
command macro does, and runs the function where Tauri would have run it —
sync commands on the main thread, async ones on the runtime, and
`#[tauri::command(async)]` sync functions on a blocking thread.

Run from the repository root:

    python3 scripts/gen_native_dispatch.py

Rerun after adding, removing, or re-signing a command; the Rust build reads
the generated file, so a stale one fails to compile rather than misroutes.
"""

from __future__ import annotations

import re
import sys
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT / "src-tauri" / "src"
OUT = SRC / "native_bridge" / "dispatch.rs"

INJECTED_APP = {"AppHandle", "tauri::AppHandle"}


@dataclass
class Param:
    name: str
    ty: str

    @property
    def injection(self) -> str | None:
        if self.ty in INJECTED_APP:
            return "app"
        if self.ty.startswith(("State<", "tauri::State<")):
            return "state"
        return None

    @property
    def wire_name(self) -> str:
        return lower_camel(self.name.removeprefix("r#"))


@dataclass
class Command:
    path: str
    file: Path
    is_async: bool
    forced_async: bool
    params: list[Param]
    ret: str

    @property
    def name(self) -> str:
        return self.path.rsplit("::", 1)[-1]

    @property
    def returns_result(self) -> bool:
        return self.ret.startswith("Result<")


def lower_camel(name: str) -> str:
    head, *rest = name.split("_")
    return head + "".join(part[:1].upper() + part[1:] for part in rest)


def macro_items(source: str, macro: str) -> list[str]:
    match = re.search(macro + r"!\[(.*?)\]", source, re.S)
    if match is None:
        sys.exit(f"{macro}! not found in lib.rs")
    body = re.sub(r"//.*", "", match.group(1))
    return re.findall(r"([A-Za-z_][A-Za-z0-9_:]*)\s*,", body)


def module_file(path: str) -> Path:
    parts = path.split("::")[:-1]
    if not parts:
        return SRC / "lib.rs"
    flat = SRC.joinpath(*parts[:-1]) / f"{parts[-1]}.rs"
    if flat.exists():
        return flat
    nested = SRC.joinpath(*parts) / "mod.rs"
    if nested.exists():
        return nested
    sys.exit(f"no source file for module {'::'.join(parts)}")


def split_top_level(text: str) -> list[str]:
    """Split on commas outside angle brackets and parentheses."""
    parts, depth, start = [], 0, 0
    for index, char in enumerate(text):
        if char in "<([":
            depth += 1
        elif char in ">)]":
            depth -= 1
        elif char == "," and depth == 0:
            parts.append(text[start:index])
            start = index + 1
    parts.append(text[start:])
    return [part.strip() for part in parts if part.strip()]


def find_signature(source: str, name: str) -> tuple[bool, bool, list[Param], str]:
    pattern = re.compile(
        r"(?P<attrs>(?:#\[[^\n]*\]\s*)+)(?:pub(?:\([a-z]+\))?\s+)?(?P<asyncness>async\s+)?fn\s+"
        + re.escape(name)
        + r"\s*(?:<[^>]*>)?\s*\("
    )
    match = pattern.search(source)
    if match is None or "tauri::command" not in match.group("attrs"):
        sys.exit(f"command {name} not found with #[tauri::command]")
    depth, index = 1, match.end()
    while depth:
        char = source[index]
        depth += (char == "(") - (char == ")")
        index += 1
    raw_params = source[match.end() : index - 1]
    rest = source[index:]
    ret = "()"
    arrow = re.match(r"\s*->\s*(.*?)\s*(?:\{|where\b)", rest, re.S)
    if arrow:
        ret = re.sub(r"\s+", " ", arrow.group(1)).strip()
    params = []
    for piece in split_top_level(raw_params):
        pname, _, pty = piece.partition(":")
        pname = pname.strip().removeprefix("mut ").strip()
        params.append(Param(pname, re.sub(r"\s+", " ", pty.strip())))
    forced = "tauri::command(async)" in match.group("attrs")
    return bool(match.group("asyncness")), forced, params, ret


def load_commands() -> list[Command]:
    lib = (SRC / "lib.rs").read_text()
    commands: dict[str, Command] = {}
    for path in macro_items(lib, "collect_commands"):
        name = path.rsplit("::", 1)[-1]
        if name in commands:
            # `is_laptop` is listed once per platform cfg; one arm serves both.
            continue
        file = module_file(path)
        is_async, forced, params, ret = find_signature(file.read_text(), name)
        commands[name] = Command(path, file, is_async, forced, params, ret)
    return list(commands.values())


def load_events() -> list[str]:
    lib = (SRC / "lib.rs").read_text()
    return macro_items(lib, "collect_events")


def call_expression(command: Command, app: str) -> str:
    arguments = []
    for param in command.params:
        injection = param.injection
        if injection == "app":
            arguments.append(f"{app}.clone()")
        elif injection == "state":
            arguments.append(f"{app}.state()")
        else:
            arguments.append(param.name.removeprefix("r#"))
    joined = ", ".join(arguments)
    return f"crate::{command.path}({joined})"


def arm(command: Command) -> str:
    lines = [f'        "{command.name}" => {{']
    wire_params = [param for param in command.params if param.injection is None]
    if wire_params:
        lines.append("            let mut args = Args::parse(params)?;")
        for param in wire_params:
            local = param.name.removeprefix("r#")
            lines.append(f'            let {local} = args.take("{param.wire_name}")?;')
    finish = "reply" if command.returns_result else "encode_plain"
    if command.is_async:
        call = call_expression(command, "app")
        lines.append(f"            {finish}({call}.await)")
    else:
        call = call_expression(command, "handle")
        if any(param.injection for param in command.params):
            lines.append("            let handle = app.clone();")
        if command.forced_async:
            lines.append(f"            off_main_thread(move || {finish}({call})).await?")
        else:
            lines.append(f"            on_main_thread(app, move || {finish}({call})).await?")
    lines.append("        }")
    return "\n".join(lines)


def render(commands: list[Command], events: list[str]) -> str:
    arms = "\n".join(arm(command) for command in commands)
    event_lines = "\n".join(f"    crate::{event}::NAME," for event in events)
    return f"""//! Generated by `scripts/gen_native_dispatch.py`; do not edit.
//!
//! One arm per command in `collect_commands!`, reading arguments under the
//! names the webview bindings used and running each function where Tauri
//! ran it: sync commands on the main thread, async ones on the runtime.

use serde_json::value::RawValue;
use tauri::{{AppHandle, Manager}};
use tauri_specta::Event as _;

use super::{{encode_plain, off_main_thread, on_main_thread, reply, Args, Fault}};

/// Every typed event the core emits, by the name the bindings export.
pub(super) const TYPED_EVENTS: [&str; {len(events)}] = [
{event_lines}
];

pub(super) async fn call(
    app: &AppHandle,
    method: &str,
    params: Option<&RawValue>,
) -> Result<String, Fault> {{
    match method {{
{arms}
        other => Err(Fault::unknown_method(other)),
    }}
}}
"""


def main() -> None:
    commands = load_commands()
    events = load_events()
    OUT.parent.mkdir(exist_ok=True)
    OUT.write_text(render(commands, events))
    sync = sum(not command.is_async for command in commands)
    print(f"{OUT.relative_to(ROOT)}: {len(commands)} commands ({sync} sync), {len(events)} events")


if __name__ == "__main__":
    main()
