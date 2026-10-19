#!/usr/bin/env python3
"""Own the dev app outside cargo-watch's replaceable rebuild process group.

start-app.sh runs `watch`; dev-run.sh requests `running`, `stop` and `start`.
Only this owner signals the child it created, never a stale PID from disk.
This is Bilibi launcher plumbing, with no Bilimbi module counterpart.
"""
import fcntl
from pathlib import Path
import signal
import socket
import subprocess
import sys


DEV = Path(".local/dev")
SOCKET = DEV / "control.sock"
PID = DEV / "app.pid"


def reply(connection, result):
    try:
        connection.sendall(str(result).encode())
    except (BrokenPipeError, ConnectionResetError):
        pass  # cargo-watch replaced the rebuild that made this request.
    finally:
        connection.close()


def stop(process):
    if process is not None and process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()


def watch(command):
    DEV.mkdir(parents=True, exist_ok=True)
    # A second launcher must not unlink the active owner's socket or PID record.
    with (DEV / "owner.lock").open("w") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            sys.exit("This checkout already has a dev launcher. Stop it before starting another.")
        stopping = False

        def shutdown(_signum, _frame):
            nonlocal stopping
            stopping = True

        signal.signal(signal.SIGINT, shutdown)
        signal.signal(signal.SIGTERM, shutdown)
        app = watcher = None
        SOCKET.unlink(missing_ok=True)
        PID.unlink(missing_ok=True)
        with socket.socket(socket.AF_UNIX) as server:
            server.bind(str(SOCKET))
            SOCKET.chmod(0o600)
            server.listen()
            server.settimeout(0.2)
            try:
                # Neither child shares the terminal/rebuild process group. The
                # launcher forwards Ctrl-C to cargo-watch and reaps its own app.
                watcher = subprocess.Popen(command, start_new_session=True)
                while not stopping and watcher.poll() is None:
                    if app is not None and app.poll() is not None:
                        result = app.returncode
                        state = Path(".local/dev-run.state")
                        if state.read_text().strip() == "serve":
                            state.write_text("ended\n")
                        if result == -signal.SIGTERM:
                            reason = (
                                "another process stopped it (SIGTERM), not a file change. "
                                "To start it again, save any file or run `touch Cargo.toml`, "
                                "or press Ctrl-C and run ./scripts/start-app.sh again."
                            )
                        else:
                            code = 128 - result if result < 0 else result
                            reason = (
                                f"the app stopped (exit {code}, see above). "
                                "Fix the error and save a file to try again, or press Ctrl-C "
                                "and run ./scripts/start-app.sh again."
                            )
                        print(f"Bilibi is not running: {reason}", flush=True)
                        app = None
                        PID.unlink(missing_ok=True)
                    try:
                        connection, _ = server.accept()
                    except socket.timeout:
                        continue
                    connection.settimeout(1)
                    try:
                        with connection.makefile("rb") as stream:
                            action = stream.readline(32).strip()
                    except (TimeoutError, ConnectionResetError):
                        connection.close()
                        continue
                    if action == b"running":
                        reply(connection, 0 if app is not None and app.poll() is None else 1)
                    elif action == b"stop":
                        stop(app)
                        app = None
                        PID.unlink(missing_ok=True)
                        reply(connection, 0)
                    elif action == b"start" and app is None:
                        try:
                            app = subprocess.Popen(
                                [str(DEV.resolve() / "bilibi-dev"), "run"],
                                start_new_session=True, stdin=subprocess.DEVNULL,
                            )
                            PID.write_text(f"{app.pid}\n")
                            reply(connection, 0)
                        except OSError as error:
                            stop(app)
                            app = None
                            PID.unlink(missing_ok=True)
                            print(f"Could not start Bilibi: {error}", file=sys.stderr)
                            reply(connection, 1)
                    else:
                        reply(connection, 1)
            finally:
                if watcher is not None and watcher.poll() is None:
                    watcher.send_signal(signal.SIGINT)
                    try:
                        watcher.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        stop(watcher)
                stop(app)
                PID.unlink(missing_ok=True)
                SOCKET.unlink(missing_ok=True)
        return 130 if stopping else watcher.returncode


def request(action):
    with socket.socket(socket.AF_UNIX) as connection:
        connection.connect(str(SOCKET))
        connection.sendall(action.encode() + b"\n")
        return int(connection.recv(32))


if __name__ == "__main__":
    if sys.argv[1] == "watch":
        sys.exit(watch(sys.argv[2:]))
    sys.exit(request(sys.argv[1]))
