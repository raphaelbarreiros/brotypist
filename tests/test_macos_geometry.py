"""Synthetic AX geometry boundary cases; no real apps, hooks or user content.

The oracle records exact native UTF-16 requests and forbids sensitive reads.
These tests do not establish live Electron rendering or acceptance support.
"""
from types import SimpleNamespace

import pytest

from inklow.native import macos
from inklow.native.macos import MacBridge


RECT = ((120, 240), (9, 21))
EXPECTED = (120.0, 240.0, 9.0, 21.0)


class GeometryHost:
    """Native AX fake: explicit tree, text, permissions and native rectangles."""

    def __init__(self, text, leaves):
        self.bridge = MacBridge.__new__(MacBridge)
        self.bridge._secure_description = "Localized secure role"
        self.attributes = {
            ("field", "AXRole"): "AXTextArea",
            ("field", "AXValue"): text,
            ("field", "AXChildren"): [f"leaf{i}" for i in range(len(leaves))],
        }
        for i, value in enumerate(leaves):
            self.attributes[(f"leaf{i}", "AXRole")] = "AXStaticText"
            self.attributes[(f"leaf{i}", "AXValue")] = value
        self.bounds = {}
        self.reads = []
        self.requests = []
        self.forbidden = set()
        self.after_read = None
        self.bridge.ax = SimpleNamespace(
            kAXValueCFRangeType=1, kAXValueCGRectType=2,
            AXValueCreate=lambda kind, value: value,
            AXValueGetValue=lambda value, kind, output: (True, value),
            AXUIElementCopyAttributeValue=self.attribute,
            AXUIElementCopyParameterizedAttributeValue=self.range_bounds,
        )

    def attribute(self, node, name, output):
        self.reads.append((node, name))
        assert (node, name) not in self.forbidden, "Forbidden AX content read"
        if self.after_read:
            self.after_read(node, name)
        return 0, self.attributes.get((node, name))

    def range_bounds(self, node, name, native_range, output):
        assert name == "AXBoundsForRange"
        self.requests.append((node, native_range))
        rectangle = self.bounds.get((node, native_range))
        return (0, rectangle) if rectangle is not None else (-25205, None)


@pytest.fixture(autouse=True)
def stable_clock(monkeypatch):
    monkeypatch.setattr(macos.time, "monotonic", lambda: 0.0)


@pytest.mark.parametrize("text,location,length", [
    ("hello", 3, 0), ("hello", 5, 0), ("hello", 1, 3),
    ("😀a", 3, 0), ("😀a", 0, 2), ("a😀b", 3, 1),
    ("e\u0301!", 1, 1), ("hello\nworld", 6, 5),
])
def test_single_leaf_uses_exact_utf16_range(text, location, length):
    host = GeometryHost(text, [text])
    host.bounds[("leaf0", (location, length))] = RECT
    assert host.bridge._range_bounds("field", location, length) == EXPECTED
    assert host.requests == [("field", (location, length)), ("leaf0", (location, length))]


@pytest.mark.parametrize("separator", ["", "\n", "\r", "\r\n", "\u2028", "\u2029", "\n\n"])
def test_multiple_leaves_use_only_observed_paragraph_gap(separator):
    host = GeometryHost("😀a" + separator + "bc", ["😀a", "bc"])
    location = 3 + len(separator)
    host.bounds[("leaf1", (1, 1))] = RECT
    assert host.bridge._range_bounds("field", location + 1, 1) == EXPECTED
    assert host.requests[-1] == ("leaf1", (1, 1))


def test_explicit_separator_leaf_can_prove_unique_mapping():
    host = GeometryHost("a\nb", ["a", "\n", "b"])
    host.bounds[("leaf2", (0, 1))] = RECT
    assert host.bridge._range_bounds("field", 2, 1) == EXPECTED


@pytest.mark.parametrize("text,leaves", [
    ("hello", ["other"]), ("hello", ["hell"]), ("hello", ["hello", "other"]),
    ("a b", ["a", "b"]), ("a\ufffcb", ["a", "b"]),
    ("\na", ["a"]), ("a\n", ["a"]),
    ("\ufeff", [""]), ("", ["\ufeff"]),
    ("a\n\nb", ["a", "\n", "b"]),
])
def test_unmatched_or_ambiguous_stream_never_queries_leaf_geometry(text, leaves):
    host = GeometryHost(text, leaves)
    for i in range(len(leaves)):
        host.bounds[(f"leaf{i}", (0, 0))] = RECT
    assert host.bridge._range_bounds("field", 0, 0) is None
    assert host.requests == [("field", (0, 0))]


@pytest.mark.parametrize("location,length", [(1, 0), (0, 1), (1, 1), (0, 4), (-1, 0), (0, -1)])
def test_surrogate_interior_or_out_of_range_is_refused(location, length):
    host = GeometryHost("😀a", ["😀a"])
    host.bounds[("leaf0", (location, length))] = RECT
    assert host.bridge._range_bounds("field", location, length) is None
    assert not any(node == "leaf0" for node, _ in host.requests)


@pytest.mark.parametrize("separator,location,length", [
    ("", 1, 2), ("\n", 1, 3), ("\n", 2, 1), ("", 2, 0),
])
def test_cross_leaf_ranges_separator_ranges_and_shared_carets_are_refused(separator, location, length):
    host = GeometryHost("ab" + separator + "cd", ["ab", "cd"])
    host.bounds[("leaf0", (location, length))] = RECT
    assert host.bridge._range_bounds("field", location, length) is None
    assert host.requests == [("field", (location, length))]


@pytest.mark.parametrize("node", ["field", "group", "leaf0"])
@pytest.mark.parametrize("attribute,value", [
    ("AXProtectedContent", True), ("AXHidden", True),
    ("AXSubrole", "AXSecureTextField"), ("AXRoleDescription", "Localized secure role"),
])
def test_sensitive_subtrees_are_rejected_before_values_or_children(node, attribute, value):
    host = GeometryHost("hello", ["hello"])
    host.attributes[("field", "AXChildren")] = ["group"]
    host.attributes[("group", "AXRole")] = "AXGroup"
    host.attributes[("group", "AXChildren")] = ["leaf0"]
    host.attributes[(node, attribute)] = value
    host.forbidden.update({(node, "AXValue"), (node, "AXChildren")})
    host.bounds[("leaf0", (1, 0))] = RECT
    assert host.bridge._range_bounds("field", 1, 0) is None
    assert host.requests == [("field", (1, 0))]


@pytest.mark.parametrize("role", ["AXTextArea", "AXTextField", "AXComboBox"])
def test_nested_editable_is_not_read_or_traversed(role):
    host = GeometryHost("hello", ["hello"])
    host.attributes[("leaf0", "AXRole")] = role
    host.forbidden.update({("leaf0", "AXValue"), ("leaf0", "AXChildren")})
    assert host.bridge._range_bounds("field", 1, 0) is None


def test_nontext_containers_never_read_values():
    host = GeometryHost("hello", ["hello"])
    host.attributes[("field", "AXChildren")] = ["group"]
    host.attributes[("group", "AXRole")] = "AXGroup"
    host.attributes[("group", "AXChildren")] = ["leaf0"]
    host.forbidden.add(("group", "AXValue"))
    host.bounds[("leaf0", (1, 0))] = RECT
    assert host.bridge._range_bounds("field", 1, 0) == EXPECTED


@pytest.mark.parametrize("cycle", [True, False])
def test_cycles_and_repeated_native_nodes_are_not_partial_text_proof(cycle):
    host = GeometryHost("hello", ["hello"])
    host.attributes[("field", "AXChildren")] = ["field"] if cycle else ["leaf0", "leaf0"]
    assert host.bridge._range_bounds("field", 1, 0) is None
    assert len(host.reads) < 30


@pytest.mark.parametrize("leaf_depth,accepted", [(8, True), (9, False)])
def test_depth_budget_requires_complete_tree(leaf_depth, accepted):
    host = GeometryHost("a", ["a"])
    parent = "field"
    for depth in range(1, leaf_depth):
        child = f"group{depth}"
        host.attributes[(parent, "AXChildren")] = [child]
        host.attributes[(child, "AXRole")] = "AXGroup"
        parent = child
    host.attributes[(parent, "AXChildren")] = ["leaf0"]
    host.bounds[("leaf0", (0, 1))] = RECT
    assert host.bridge._range_bounds("field", 0, 1) == (EXPECTED if accepted else None)


@pytest.mark.parametrize("leaf_count,accepted", [(127, True), (128, False)])
def test_node_budget_includes_root_and_never_accepts_truncation(leaf_count, accepted):
    host = GeometryHost("a" * leaf_count, ["a"] * leaf_count)
    host.bounds[(f"leaf{leaf_count - 1}", (0, 1))] = RECT
    assert host.bridge._range_bounds("field", leaf_count - 1, 1) == (EXPECTED if accepted else None)


def test_deadline_expiration_stops_before_late_leaf_value(monkeypatch):
    clock = [0.0]
    monkeypatch.setattr(macos.time, "monotonic", lambda: clock[0])
    host = GeometryHost("hello", ["hello"])
    host.forbidden.add(("leaf0", "AXValue"))

    def expire(node, name):
        if node == "leaf0" and name == "AXRole":
            clock[0] = 1.0

    host.after_read = expire
    assert host.bridge._range_bounds("field", 1, 0) is None
    assert host.requests == [("field", (1, 0))]


@pytest.mark.parametrize("rectangle", [
    ((float("nan"), 240), (9, 21)), ((120, float("inf")), (9, 21)),
    ((120, 240), (float("inf"), 21)), ((120, 240), (9, float("nan"))),
    ((120, 240), (-1, 21)), ((120, 240), (9, 0)), ((120, 240), (0, 21)),
])
def test_invalid_native_leaf_bounds_are_unavailable(rectangle):
    host = GeometryHost("hello", ["hello"])
    host.bounds[("leaf0", (0, 1))] = rectangle
    assert host.bridge._range_bounds("field", 0, 1) is None


def test_zero_width_collapsed_native_bounds_are_valid():
    host = GeometryHost("hello", ["hello"])
    host.bounds[("leaf0", (5, 0))] = ((120, 240), (0, 21))
    assert host.bridge._range_bounds("field", 5, 0) == (120.0, 240.0, 0.0, 21.0)


def test_valid_direct_native_bounds_need_no_descendant_or_value_reads():
    host = GeometryHost("unrelated", ["other"])
    host.forbidden.update(host.attributes)
    host.bounds[("field", (2, 1))] = RECT
    assert host.bridge._range_bounds("field", 2, 1) == EXPECTED
    assert host.reads == []
    assert host.requests == [("field", (2, 1))]


def test_invalid_direct_bounds_can_use_proven_leaf_bounds():
    host = GeometryHost("hello", ["hello"])
    host.bounds[("field", (0, 1))] = ((float("nan"), 0), (9, 21))
    host.bounds[("leaf0", (0, 1))] = RECT
    assert host.bridge._range_bounds("field", 0, 1) == EXPECTED


@pytest.mark.parametrize("role", ["AXGroup", "AXStaticText"])
def test_explicit_nested_editability_is_checked_before_content(role):
    host = GeometryHost("hello", ["hello"])
    host.attributes[("leaf0", "AXRole")] = role
    host.attributes[("leaf0", "AXEditable")] = True
    host.forbidden.update({("leaf0", "AXValue"), ("leaf0", "AXChildren")})
    assert host.bridge._range_bounds("field", 1, 0) is None


def test_later_protected_leaf_invalidates_otherwise_matching_early_geometry():
    host = GeometryHost("ab", ["a", "b"])
    host.attributes[("leaf1", "AXProtectedContent")] = True
    host.forbidden.add(("leaf1", "AXValue"))
    host.bounds[("leaf0", (0, 1))] = RECT
    assert host.bridge._range_bounds("field", 0, 1) is None
    assert host.requests == [("field", (0, 1))]


def test_excessive_separator_mapping_work_fails_closed():
    host = GeometryHost("a" + "\n" * 4097 + "b", ["a", "b"])
    host.bounds[("leaf1", (0, 1))] = RECT
    assert host.bridge._range_bounds("field", 4098, 1) is None
    assert host.requests == [("field", (4098, 1))]


def test_bounds_returning_after_deadline_are_not_accepted(monkeypatch):
    clock = [0.0]
    monkeypatch.setattr(macos.time, "monotonic", lambda: clock[0])
    host = GeometryHost("hello", ["hello"])
    host.bounds[("leaf0", (1, 0))] = RECT
    native_query = host.range_bounds

    def expire_after_query(node, name, native_range, output):
        result = native_query(node, name, native_range, output)
        if node == "leaf0":
            clock[0] = 1.0
        return result

    host.bridge.ax.AXUIElementCopyParameterizedAttributeValue = expire_after_query
    assert host.bridge._range_bounds("field", 1, 0) is None
    assert host.requests == [("field", (1, 0)), ("leaf0", (1, 0))]


@pytest.mark.parametrize("child", ["field", "leaf0", "protected"])
def test_static_text_with_children_is_not_assumed_to_be_a_leaf(child):
    host = GeometryHost("hello", ["hello"])
    host.attributes[("leaf0", "AXChildren")] = [child]
    host.attributes[("protected", "AXProtectedContent")] = True
    host.forbidden.update({("leaf0", "AXValue"), ("protected", "AXValue")})
    host.bounds[("leaf0", (1, 0))] = RECT
    assert host.bridge._range_bounds("field", 1, 0) is None
    assert host.requests == [("field", (1, 0))]
