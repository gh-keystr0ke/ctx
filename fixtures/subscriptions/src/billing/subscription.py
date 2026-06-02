from dataclasses import dataclass
from datetime import datetime


@dataclass
class Subscription:
    status: str
    paid_until: datetime


class SubscriptionService:
    def cancel(self, subscription: Subscription, now: datetime) -> None:
        if subscription.paid_until > now:
            subscription.status = "canceling"
        else:
            subscription.status = "inactive"


class StripeWebhookHandler:
    def handle_subscription_update(
        self, subscription: Subscription, now: datetime
    ) -> None:
        SubscriptionService().cancel(subscription, now)
