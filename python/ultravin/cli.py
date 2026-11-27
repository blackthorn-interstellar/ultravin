"""The `ultravin` command. Rust parses and runs it; this only hands over argv."""

import signal
import sys

from ultravin._ultravin import _cli


def main() -> None:
    # Python's SIGINT handler only sets a flag for the interpreter, which runs
    # nothing until the job returns. The default action lets Ctrl-C stop it.
    signal.signal(signal.SIGINT, signal.SIG_DFL)
    sys.exit(_cli(["ultravin", *sys.argv[1:]]))


if __name__ == "__main__":
    main()
