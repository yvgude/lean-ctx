"""Utility functions for data processing."""

import json
import os
from datetime import datetime
from functools import wraps
from typing import Any, Dict, List, Optional


def timer(func):
    """Decorator that logs execution time."""
    @wraps(func)
    def wrapper(*args, **kwargs):
        start = datetime.now()
        result = func(*args, **kwargs)
        elapsed = (datetime.now() - start).total_seconds()
        print(f"{func.__name__} took {elapsed:.3f}s")
        return result
    return wrapper


class DataProcessor:
    """Processes data through a configurable pipeline."""

    def __init__(self, config: Optional[Dict[str, Any]] = None):
        self.config = config or {}
        self.items: List[Dict[str, Any]] = []

    @timer
    def load(self, path: str) -> None:
        with open(path) as f:
            self.items = json.load(f)

    @timer
    def transform(self, fn) -> List[Dict[str, Any]]:
        return [fn(item) for item in self.items]

    def save(self, path: str, data: List[Dict[str, Any]]) -> None:
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as f:
            json.dump(data, f, indent=2)


def merge_dicts(base: Dict, override: Dict) -> Dict:
    result = base.copy()
    result.update(override)
    return result


def parse_config(path: str) -> Dict[str, Any]:
    with open(path) as f:
        return json.load(f)
