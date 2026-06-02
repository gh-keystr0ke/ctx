from datetime import datetime, timedelta

from billing.subscription import Subscription, SubscriptionService


def test_cancel_keeps_access_until_paid_until() -> None:
    now = datetime.now()
    subscription = Subscription(status="active", paid_until=now + timedelta(days=10))

    SubscriptionService().cancel(subscription, now)

    assert subscription.status == "canceling"
