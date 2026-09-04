from typing import Any, Generic, TypeVar

from sqlalchemy.ext.asyncio import AsyncSession
from sqlalchemy.orm import Session, mapped_column

T = TypeVar("T")


class ScalarResult(Generic[T]):
    def scalar_one(self) -> T: ...


class Model:
    __tablename__ = "models"
    status: int = mapped_column()
    some_property: int


class Subscription:
    __tablename__ = "subscriptions"
    id: int = mapped_column()
    addons: list[Model]


class Queue:
    def add(self, item: Model) -> None: ...


async def typed_writes(
    session: Session,
    async_session: AsyncSession,
    result: ScalarResult[Model],
    annotated: Model,
    subscription: Subscription,
    queue: Queue,
    dynamic: Any,
    value: int,
) -> None:
    row = Model()
    session.add(row)
    session.add_all([row, annotated])
    session.merge(row)
    session.delete(row)
    async_session.add(annotated)

    fetched = await async_session.get(Model, 1)
    if fetched is not None:
        fetched.status = value

    fetched_optional = await async_session.get(Model, 2)
    fetched_optional.status = value

    selected = result.scalar_one()
    selected.status = value
    annotated.status = value
    annotated.status += 1
    annotated.some_property = value
    subscription.addons = [annotated]
    tags: set[str] = set()
    tags.add("name")
    collection: set[Model] = set()
    collection.add(annotated)
    queue.add(annotated)
    dynamic.status = value
