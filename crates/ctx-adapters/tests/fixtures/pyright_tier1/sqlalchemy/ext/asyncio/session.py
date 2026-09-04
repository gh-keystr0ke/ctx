from typing import TypeVar

T = TypeVar("T")


class AsyncSession:
    async def get(self, entity: type[T], ident: int) -> T | None: ...
