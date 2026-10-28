"""UI-independent policy for a single gentle charging request per discharge."""
from __future__ import annotations


class LowBatteryHelpPolicy:
    """Unknown samples never ask or rearm an already delivered request."""

    def __init__(self) -> None:
        self._asked = False

    def should_ask(self, reading: dict) -> bool:
        """Consume one low-battery request, rearming only on known external power.

        A true result immediately latches the request. Callers must establish
        that a help surface is eligible before asking; this policy does not
        observe whether the UI was actually displayed.
        """
        if reading.get("available") is not True or reading.get("has_battery") is not True:
            return False
        on_battery = reading.get("on_battery")
        charging = reading.get("charging")
        if on_battery is False:
            # A known external-power reading is genuine even when charge state
            # or percentage is temporarily unavailable (for example on attach).
            self._asked = False
            return False
        if on_battery is not True or charging is not False:
            return False
        percent = reading.get("percent")
        if type(percent) is not int or not 0 <= percent <= 20 or self._asked:
            return False
        self._asked = True
        return True
