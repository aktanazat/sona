#!/usr/bin/env -S uv run --offline --no-project --script
import argparse
import json
import os
from pathlib import Path
import resource
import socket
import subprocess
import sys


def check(app: Path, endpoint: Path) -> None:
    assert sys.platform == "darwin", "This regression requires macOS."
    hardware = {
        key: subprocess.check_output(["/usr/sbin/sysctl", "-n", key], text=True).strip()
        for key in ["hw.model", "hw.memsize", "hw.ncpu"]
    }
    assert hardware["hw.model"].startswith("VirtualMac"), "Use the isolated test Mac."
    assert int(hardware["hw.memsize"]) <= 4 * 1024**3
    assert int(hardware["hw.ncpu"]) <= 2
    app = app.resolve()
    endpoint = endpoint.resolve()
    scratch = Path("/private/var/tmp")
    assert scratch in app.parents and app.suffix == ".app"
    assert scratch in endpoint.parents and endpoint.is_socket()
    assert endpoint.stat().st_uid == os.getuid()
    resource.setrlimit(resource.RLIMIT_CPU, (5, 5))
    resource.setrlimit(resource.RLIMIT_FSIZE, (1024**2, 1024**2))
    resource.setrlimit(resource.RLIMIT_NOFILE, (64, 64))
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
        connection.settimeout(10)
        connection.connect(str(endpoint))
        connection.sendall(
            json.dumps(
                {
                    "id": 1,
                    "method": "get_agent_bridge_hook_snippet",
                    "params": {},
                }
            ).encode()
            + b"\n"
        )
        with connection.makefile("rb") as frames:
            while True:
                line = frames.readline(1024**2 + 1)
                if not line or len(line) > 1024**2:
                    raise RuntimeError(
                        "The native app did not return a bounded setup response."
                    )
                response = json.loads(line)
                if response.get("id") == 1:
                    break
    if "error" in response:
        raise RuntimeError("Native agent setup failed: " + str(response["error"]))
    config = json.loads(response["result"])
    assert set(config) == {"claude", "codex", "grok", "omp", "opencode"}
    helper = app / "Contents/Helpers/SonaCore.app/Contents"
    package = helper / "Resources/resources/agent-bridge/opencode"
    assert config["opencode"] == {"plugins": [{"package": str(package)}]}
    assert package.is_dir()
    assert Path(config["claude"]["command"]).samefile(helper / "MacOS/sona-agent-hook")
    assert Path(config["omp"]["args"][1]).samefile(
        helper / "Resources/resources/agent-bridge/sona-omp-agent-bridge.mjs"
    )
    print("The running native app exposes real bundled setup for all five agents.")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--app", type=Path, required=True)
    parser.add_argument("--socket", type=Path, required=True)
    arguments = parser.parse_args()
    check(arguments.app, arguments.socket)
