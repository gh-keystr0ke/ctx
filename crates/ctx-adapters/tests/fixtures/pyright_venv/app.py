from sqlalchemy.ext.asyncio import AsyncSession


class SubscriptionDB:
    __tablename__ = "billing_subs"
    id: int


async def create(session: AsyncSession) -> None:
    db_subscription = SubscriptionDB()
    session.add(db_subscription)
    session.add_all([db_subscription])
