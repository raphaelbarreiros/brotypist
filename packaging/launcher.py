"""Absolute-import entry point for the frozen desktop executable."""
from multiprocessing import freeze_support

from inklow.__main__ import main

if __name__ == "__main__":
    # Dispatch frozen multiprocessing children before entering the desktop CLI.
    freeze_support()
    raise SystemExit(main())
