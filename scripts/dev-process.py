#!/usr/bin/env python3
"""Own the dev app outside cargo-watch's replaceable rebuild process group.

start-app.sh runs `watch`; dev-run.sh requests `running`, `stop` and `start`.
Only this owner signals the child it created, never a stale PID from disk.
This is Bilibi launcher plumbing, with no Bilimbi module counterpart.
"""
import fcntl
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import importlib.util
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import threading


spec = importlib.util.spec_from_file_location('dev_status', Path(__file__).with_name('dev-status.py'))
status = importlib.util.module_from_spec(spec)
spec.loader.exec_module(status)


class StatusHandler(BaseHTTPRequestHandler):
    def do_GET(self):
        # No CORS, arbitrary file serving or environment/runtime log endpoint.
        # Reject DNS rebinding; Caddy sets this loopback Host on its proxy hop.
        if self.headers.get('Host') != f'127.0.0.1:{self.server.server_port}':
            self.send_error(403)
            return
        if self.path != '/__bilibi_dev/status':
            self.send_error(404)
            return
        body = json.dumps(status.read()).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Cache-Control', 'no-store')
        self.send_header('X-Content-Type-Options', 'nosniff')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        try:
            self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError):
            pass

    def log_message(self, _format, *_args):
        pass


DEV = Path(".local/dev")
SOCKET = DEV / "control.sock"
PID = DEV / "app.pid"
# start-app.sh prepares these after the owner starts. Transfer only these values
# over the private control socket; never persist credentials or log the payload.
APP_ENVIRONMENT = {'DATABASE_URL', 'APP_ORIGIN', 'BIND_ADDR'}


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
        status.write('prepare')
        (DEV / 'seed-needed').touch()
        Path('.local/dev-run.state').unlink(missing_ok=True)
        http = ThreadingHTTPServer(('127.0.0.1', 0), StatusHandler)
        http.daemon_threads = True
        threading.Thread(target=http.serve_forever, daemon=True).start()
        environment = {**os.environ, 'BILIBI_DEV_STATUS_PORT': str(http.server_port)}
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
                watcher = subprocess.Popen(command, start_new_session=True, env=environment)
                while not stopping and watcher.poll() is None:
                    if app is not None and app.poll() is not None:
                        result = app.returncode
                        state = Path(".local/dev-run.state")
                        if state.exists() and state.read_text().strip() == "serve":
                            state.write_text("ended\n")
                            status.write("serve", failed=True, finished=True)
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
                            app_environment = json.loads(stream.readline(16384)) if action == b'start' else {}
                            if (not isinstance(app_environment, dict)
                                    or app_environment.keys() - APP_ENVIRONMENT
                                    or any(not isinstance(value, str) for value in app_environment.values())):
                                raise ValueError('Invalid app environment')
                    except (TimeoutError, ConnectionResetError, ValueError):
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
                                env={**os.environ, **app_environment},
                            )
                            PID.write_text(f"{app.pid}\n")
                            reply(connection, 0)
                        except OSError as error:
                            stop(app)
                            app = None
                            PID.unlink(missing_ok=True)
                            status.write('start', failed=True, finished=True)
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
                status.write('stopped')
                http.shutdown()
                http.server_close()
        return 130 if stopping else watcher.returncode


def request(action):
    with socket.socket(socket.AF_UNIX) as connection:
        connection.connect(str(SOCKET))
        payload = action.encode() + b"\n"
        if action == 'start':
            payload += json.dumps({key: os.environ[key] for key in APP_ENVIRONMENT if key in os.environ}).encode() + b'\n'
        connection.sendall(payload)
        return int(connection.recv(32))


if __name__ == "__main__":
    if sys.argv[1] == "watch":
        sys.exit(watch(sys.argv[2:]))
    sys.exit(request(sys.argv[1]))
