"""Standalone ONEFILE updater entry: no desktop, runtime or model imports."""
from inklow.update_install import helper_main

if __name__ == "__main__":
    raise SystemExit(helper_main())
