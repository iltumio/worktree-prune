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
        locked_dir = wt / 'locked_dir'
        env = dict(os.environ, TERM='xterm-256color', CARGO_TARGET_BASE_DIR=str(Path(temp) / 'targets'))
        if mode == 'sudo_apply':
            locked_dir.mkdir()
            (locked_dir / 'file').write_text('locked')
            os.chmod(locked_dir, 0o500)
            fake_bin = Path(temp) / 'bin'
            fake_bin.mkdir()
            sudo_shim = fake_bin / 'sudo'
            sudo_shim.write_text('#!/bin/sh\nchmod -R u+rwx "$5" 2>/dev/null || true\nexec "$@"\n')
            os.chmod(sudo_shim, 0o755)
            env['PATH'] = f"{fake_bin}:{env.get('PATH', '')}"
        second = None
        if mode in ['multi_apply', 'clean_multi']:
            other = Path(temp) / 'nested' / 'other'
            other.parent.mkdir()
            git(root, 'clone', str(root), str(other))
            second = other / '.worktrees' / 'second'
            git(other, 'worktree', 'add', '-b', 'second', str(second))
        clean = mode.startswith('clean_')
        target = None
        if clean:
            target = Path(temp) / 'targets' / 'repo' / ('main' if mode == 'clean_main' else 'feature') / 'target'
            if mode in ['clean_local', 'clean_tracked']:
                target = wt / 'target'
            target.mkdir(parents=True)
            (target / 'artifact').write_text('build cache')
            (wt / 'precious').write_text('keep me')
            if mode == 'clean_locked':
                git(root, 'worktree', 'lock', str(wt))
            if mode == 'clean_shared':
                (root / '.cargo').mkdir()
                (root / '.cargo' / 'config.toml').write_text(f"[build]\ntarget-dir = '{target}'\n")
            if mode == 'clean_tracked':
                git(wt, 'add', 'target/artifact')
            if mode == 'clean_local_commit':
                git(wt, 'add', 'precious')
                git(wt, 'commit', '-m', 'local-only')
        second_target = None
        if mode == 'clean_multi':
            second_target = Path(temp) / 'targets' / 'other' / 'second' / 'target'
            second_target.mkdir(parents=True)
            (second_target / 'artifact').write_text('cache')
        changed_target = None
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 32, 160, 0, 0))
        original = termios.tcgetattr(slave)
        args = [str(BINARY)]
        process = subprocess.Popen(args, cwd=wt if mode == 'hard_block' else (Path(temp) if mode in ['multi_apply', 'clean_multi'] else root), env=env, stdin=slave, stdout=slave, stderr=slave)
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
            if clean:
                os.write(master, b'c')
                wait_for(b'CLEAN TARGETS')
                os.write(master, b'a\r' if mode == 'clean_multi' else (b' \r' if mode == 'clean_main' else b'j \r'))
                wait_for(b'Clean targets')
                if mode in ['clean_shared', 'clean_tracked']:
                    wait_for(b'blocked')
                    os.write(master, b'yq')
                elif mode == 'clean_cancel':
                    os.write(master, b'nq')
                elif mode == 'clean_switch':
                    os.write(master, b'nc')
                    wait_for(b'worktree-prune')
                    os.write(master, b'\rq')
                else:
                    wait_for(b'Confirm clean')
                    if mode == 'clean_changed':
                        changed_target = Path(temp) / 'targets' / 'repo' / 'changed' / 'target'
                        changed_target.mkdir(parents=True)
                        (changed_target / 'precious').write_text('keep')
                        (wt / '.cargo').mkdir()
                        (wt / '.cargo' / 'config.toml').write_text(f"[build]\ntarget-dir = '{changed_target}'\n")
                    os.write(master, b'y')
            elif mode == 'cancel':
                os.write(master, b'\x03')
            else:
                os.write(master, b'a\r' if mode == 'multi_apply' else b'j \r')
                wait_for(b'Removal')
                if mode in ['apply', 'multi_apply']:
                    os.write(master, b'y')
                elif mode in ['blocked', 'force_apply', 'force_cancel', 'force_reset']:
                    wait_for(b'confirm FORCE')
                    assert wt.exists()
                    if mode == 'blocked':
                        os.write(master, b'nq')
                    elif mode == 'force_apply':
                        os.write(master, b'y')
                    elif mode == 'force_cancel':
                        os.write(master, b'q')
                    else:
                        os.write(master, b'\x1b')
                        wait_for(b'preview')
                        os.write(master, b'\r')
                        wait_for(b'confirm FORCE')
                        os.write(master, b'nq')
                elif mode == 'sudo_apply':
                    wait_for(b'confirm FORCE + SUDO')
                    assert wt.exists()
                    os.write(master, b'y')
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
            assert process.returncode == (1 if mode == 'clean_changed' else 0), captured.decode(errors='replace')
            assert wt.exists() == (mode not in ['apply', 'multi_apply', 'force_apply', 'sudo_apply']), mode
            if second is not None:
                assert second.exists() == (mode == 'clean_multi'), 'unexpected second worktree state'
                if second_target is not None:
                    assert not second_target.exists()
            if clean:
                assert target.exists() == (mode in ['clean_cancel', 'clean_shared', 'clean_tracked', 'clean_switch', 'clean_changed']), mode
                if changed_target is not None:
                    assert (changed_target / 'precious').read_text() == 'keep'
                assert (wt / 'precious').read_text() == 'keep me'
                git(root, 'show-ref', '--verify', 'refs/heads/feature')
                registered = subprocess.check_output(['git', '-C', str(root), 'worktree', 'list', '--porcelain'])
                assert str(wt).encode() in registered
            assert termios.tcgetattr(slave) == original, 'terminal was not restored'
        finally:
            if locked_dir.exists():
                os.chmod(locked_dir, 0o700)
            if process.poll() is None:
                process.kill()
                process.wait()
            os.close(master)
            os.close(slave)
    print(f'TUI {mode}: passed')


for mode in ['preview', 'cancel', 'blocked', 'apply', 'force_apply', 'force_cancel', 'force_reset', 'sudo_apply', 'hard_block', 'multi_apply']:
    scenario(mode)

for mode in ['clean_apply', 'clean_cancel', 'clean_main', 'clean_locked', 'clean_shared', 'clean_local', 'clean_tracked', 'clean_local_commit', 'clean_switch', 'clean_multi', 'clean_changed']:
    scenario(mode)
