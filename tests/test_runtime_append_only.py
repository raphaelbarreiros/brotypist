"""Append-only macOS capability must not disable ordinary completion consumers."""
from dataclasses import replace

import pytest

from test_runtime import field_snapshot, runtime, wait_request


def chat_snapshot(text="a", **changes):
    return replace(field_snapshot(text, can_replace=False), app_id="com.hnc.Discord", **changes)


def configure_chat(controller, bridge, tmp_path):
    model = tmp_path / "local.gguf"
    model.touch()
    controller.config = replace(controller.config, model_path=str(model), min_chars=1000)
    bridge.snapshot = chat_snapshot()
    bridge.info = {"app_id": "com.hnc.Discord", "name": "Synthetic Discord", "domain": ""}


def test_append_only_chat_prepares_local_model_once_without_evaluating_writing(runtime, tmp_path):
    controller, bridge, client = runtime
    configure_chat(controller, bridge, tmp_path)
    controller.tick()
    controller.tick()
    assert len(client.preparations) == 1
    assert not client.requests
    assert not bridge.insertions
    assert not controller._can_replace(bridge.snapshot)
    assert not controller._submit(bridge.snapshot, mode="emoji")
    assert not controller._submit(bridge.snapshot, mode="synonyms")
    assert not client.requests


@pytest.mark.parametrize("changes", [
    {"protected": True}, {"multiline": False}, {"selection_length": 1},
    {"on_demand_only": True}, {"inline_render": None},
    {"caret_rect": None},
])
def test_append_only_chat_still_obeys_preparation_privacy_and_layout_guards(runtime, tmp_path, changes):
    controller, bridge, client = runtime
    configure_chat(controller, bridge, tmp_path)
    bridge.snapshot = chat_snapshot(**changes)
    controller.tick()
    assert not client.preparations
    assert not client.requests
    assert not bridge.insertions


def test_append_only_chat_obeys_battery_and_pause_prewarm_guards(runtime, tmp_path):
    controller, bridge, client = runtime
    configure_chat(controller, bridge, tmp_path)
    controller.config = replace(controller.config, battery_on_demand=True)
    controller._on_battery = True
    controller.tick()
    assert not client.preparations
    controller._on_battery = False
    controller.pause(5)
    controller.tick()
    assert not client.preparations
    assert not client.requests


def test_append_only_chat_completion_still_accepts_through_insert(runtime):
    controller, bridge, client = runtime
    bridge.snapshot = chat_snapshot("Dear team, the update")
    bridge.info = {"app_id": "com.hnc.Discord", "name": "Synthetic Discord", "domain": ""}
    controller.tick()
    controller._candidate_since -= 10
    identifier = wait_request(controller, 0)
    client.events.append({"type": "result", "id": identifier, "text": " arrives", "final": True})
    controller.tick()
    assert controller.invoke("all")
    assert bridge.snapshot.text == "Dear team, the update arrives"
    assert bridge.insertions == [" arrives"]
