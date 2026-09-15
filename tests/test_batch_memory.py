import json
from datetime import datetime, timezone

import ultravin as uv


def test_large_batches_preserve_all_python_and_json_shapes_across_chunks() -> None:
    vins = ["1HGCM82633A004352", "1FTFW1ET5DFC10312"] * 4_097
    years = [1995 if index % 3 == 0 else None for index in range(len(vins))]
    now = datetime(2026, 9, 13, tzinfo=timezone.utc)

    for full in (False, True):
        dictionaries = uv.decode_batch(vins, years=years, full=full, now=now)
        array = json.loads(uv.decode_batch_json(vins, years=years, full=full, now=now))
        lines = [
            json.loads(line) for line in uv._decode_batch_jsonl(vins, years=years, full=full, now=now).splitlines()
        ]
        assert array == dictionaries
        del array
        assert lines == dictionaries
        del lines
        assert [row["vin"] for row in dictionaries] == vins
        for index in (0, 8_191, 8_192, len(vins) - 1):
            assert dictionaries[index] == uv.decode(vins[index], year=years[index], full=full, now=now)
