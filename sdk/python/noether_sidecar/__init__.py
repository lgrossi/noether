from __future__ import annotations

import json
import urllib.error
import urllib.parse
import urllib.request
from collections.abc import Callable
from datetime import UTC, datetime
from typing import Any, Literal, TypeVar

FailMode = Literal["fail_open", "fail_closed"]
DecisionOutcome = Literal["allow", "warn", "deny"]
PolicyAction = Literal["allow", "warn", "ask", "block"]
DecisionSeverity = Literal["info", "warn", "deny"]

JsonObject = dict[str, Any]
T = TypeVar("T")


class NoetherError(Exception):
    pass


class NoetherHttpError(NoetherError):
    def __init__(self, status: int, body: str) -> None:
        super().__init__(f"Noether request failed with HTTP {status}: {body}")
        self.status = status
        self.body = body


class NoetherDeniedError(NoetherError):
    def __init__(self, decision: JsonObject) -> None:
        explanations = decision.get("explanations") or []
        reasons = "; ".join(
            str(item.get("reason"))
            for item in explanations
            if isinstance(item, dict) and item.get("reason")
        )
        super().__init__(f"Noether denied request: {reasons}")
        self.decision = decision


class NoetherClient:
    def __init__(
        self,
        url: str = "http://127.0.0.1:4051",
        timeout: float = 1.0,
        fail_mode: FailMode = "fail_closed",
    ) -> None:
        if fail_mode not in ("fail_open", "fail_closed"):
            raise ValueError("fail_mode must be fail_open or fail_closed")
        self.url = url.rstrip("/")
        self.timeout = timeout
        self.fail_mode = fail_mode

    def authorize(self, request: JsonObject) -> JsonObject:
        try:
            return self._post_json("/v1/authorize", request)
        except Exception as error:
            return _synthetic_decision(self.fail_mode, error)

    def require_authorization(self, request: JsonObject) -> JsonObject:
        decision = self.authorize(request)
        if decision.get("outcome") == "deny":
            raise NoetherDeniedError(decision)
        return decision

    def finalize(self, reservation_id: str, payload: JsonObject) -> JsonObject:
        quoted = urllib.parse.quote(reservation_id, safe="")
        return self._post_json(f"/v1/reservations/{quoted}/finalize", payload)

    def event(self, event: JsonObject) -> JsonObject:
        return self._post_json("/v1/events", event)

    def health(self) -> JsonObject:
        return self._get_json("/health")

    def with_decision(
        self,
        request: JsonObject,
        run: Callable[[JsonObject, "NoetherClient"], T],
    ) -> T:
        decision = self.require_authorization(request)
        return run(decision, self)

    def _get_json(self, path: str) -> JsonObject:
        return self._request_json("GET", path)

    def _post_json(self, path: str, payload: JsonObject) -> JsonObject:
        return self._request_json("POST", path, payload)

    def _request_json(
        self,
        method: str,
        path: str,
        payload: JsonObject | None = None,
    ) -> JsonObject:
        body = None
        headers = {"Accept": "application/json"}
        if payload is not None:
            body = json.dumps(payload).encode("utf-8")
            headers["Content-Type"] = "application/json"
        request = urllib.request.Request(
            f"{self.url}{path}",
            data=body,
            headers=headers,
            method=method,
        )
        try:
            with urllib.request.urlopen(request, timeout=self.timeout) as response:
                raw = response.read().decode("utf-8")
        except urllib.error.HTTPError as error:
            raise NoetherHttpError(error.code, error.read().decode("utf-8")) from error
        except urllib.error.URLError as error:
            raise NoetherError("Noether request failed") from error
        return json.loads(raw)


def _synthetic_decision(fail_mode: FailMode, error: Exception) -> JsonObject:
    outcome: DecisionOutcome = "allow" if fail_mode == "fail_open" else "deny"
    action: PolicyAction = "allow" if fail_mode == "fail_open" else "block"
    severity: DecisionSeverity = "warn" if fail_mode == "fail_open" else "deny"
    return {
        "decision_id": f"sdk-{fail_mode}",
        "outcome": outcome,
        "action": action,
        "explanations": [
            {
                "rule_id": "sdk.sidecar_unavailable",
                "reason": f"Noether sidecar unavailable; applying {fail_mode}",
                "severity": severity,
            }
        ],
        "created_at": datetime.now(UTC).isoformat(),
        "metadata": {"error": str(error)},
    }


__all__ = [
    "NoetherClient",
    "NoetherDeniedError",
    "NoetherError",
    "NoetherHttpError",
]
