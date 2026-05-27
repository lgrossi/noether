from __future__ import annotations

import asyncio
import os
import uuid
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from typing import Any

from noether_sidecar import NoetherClient

try:
    from litellm.integrations.custom_logger import CustomLogger
except Exception:

    class CustomLogger:  # type: ignore[no-redef]
        pass


JsonObject = dict[str, Any]


@dataclass(frozen=True)
class NoetherLiteLLMConfig:
    noether_url: str = "http://127.0.0.1:4051"
    timeout: float = 1.0
    fail_mode: str = "fail_closed"
    project: str | None = None
    subject: str | None = None
    budget_id: str | None = None
    entities: tuple[str, ...] = ()

    @classmethod
    def from_env(cls, env: Mapping[str, str] | None = None) -> "NoetherLiteLLMConfig":
        values = env or os.environ
        return cls(
            noether_url=values.get("NOET_URL", "http://127.0.0.1:4051").rstrip("/"),
            timeout=float(values.get("NOET_LITELLM_TIMEOUT", "1.0")),
            fail_mode=values.get("NOET_LITELLM_FAIL_MODE", "fail_closed"),
            project=_empty_to_none(values.get("NOET_LITELLM_PROJECT")),
            subject=_empty_to_none(values.get("NOET_LITELLM_SUBJECT")),
            budget_id=_empty_to_none(values.get("NOET_LITELLM_BUDGET_ID")),
            entities=tuple(_split_csv(values.get("NOET_LITELLM_ENTITIES"))),
        )


class NoetherLiteLLMHandler(CustomLogger):
    def __init__(
        self,
        config: NoetherLiteLLMConfig | None = None,
        client: NoetherClient | None = None,
    ) -> None:
        self.config = config or NoetherLiteLLMConfig.from_env()
        self.client = client or NoetherClient(
            url=self.config.noether_url,
            timeout=self.config.timeout,
            fail_mode=self.config.fail_mode,  # type: ignore[arg-type]
        )

    async def async_pre_call_hook(
        self,
        user_api_key_dict: Any,
        cache: Any,
        data: JsonObject,
        call_type: str,
    ) -> JsonObject | str:
        request_id = _request_id(data)
        authorize_request = self.build_authorize_request(data, user_api_key_dict, call_type, request_id)
        decision = await asyncio.to_thread(self.client.authorize, authorize_request)
        _store_noether_context(data, request_id, decision)
        if decision.get("outcome") == "deny":
            return _deny_message(decision)
        return data

    async def async_post_call_success_hook(
        self,
        data: JsonObject,
        user_api_key_dict: Any,
        response: Any,
    ) -> Any:
        context = _noether_context(data)
        if not context:
            return response
        usage = extract_usage(data, response)
        payload = {
            "reservation_id": context["reservation_id"],
            "actual_cost_usd": usage.get("cost_usd"),
            "usage": usage,
            "metadata": {
                "trace_id": context.get("trace_id"),
                "request_id": context.get("request_id"),
                "source": "noether-litellm",
                "outcome": "success",
            },
        }
        await asyncio.to_thread(self.client.finalize, context["reservation_id"], payload)
        return response

    async def async_post_call_failure_hook(
        self,
        request_data: JsonObject,
        original_exception: Exception,
        user_api_key_dict: Any,
        traceback_str: str | None = None,
    ) -> None:
        context = _noether_context(request_data)
        if not context:
            return None
        payload = {
            "reservation_id": context["reservation_id"],
            "actual_cost_usd": 0,
            "metadata": {
                "trace_id": context.get("trace_id"),
                "request_id": context.get("request_id"),
                "source": "noether-litellm",
                "outcome": "failure",
                "error_type": type(original_exception).__name__,
                "error": str(original_exception),
            },
        }
        await asyncio.to_thread(self.client.finalize, context["reservation_id"], payload)
        await asyncio.to_thread(
            self.client.event,
            {
                "trace_id": context.get("trace_id"),
                "kind": "litellm.call_failure",
                "payload": {
                    "source": "noether-litellm",
                    "decision_id": context.get("decision_id"),
                    "reservation_id": context.get("reservation_id"),
                    "request_id": context.get("request_id"),
                    "error_type": type(original_exception).__name__,
                    "error": str(original_exception),
                    "traceback": traceback_str,
                },
            },
        )
        return None

    def build_authorize_request(
        self,
        data: JsonObject,
        user_api_key_dict: Any,
        call_type: str,
        request_id: str,
    ) -> JsonObject:
        metadata = _metadata(data)
        model = _string(data.get("model"))
        provider = _provider_from_model(model)
        project = self.config.project or _string(metadata.get("project")) or _attr(user_api_key_dict, "team_id")
        subject = self.config.subject or _string(metadata.get("subject")) or _string(data.get("user")) or _attr(user_api_key_dict, "user_id")
        trace_id = _string(metadata.get("trace_id")) or request_id
        entities = _entities(self.config.entities, project, subject, _attr(user_api_key_dict, "team_id"))

        return _drop_none(
            {
                "budget_id": self.config.budget_id or _string(metadata.get("budget_id")),
                "entities": entities or None,
                "subject": subject,
                "project": project,
                "provider": provider,
                "model": model,
                "estimated_tokens": _integer(metadata.get("noether_estimated_tokens")),
                "estimated_cost_usd": _number(metadata.get("noether_estimated_cost_usd")),
                "metadata": _drop_none(
                    {
                        "harness": "litellm",
                        "integration": "noether-litellm",
                        "call_type": call_type,
                        "trace_id": trace_id,
                        "request_id": request_id,
                        "litellm_model": model,
                        "litellm_user": _string(data.get("user")),
                        "api_key_alias": _attr(user_api_key_dict, "key_alias"),
                        "team_id": _attr(user_api_key_dict, "team_id"),
                        "payload_keys": sorted(str(key) for key in data.keys()),
                        "message_count": _sequence_length(data.get("messages")),
                        "input_count": _sequence_length(data.get("input")),
                        "stream": data.get("stream") if isinstance(data.get("stream"), bool) else None,
                    }
                ),
            }
        )


def extract_usage(data: Mapping[str, Any], response: Any) -> JsonObject:
    usage = _mapping(_get(response, "usage"))
    cost = _number(data.get("response_cost")) or _number(_get(response, "response_cost"))
    return _drop_none(
        {
            "provider": _provider_from_model(_string(data.get("model"))),
            "model": _string(data.get("model")),
            "input_tokens": _integer(usage.get("prompt_tokens") or usage.get("input_tokens")),
            "output_tokens": _integer(usage.get("completion_tokens") or usage.get("output_tokens")),
            "total_tokens": _integer(usage.get("total_tokens")),
            "cost_usd": cost,
            "stop_reason": _stop_reason(response),
        }
    )


def _request_id(data: JsonObject) -> str:
    metadata = _metadata(data)
    return _string(metadata.get("request_id")) or f"litellm-{uuid.uuid4()}"


def _store_noether_context(data: JsonObject, request_id: str, decision: Mapping[str, Any]) -> None:
    reservation = _mapping(decision.get("reservation"))
    reservation_id = _string(reservation.get("id"))
    if not reservation_id:
        return
    metadata = data.setdefault("metadata", {})
    if not isinstance(metadata, dict):
        metadata = {}
        data["metadata"] = metadata
    metadata["_noether"] = {
        "decision_id": decision.get("decision_id"),
        "reservation_id": reservation_id,
        "request_id": request_id,
        "trace_id": _string(_mapping(decision.get("metadata")).get("trace_id")) or _string(metadata.get("trace_id")) or request_id,
    }


def _noether_context(data: Mapping[str, Any]) -> JsonObject:
    return _mapping(_metadata(data).get("_noether"))


def _metadata(data: Mapping[str, Any]) -> JsonObject:
    return _mapping(data.get("metadata") or _mapping(data.get("litellm_params")).get("metadata"))


def _deny_message(decision: Mapping[str, Any]) -> str:
    explanations = decision.get("explanations")
    if isinstance(explanations, Sequence) and not isinstance(explanations, str):
        reasons = [
            str(item.get("reason"))
            for item in explanations
            if isinstance(item, Mapping) and item.get("reason")
        ]
        if reasons:
            return f"Noether denied request: {'; '.join(reasons)}"
    return "Noether denied request"


def _provider_from_model(model: str | None) -> str | None:
    if not model or "/" not in model:
        return None
    provider, _separator, _model = model.partition("/")
    return provider or None


def _entities(configured: Sequence[str], project: str | None, subject: str | None, team_id: str | None) -> list[str]:
    values = [*configured]
    if project:
        values.append(f"project:{project}")
    if subject:
        values.append(subject if ":" in subject else f"user:{subject}")
    if team_id:
        values.append(f"team:{team_id}")
    deduped = []
    for value in values:
        if value not in deduped:
            deduped.append(value)
    return deduped


def _stop_reason(response: Any) -> str | None:
    choices = _get(response, "choices")
    if isinstance(choices, Sequence) and not isinstance(choices, str) and choices:
        return _string(_get(choices[0], "finish_reason"))
    return None


def _mapping(value: Any) -> JsonObject:
    return dict(value) if isinstance(value, Mapping) else {}


def _get(value: Any, key: str) -> Any:
    if isinstance(value, Mapping):
        return value.get(key)
    return getattr(value, key, None)


def _attr(value: Any, key: str) -> str | None:
    return _string(_get(value, key))


def _string(value: Any) -> str | None:
    return value if isinstance(value, str) and value else None


def _integer(value: Any) -> int | None:
    return int(value) if isinstance(value, int | float) and value >= 0 else None


def _number(value: Any) -> float | None:
    return float(value) if isinstance(value, int | float) and value >= 0 else None


def _sequence_length(value: Any) -> int | None:
    return len(value) if isinstance(value, Sequence) and not isinstance(value, str) else None


def _split_csv(value: str | None) -> list[str]:
    if not value:
        return []
    return [item.strip() for item in value.split(",") if item.strip()]


def _empty_to_none(value: str | None) -> str | None:
    return value.strip() if value and value.strip() else None


def _drop_none(value: JsonObject) -> JsonObject:
    return {key: item for key, item in value.items() if item is not None}


proxy_handler_instance = NoetherLiteLLMHandler()
