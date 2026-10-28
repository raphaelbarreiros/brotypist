"""Per-profile process lock. A duplicate assistant must not intercept keys twice."""
from __future__ import annotations

from contextlib import contextmanager
import errno
import os
import sys

from .config import config_path


@contextmanager
def instance_lock():
    """Yield whether this profile's nonblocking process lock was acquired.

    A False result is normal contention: callers must not start another key
    interceptor. The descriptor remains owned for the context's lifetime;
    unexpected filesystem or locking errors propagate.
    """
    folder = config_path().parent
    folder.mkdir(parents=True, exist_ok=True, mode=0o700)
    descriptor = os.open(folder / "instance.lock", os.O_RDWR | os.O_CREAT, 0o600)
    acquired = False
    try:
        if sys.platform == "win32":
            import msvcrt
            if os.fstat(descriptor).st_size == 0:
                os.write(descriptor, b"0")
            os.lseek(descriptor, 0, os.SEEK_SET)
            try:
                msvcrt.locking(descriptor, msvcrt.LK_NBLCK, 1)
                acquired = True
            except OSError as error:
                if error.errno not in (errno.EACCES, errno.EAGAIN, errno.EDEADLK):
                    raise
        else:
            import fcntl
            try:
                fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
                acquired = True
            except OSError as error:
                if error.errno not in (errno.EACCES, errno.EAGAIN):
                    raise
        yield acquired
        # Keep the lock file in place so later launches lock the same inode.
    finally:
        if acquired:
            if sys.platform == "win32":
                os.lseek(descriptor, 0, os.SEEK_SET)
                msvcrt.locking(descriptor, msvcrt.LK_UNLCK, 1)
            else:
                fcntl.flock(descriptor, fcntl.LOCK_UN)
        os.close(descriptor)
