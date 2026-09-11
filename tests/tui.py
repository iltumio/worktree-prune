"""Exercise actual keyboard input, deletion and terminal restoration on Linux."""
import fcntl
import os
from pathlib import Path
import pty
import select
import struct
import subprocess
import sys
import tempfile
import termios
import time

BINARY = Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/worktree-prune').resolve()


def git(root, *args):
    subprocess.run(['git', '-C', str(root), *args], check=True, capture_output=True)


def scenario(mode):
    with tempfile.TemporaryDirectory() as temp:
        root = Path(temp) / 'repo'
        root.mkdir()
        git(root, 'init', '-b', 'main')
        git(root, 'config', 'user.name', 'Test')
        git(root, 'config', 'user.email', 'test@example.invalid')
        git(root, 'config', 'commit.gpgsign', 'false')
        git(root, 'config', 'core.hooksPath', '/dev/null')
        git(root, 'commit', '--allow-empty', '-m', 'initial')
        wt = Path(temp) / 'feature'
        git(root, 'worktree', 'add', '-b', 'feature', str(wt))
        if mode in ['blocked', 'force_apply', 'force_cancel', 'force_reset']:
            (wt / 'precious').write_text('keep me')
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 32, 160, 0, 0))
        original = termios.tcgetattr(slave)
        env = dict(os.environ, TERM='xterm-256color', CARGO_TARGET_BASE_DIR=str(Path(temp) / 'targets'))
        args = [str(BINARY)]
        process = subprocess.Popen(args, cwd=wt if mode == 'hard_block' else root, env=env, stdin=slave, stdout=slave, stderr=slave)
        captured = bytearray()

        def wait_for(needle):
            deadline = time.monotonic() + 10
            while needle not in captured:
                assert time.monotonic() < deadline, (needle, captured.decode(errors='replace'))
                if select.select([master], [], [], 0.1)[0]:
                    captured.extend(os.read(master, 65536))
                assert process.poll() is None, captured.decode(errors='replace')
            end = captured.index(needle) + len(needle)
            del captured[:end]

        try:
            wait_for(b'preview')
            if mode == 'cancel':
                os.write(master, b'\x03')
            else:
                os.write(master, b'j \r')
                wait_for(b'Removal')
                if mode == 'apply':
                    os.write(master, b'y')
                elif mode in ['blocked', 'force_apply', 'force_cancel', 'force_reset']:
                    wait_for(b'confirm FORCE')
                    assert wt.exists()
                    if mode == 'blocked':
                        os.write(master, b'nq')
                    else:
                        os.write(master, b'y')
                        wait_for(b'confirm')
                        assert wt.exists(), 'enabling force must not delete immediately'
                        if mode == 'force_apply':
                            os.write(master, b'y')
                        elif mode == 'force_cancel':
                            os.write(master, b'q')
                        else:
                            os.write(master, b'\x1b')
                            wait_for(b'preview')
                            os.write(master, b'\r')
                            wait_for(b'confirm FORCE')
                            os.write(master, b'nq')
                elif mode == 'hard_block':
                    wait_for(b'cannot force')
                    os.write(master, b'yq')
                else:
                    os.write(master, b'q')
            # Keep draining redraws: a PTY can fill while the child is exiting,
            # especially on CI runners with smaller terminal buffers.
            deadline = time.monotonic() + 10
            while process.poll() is None:
                assert time.monotonic() < deadline, 'TUI did not exit'
                if select.select([master], [], [], 0.1)[0]:
                    captured.extend(os.read(master, 65536))
            assert process.returncode == 0, captured.decode(errors='replace')
            assert wt.exists() == (mode not in ['apply', 'force_apply']), mode
            assert termios.tcgetattr(slave) == original, 'terminal was not restored'
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
            os.close(master)
            os.close(slave)
    print(f'TUI {mode}: passed')


for mode in ['preview', 'cancel', 'blocked', 'apply', 'force_apply', 'force_cancel', 'force_reset', 'hard_block']:
    scenario(mode)
