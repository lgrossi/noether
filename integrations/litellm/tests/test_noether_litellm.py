from __future__ import annotations

import sys
from pathlib import Path
from unittest import IsolatedAsyncioTestCase, main

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "sdk/python"))
sys.path.insert(0, str(ROOT / "integrations/litellm"))

from noether_litellm import NoetherLiteLLMConfig, NoetherLiteLLMHandler, extract_usage  # noqa: E402


class FakeNoetherClient:
    def __init__(self, decision: dict) -> None:
        self.decision = decision
        self.authorize_calls: list[dict] = []
        self.finalize_calls: list[tuple[str, dict]] = []
        self.event_calls: list[dict] = []

    def authorize(self, request: dict) -> dict:
        self.authorize_calls.append(request)
        return self.decision

    def finalize(self, reservation_id: str, payload: dict) -> dict:
        self.finalize_calls.append((reservation_id, payload))
        return {"id": reservation_id, "status": "finalized"}

    def event(self, event: dict) -> dict:
        self.event_calls.append(event)
        return {"accepted": True}


class User:
    user_id = "user:alice"
    team_id = "platform"
    key_alias = "local-key"


class LiteLLMIntegrationTests(IsolatedAsyncioTestCase):
    async def test_pre_call_authorizes_and_keeps_provider_with_litellm(self) -> None:
        fake = FakeNoetherClient(
            {
                "decision_id": "decision-1",
                "outcome": "allow",
                "reservation": {"id": "reservation-1"},
                "explanations": [],
                "created_at": "2026-05-27T00:00:00Z",
            }
        )
        handler = NoetherLiteLLMHandler(
            config=NoetherLiteLLMConfig(project="noether"),
            client=fake,  # type: ignore[arg-type]
        )
        data = {
            "model": "openai/gpt-4.1",
            "messages": [{"role": "user", "content": "do not send"}],
            "metadata": {
                "trace_id": "trace-1",
                "noether_estimated_tokens": 42,
                "noether_estimated_cost_usd": 0.01,
            },
            "stream": True,
        }

        result = await handler.async_pre_call_hook(User(), None, data, "completion")

        self.assertIs(result, data)
        request = fake.authorize_calls[0]
        self.assertEqual(request["provider"], "openai")
        self.assertEqual(request["model"], "openai/gpt-4.1")
        self.assertEqual(request["project"], "noether")
        self.assertEqual(request["subject"], "user:alice")
        self.assertIn("project:noether", request["entities"])
        self.assertIn("team:platform", request["entities"])
        self.assertEqual(request["estimated_tokens"], 42)
        self.assertEqual(request["estimated_cost_usd"], 0.01)
        self.assertEqual(request["metadata"]["harness"], "litellm")
        self.assertEqual(request["metadata"]["integration"], "noether-litellm")
        self.assertEqual(request["metadata"]["trace_id"], "trace-1")
        self.assertEqual(request["metadata"]["message_count"], 1)
        self.assertNotIn("do not send", str(request))
        self.assertEqual(data["metadata"]["_noether"]["reservation_id"], "reservation-1")

    async def test_pre_call_denies_by_returning_litellm_rejection_string(self) -> None:
        fake = FakeNoetherClient(
            {
                "decision_id": "decision-1",
                "outcome": "deny",
                "explanations": [{"reason": "daily cap exceeded"}],
                "created_at": "2026-05-27T00:00:00Z",
            }
        )
        handler = NoetherLiteLLMHandler(client=fake)  # type: ignore[arg-type]

        result = await handler.async_pre_call_hook(User(), None, {"model": "openai/gpt-4.1"}, "completion")

        self.assertEqual(result, "Noether denied request: daily cap exceeded")

    async def test_success_finalizes_observed_usage(self) -> None:
        fake = FakeNoetherClient({"decision_id": "decision-1", "outcome": "allow", "reservation": {"id": "reservation-1"}})
        handler = NoetherLiteLLMHandler(client=fake)  # type: ignore[arg-type]
        data = {
            "model": "openai/gpt-4.1",
            "response_cost": 0.012,
            "metadata": {
                "_noether": {
                    "decision_id": "decision-1",
                    "reservation_id": "reservation-1",
                    "trace_id": "trace-1",
                    "request_id": "request-1",
                }
            },
        }
        response = {
            "usage": {"prompt_tokens": 10, "completion_tokens": 20, "total_tokens": 30},
            "choices": [{"finish_reason": "stop"}],
        }

        returned = await handler.async_post_call_success_hook(data, User(), response)

        self.assertIs(returned, response)
        reservation_id, payload = fake.finalize_calls[0]
        self.assertEqual(reservation_id, "reservation-1")
        self.assertEqual(payload["actual_cost_usd"], 0.012)
        self.assertEqual(payload["usage"]["input_tokens"], 10)
        self.assertEqual(payload["usage"]["output_tokens"], 20)
        self.assertEqual(payload["usage"]["total_tokens"], 30)
        self.assertEqual(payload["metadata"]["outcome"], "success")

    async def test_failure_finalizes_zero_cost_and_records_event(self) -> None:
        fake = FakeNoetherClient({"decision_id": "decision-1", "outcome": "allow", "reservation": {"id": "reservation-1"}})
        handler = NoetherLiteLLMHandler(client=fake)  # type: ignore[arg-type]
        data = {
            "metadata": {
                "_noether": {
                    "decision_id": "decision-1",
                    "reservation_id": "reservation-1",
                    "trace_id": "trace-1",
                    "request_id": "request-1",
                }
            }
        }

        await handler.async_post_call_failure_hook(data, RuntimeError("provider failed"), User(), "traceback")

        self.assertEqual(fake.finalize_calls[0][0], "reservation-1")
        self.assertEqual(fake.finalize_calls[0][1]["actual_cost_usd"], 0)
        self.assertEqual(fake.finalize_calls[0][1]["metadata"]["outcome"], "failure")
        self.assertEqual(fake.event_calls[0]["kind"], "litellm.call_failure")
        self.assertEqual(fake.event_calls[0]["payload"]["error_type"], "RuntimeError")

    def test_extract_usage_accepts_object_or_mapping_shapes(self) -> None:
        response = {
            "usage": {"prompt_tokens": 3, "completion_tokens": 4, "total_tokens": 7},
            "choices": [{"finish_reason": "stop"}],
        }

        usage = extract_usage({"model": "anthropic/claude-sonnet", "response_cost": 0.2}, response)

        self.assertEqual(usage["provider"], "anthropic")
        self.assertEqual(usage["input_tokens"], 3)
        self.assertEqual(usage["output_tokens"], 4)
        self.assertEqual(usage["cost_usd"], 0.2)
        self.assertEqual(usage["stop_reason"], "stop")


if __name__ == "__main__":
    main()
