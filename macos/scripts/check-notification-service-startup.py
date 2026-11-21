#!/usr/bin/env -S uv run --offline --no-project --script
import argparse
from contextlib import closing
import json
import os
from pathlib import Path
import plistlib
import resource
import select
import shutil
import socket
import subprocess
import sys
import tempfile
import time


UNAVAILABLE = b"Notification settings are unavailable."


def check(app: Path) -> None:
    assert sys.platform == "darwin", "This regression requires macOS."
    hardware = {
        key: subprocess.check_output(["/usr/sbin/sysctl", "-n", key], text=True).strip()
        for key in ["hw.model", "hw.memsize", "hw.activecpu"]
    }
    assert hardware["hw.model"].startswith("VirtualMac"), "Run only in an isolated VM."
    assert int(hardware["hw.memsize"]) <= 4 * 1024**3
    assert int(hardware["hw.activecpu"]) <= 2
    app = app.resolve(strict=True)
    info = plistlib.loads((app / "Contents/Info.plist").read_bytes())
    assert info["CFBundleIdentifier"].startswith("com.aktanazat.sona.mac.scratch-")
    core = app / "Contents/Helpers/SonaCore.app/Contents/MacOS/sona-core"
    assert core.is_file() and os.access(core, os.X_OK)
    process_count = len(
        subprocess.check_output(
            ["/bin/ps", "-u", str(os.getuid()), "-o", "pid="]
        ).splitlines()
    )

    def limits() -> None:
        resource.setrlimit(resource.RLIMIT_CPU, (60, 60))
        resource.setrlimit(resource.RLIMIT_FSIZE, (64 * 1024**2, 64 * 1024**2))
        resource.setrlimit(
            resource.RLIMIT_NPROC, (process_count + 32, process_count + 32)
        )
        resource.setrlimit(resource.RLIMIT_NOFILE, (1024, 1024))

    with tempfile.TemporaryDirectory(
        prefix="sona-n-", dir="/private/var/tmp"
    ) as temporary:
        root = Path(temporary)
        assert shutil.disk_usage(root).total <= 128 * 1024**3
        for name in ["home", "tmp", "runtime", "config", "cache", "state"]:
            (root / name).mkdir(mode=0o700)

        def quote(path: Path) -> str:
            return json.dumps(str(path), ensure_ascii=False)

        ancestors = set(app.parents) | {
            Path("/Library"),
            Path("/Library/Frameworks"),
            Path("/etc"),
            Path("/var"),
        }
        metadata = "\n".join(f"(literal {quote(path)})" for path in sorted(ancestors))
        profile = root / "core.sb"
        profile.write_text(
            "(version 1)\n(deny default)\n"
            "(allow process-exec process-fork sysctl-read)\n"
            f"(allow file-read-metadata {metadata})\n"
            f'(allow file-read* (literal "/") (subpath {quote(app)}) (subpath {quote(root)})\n'
            ' (subpath "/System") (subpath "/usr") (subpath "/bin")\n'
            ' (subpath "/sbin") (subpath "/private/etc") (subpath "/dev"))\n'
            f'(allow file-write* (subpath {quote(root)}) (literal "/dev/null"))\n'
            f"(allow network-bind network-inbound network-outbound (subpath {quote(root)}))\n"
            "(allow mach-lookup\n"
            ' (global-name "com.apple.windowserver.active")\n'
            ' (global-name "com.apple.cfprefsd.agent")\n'
            ' (global-name "com.apple.cfprefsd.daemon")\n'
            ' (global-name "com.apple.coreservices.launchservicesd")\n'
            ' (global-name "com.apple.CoreServices.coreservicesd")\n'
            ' (global-name "com.apple.system.notification_center")\n'
            ' (global-name "com.apple.system.opendirectoryd.libinfo")\n'
            ' (global-name "com.apple.tccd")\n'
            ' (global-name "com.apple.tccd.system")\n'
            ' (global-name "com.apple.logd"))\n'
        )
        environment = {
            "HOME": str(root / "home"),
            "CFFIXED_USER_HOME": str(root / "home"),
            "TMPDIR": str(root / "tmp") + "/",
            "XDG_RUNTIME_DIR": str(root / "runtime"),
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_CACHE_HOME": str(root / "cache"),
            "XDG_STATE_HOME": str(root / "state"),
            "PATH": "/usr/bin:/bin",
            "LC_ALL": "C",
            "TZ": "UTC",
        }
        endpoint = root / "core.sock"
        stderr = bytearray()
        exited = False
        socket_ready = False
        with closing(select.kqueue()) as queue:
            directory = os.open(root, os.O_EVTONLY)
            queue.control(
                [
                    select.kevent(
                        directory,
                        filter=select.KQ_FILTER_VNODE,
                        flags=select.KQ_EV_ADD | select.KQ_EV_CLEAR,
                        fflags=select.KQ_NOTE_WRITE,
                    )
                ],
                0,
                0,
            )
            process = subprocess.Popen(
                [
                    "/usr/bin/sandbox-exec",
                    "-f",
                    str(profile),
                    str(core),
                    "--native-socket",
                    str(endpoint),
                    "--no-tray",
                    "--start-hidden",
                ],
                cwd=app,
                env=environment,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
                start_new_session=True,
                preexec_fn=limits,
            )
            assert process.stderr is not None
            os.set_blocking(process.stderr.fileno(), False)
            queue.control(
                [
                    select.kevent(
                        process.pid,
                        filter=select.KQ_FILTER_PROC,
                        flags=select.KQ_EV_ADD | select.KQ_EV_ONESHOT,
                        fflags=select.KQ_NOTE_EXIT,
                    ),
                    select.kevent(
                        process.stderr.fileno(),
                        filter=select.KQ_FILTER_READ,
                        flags=select.KQ_EV_ADD | select.KQ_EV_CLEAR,
                    ),
                ],
                0,
                0,
            )
            deadline = time.monotonic() + 30
            try:
                while not (socket_ready and UNAVAILABLE in stderr):
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        raise TimeoutError(
                            "The real unavailable-settings callback did not complete."
                        )
                    events = queue.control(None, 8, remaining)
                    for event in events:
                        if event.flags & select.KQ_EV_ERROR:
                            raise OSError(event.data, "Native startup event failed")
                        if event.filter == select.KQ_FILTER_PROC:
                            exited = True
                            raise RuntimeError(
                                "The native core exited before the callback completed."
                            )
                        if event.filter == select.KQ_FILTER_VNODE:
                            socket_ready = endpoint.is_socket()
                        if event.filter == select.KQ_FILTER_READ:
                            while True:
                                try:
                                    chunk = os.read(process.stderr.fileno(), 65536)
                                except BlockingIOError:
                                    break
                                if not chunk:
                                    break
                                stderr.extend(chunk)
                                if len(stderr) > 1024**2:
                                    raise RuntimeError(
                                        "Native startup exceeded the bounded diagnostic buffer."
                                    )
                with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
                    connection.settimeout(max(0.001, deadline - time.monotonic()))
                    connection.connect(str(endpoint))
                    request = {
                        "id": 1,
                        "method": "get_agent_bridge_status",
                        "params": {},
                    }
                    connection.sendall(json.dumps(request).encode() + b"\n")
                    with connection.makefile("rb") as frames:
                        while True:
                            line = frames.readline(1024**2 + 1)
                            if not line or len(line) > 1024**2:
                                raise RuntimeError(
                                    "The native core did not return a bounded response."
                                )
                            frame = json.loads(line)
                            if frame.get("id") != 1:
                                continue
                            status = frame.get("result")
                            assert isinstance(status, dict), (
                                "The agent console status failed after the callback."
                            )
                            assert isinstance(status.get("running"), bool)
                            assert isinstance(status.get("diagnostic"), str)
                            break
                print(
                    "The real unavailable notification callback completed; the agent console still responds."
                )
            except BaseException:
                print(stderr.decode(errors="replace"), file=sys.stderr)
                raise
            finally:
                if not exited:
                    try:
                        process.terminate()
                    except ProcessLookupError:
                        pass
                    events = queue.control(None, 8, 5)
                    exited = any(
                        event.filter == select.KQ_FILTER_PROC
                        and event.fflags & select.KQ_NOTE_EXIT
                        and not event.flags & select.KQ_EV_ERROR
                        for event in events
                    )
                    if not exited:
                        process.kill()
                process.wait()
                process.stderr.close()
                os.close(directory)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(
        description="Prove native startup survives an unavailable notification service."
    )
    parser.add_argument("app", type=Path)
    check(parser.parse_args().app)
