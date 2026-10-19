#!/usr/bin/env python3
"""Local launcher progress and bounded build diagnostics; no Bilimbi counterpart.

Only explicit build steps pass through run(). App output and environment dumps
never enter the browser log. The original bytes still go to their terminal fd.
"""
import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import selectors
import subprocess
import sys
import termios
import time


STATUS = Path('.local/dev-run.status.json')
LIMIT = 32 * 1024
PHASES = {'prepare', 'discovery', 'css', 'build', 'migrate', 'seed', 'start', 'serve', 'stopped'}
CAPTURE = {'discovery', 'css', 'build', 'migrate'}


def redact(raw):
    text = raw.decode('utf-8', errors='replace')
    # Strip terminal controls for a plain text browser log (never render HTML).
    text = re.sub(r'\x1b\][^\x07]*?(?:\x07|\x1b\\)', '', text)
    text = re.sub(r'\x1b\[[0-?]*[ -/]*[@-~]', '', text)
    text = re.sub(r'[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]', '', text).replace('\r\n', '\n').replace('\r', '\n')
    for key, value in os.environ.items():
        if value and (key == 'DATABASE_URL' or re.search(r'PASSWORD|SECRET|TOKEN|CREDENTIAL|AUTHORIZATION|COOKIE|SESSION|(?:^|_)KEY(?:_ID)?$', key)):
            text = text.replace(value, '[redacted]')
    text = re.sub(r'(\b[a-z][a-z0-9+.-]*://)[^\s/@]+:[^\s/@]+@', r'\1[redacted]@', text, flags=re.I)
    text = re.sub(r'(?i)(\b(?:Bearer|Basic)\s+)\S+', r'\1[redacted]', text)
    text = re.sub(r'(?i)(\b(?:password|secret|token|api_key|authorization)\s*[:=]\s*)\S+', r'\1[redacted]', text)
    return text[-LIMIT:]


def write(phase, *, started=None, failed=False, finished=False, output=''):
    if phase not in PHASES:
        raise ValueError('Unknown launcher phase')
    now = time.time()
    data = {'phase': phase, 'started_at': started or now, 'finished_at': now if finished else None,
            'failed': failed, 'output': output}
    STATUS.parent.mkdir(parents=True, exist_ok=True)
    temporary = STATUS.with_suffix(f'.{os.getpid()}.tmp')
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, 'w') as stream:
        json.dump(data, stream)
    temporary.replace(STATUS)


def read():
    try:
        return json.loads(STATUS.read_text())
    except (OSError, ValueError):
        return {'phase': 'prepare', 'started_at': time.time(), 'failed': False, 'output': ''}


def run(phase, command):
    started = time.time()
    write(phase, started=started)
    tail = b''
    # Keep stdout/stderr separate, and preserve their terminal behaviour (Cargo
    # colours, progress and terminal width), rather than piping Cargo through tee.
    with selectors.DefaultSelector() as selector:
        readers, writers = [], []
        try:
            for destination in (1, 2):
                if os.isatty(destination):
                    reader, writer = pty.openpty()
                    size = fcntl.ioctl(destination, termios.TIOCGWINSZ, b'\0' * 8)
                    fcntl.ioctl(writer, termios.TIOCSWINSZ, size)
                    attributes = termios.tcgetattr(writer)
                    attributes[1] &= ~termios.OPOST
                    termios.tcsetattr(writer, termios.TCSANOW, attributes)
                else:
                    reader, writer = os.pipe()
                readers.append(reader)
                writers.append(writer)
                selector.register(reader, selectors.EVENT_READ, destination)
            try:
                process = subprocess.Popen(command, stdout=writers[0], stderr=writers[1])
            except OSError as error:
                message = f'Could not run this step: {error.strerror}\n'
                print(message, end='', file=sys.stderr)
                write(phase, started=started, failed=True, finished=True, output=message)
                return 1
            finally:
                for writer in writers:
                    os.close(writer)
            while selector.get_map():
                for key, _ in selector.select():
                    try:
                        chunk = os.read(key.fd, 8192)
                    except OSError as error:
                        if error.errno != errno.EIO:
                            raise
                        chunk = b''  # A PTY reports EIO when its writer closes.
                    if not chunk:
                        selector.unregister(key.fd)
                        continue
                    remaining = memoryview(chunk)
                    while remaining:
                        remaining = remaining[os.write(key.data, remaining):]
                    if phase in CAPTURE:
                        tail = (tail + chunk)[-LIMIT:]
                        write(phase, started=started, output=redact(tail))
            result = process.wait()
            # The owner reports an early app exit. Do not overwrite that
            # failure after the start request returns successfully.
            if phase != 'start' or result != 0:
                write(phase, started=started, failed=result != 0, finished=True, output=redact(tail))
            return result if result >= 0 else 128 - result
        finally:
            for reader in readers:
                os.close(reader)


if __name__ == '__main__':
    if sys.argv[1] == 'run':
        sys.exit(run(sys.argv[2], sys.argv[3:]))
    if sys.argv[1] == 'fail':
        current = read()
        write(current['phase'], started=current.get('started_at'), failed=True,
              finished=True, output=current.get('output', ''))
    else:
        write(sys.argv[1], failed='--failed' in sys.argv[2:])
