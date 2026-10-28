"""Deferred native Quit must finish inside AppKit's nested modal run loop."""
from collections import deque
from types import SimpleNamespace
import time

import pytest

A = pytest.importorskip("AppKit")
from Foundation import NSDate, NSObject, NSRunLoop, NSTimer
from inklow import appkit_ui
from inklow.config import Config
from inklow.runtime import ShutdownPending


class CleanupBoundary:
    """Model an owned resource that needs a fixed number of cancellation retries."""

    def __init__(self, pending=0):
        self.pending = pending
        self.closed = False

    def close(self):
        """Raise pending until retries are exhausted, then mark ownership released."""
        if self.pending:
            self.pending -= 1
            raise ShutdownPending("Owned operation is still cancelling")
        self.closed = True

    stop = close


class ModalTerminationHost:
    """OS boundary: terminate waits in its documented mode before returning."""
    def __init__(self, shell):
        self.shell = shell
        self.replies = []
        self.resolved_before_return = False
        self.decision = None

    def terminate_(self, sender):
        """Pump only the nested modal mode; do not let the outer timer rescue Quit."""
        self.decision = self.shell.applicationShouldTerminate_(self)
        deadline = time.monotonic() + 0.6
        while not self.replies and time.monotonic() < deadline:
            NSRunLoop.currentRunLoop().runMode_beforeDate_(
                A.NSModalPanelRunLoopMode, NSDate.dateWithTimeIntervalSinceNow_(0.02))
        self.resolved_before_return = bool(self.replies)

    def replyToApplicationShouldTerminate_(self, approved):
        self.replies.append(bool(approved))


@pytest.fixture
def native_quit(monkeypatch):
    """Create timer-driven Quit scenarios and cancel deferred selectors on teardown."""
    shells = []

    def run(*, pending_cleanup=0, discard=True):
        shell = appkit_ui.BroTypistApplication.alloc().init()
        shells.append(shell)
        controller, overlay, updates = CleanupBoundary(pending_cleanup), CleanupBoundary(), CleanupBoundary()
        controller.config = Config(check_updates_automatically=False)
        controller.tick = lambda: None
        controller.drain_events = lambda: []
        shell.controller, shell.overlay, shell.updates = controller, overlay, updates
        shell.window = shell.welcome = shell.power_ui = shell.update_handoff = None
        shell.stopping = shell.shutdown_started = shell.shutdown_complete = False
        shell.termination_requested = shell.termination_waiting = False
        shell.termination_drafts_approved = shell.update_committed = False
        shell.startup_initialized = shell.startup_acknowledged = True
        shell.startup_failed = shell.resume_menu_shortcuts = False
        shell.shutdown_next_retry = 0.0
        shell.shutdown_error = shell.last_error = ""
        shell.tracking_menus = set()
        monkeypatch.setattr(shell, "_allow_discard_instruction_drafts", lambda: discard)
        monkeypatch.setattr(shell, "_refresh_updates", lambda: None)
        monkeypatch.setattr(shell, "_refresh_status", lambda: None)
        host = ModalTerminationHost(shell)
        monkeypatch.setattr(A, "NSApplication", SimpleNamespace(sharedApplication=lambda: host))
        shell.pending_menu_actions = deque([(lambda sender: host.terminate_(sender), None)])
        timer = NSTimer.timerWithTimeInterval_target_selector_userInfo_repeats_(
            0.025, shell, "tick:", None, True)
        try:
            # This real timer cannot re-enter its own callback to resolve Quit.
            timer.fire()
        finally:
            timer.invalidate()
        return SimpleNamespace(shell=shell, host=host, controller=controller, overlay=overlay, updates=updates)

    yield run
    for shell in shells:
        NSObject.cancelPreviousPerformRequestsWithTarget_(shell)


@pytest.mark.parametrize("pending_cleanup", [0, 2])
def test_timer_dispatched_quit_finishes_cleanup_before_modal_wait_returns(native_quit, pending_cleanup):
    result = native_quit(pending_cleanup=pending_cleanup)
    assert result.host.decision == A.NSTerminateLater
    assert result.host.resolved_before_return, "Quit stalled until its initiating GUI timer returned"
    assert result.host.replies == [True]
    assert result.controller.closed and result.overlay.closed and result.updates.closed
    assert result.shell.shutdown_complete


def test_cancelled_draft_prompt_replies_without_closing_resources(native_quit):
    result = native_quit(discard=False)
    assert result.host.resolved_before_return, "Draft cancellation did not release the modal Quit wait"
    assert result.host.replies == [False]
    assert not result.controller.closed and not result.overlay.closed and not result.updates.closed
    assert not result.shell.shutdown_started
    assert not result.shell.termination_requested and not result.shell.termination_waiting
