from typing import Generic, TypeVar

from sqlalchemy.ext.asyncio import AsyncSession
from sqlalchemy.orm import Session

T = TypeVar("T")


class ScalarResult(Generic[T]):
    def scalar_one(self) -> T: ...


class Model:
    status: int


async def typed_writes(
    session: Session,
    async_session: AsyncSession,
    result: ScalarResult[Model],
    annotated: Model,
    value: int,
) -> None:
    row = Model()
    session.add(row)
    session.add_all([row, annotated])
    session.merge(row)
    session.delete(row)

    fetched = await async_session.get(Model, 1)
    if fetched is not None:
        fetched.status = value

    selected = result.scalar_one()
    selected.status = value
    annotated.status = value
    annotated.status += 1
