from collections.abc import Iterable
from typing import TypeVar

T = TypeVar("T")


class Session:
    def add(self, instance: object) -> None: ...

    def add_all(self, instances: Iterable[object]) -> None: ...

    def merge(self, instance: T) -> T: ...

    def delete(self, instance: object) -> None: ...
