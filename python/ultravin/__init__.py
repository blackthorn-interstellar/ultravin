"""ultravin — pure-Rust NHTSA vPIC VIN decoder (CLI + library).

The decode logic lives in the compiled `ultravin._ultravin` extension; this
package only re-exports it. No logic here.
"""

from ultravin._ultravin import (
    __version__,
    decode,
    decode_batch,
    decode_batch_json,
    decode_json,
)

__all__ = ["__version__", "decode", "decode_batch", "decode_batch_json", "decode_json"]
