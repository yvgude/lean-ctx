import time
from unittest import mock

def test_clock(monkeypatch):
    monkeypatch.setattr(time, "time", lambda: 0)
    assert time.time() == 0

@mock.patch("time.monotonic")
def test_mono(m):
    assert m is not None
