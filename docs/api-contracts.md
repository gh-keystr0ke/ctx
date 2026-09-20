# HTTP API contracts

Alongside database interactions ([docs/architecture.md](architecture.md#static-database-interactions)), `ctx` deterministically recognizes HTTP endpoints a symbol exposes and outbound HTTP calls it makes, from Python source (FastAPI, Flask, `requests`, `httpx`, `aiohttp.ClientSession`, `urllib3.PoolManager`, and `http.client.HTTP(S)Connection`) and from OpenAPI 3.0/3.1 specifications (language-neutral). Recognized syntax becomes a normal `EXPOSES`/`CALLS_EXTERNAL` `FACT` edge with source evidence, retired like any other fact when the syntax disappears; unrecognized or wholly dynamic syntax produces no fact rather than a guess.

## Endpoints (`EXPOSES`)

```python
router = APIRouter(prefix="/v1")

@app.get("/subscriptions/{subscription_id}")
def get_subscription(
    subscription_id: str,
    request: Request,
    database = Depends(get_database),
    expand: bool = False,
) -> Subscription:
    ...

@router.post("/subscriptions")
def create_subscription(payload: CreateSubscription) -> Subscription:
    ...

@flask_app.route("/jobs/<int:job_id>", methods=["GET", "DELETE"])
def job(job_id: int):
    ...
```

`get_subscription` becomes one `GET /subscriptions/{subscription_id}` endpoint with `subscription_id` classified as a path parameter (its name matches a path segment), `expand` as an optional query parameter (it has a default), and `request`/`database` excluded (`Request`-typed and `Depends(...)` parameters are never contract parameters). `create_subscription` inherits the router's `/v1` prefix, and its undecorated `payload` argument is classified as the request body. `job` becomes two endpoints, `GET` and `DELETE` `/jobs/{job_id}` — Flask's `<int:job_id>` path-converter syntax normalizes to the same `{job_id}` shape FastAPI uses. A path built from a runtime expression (`@app.get(prefix + "/dynamic")`) produces no endpoint at all rather than a guessed path.

## Outbound calls (`CALLS_EXTERNAL`)

```python
requests.get("https://billing.internal/health")
httpx.post(f"https://billing.internal/subscriptions/{subscription_id}")
client.patch("https://billing.internal/subscriptions/{}".format(subscription_id))
requests.delete(dynamic_url)
```

The first three are recognized, with the interpolated segment normalized to `{param}` the same way a path parameter is. `requests.delete(dynamic_url)` — a call whose URL is a bare variable with no static template at all — produces no fact.

`aiohttp.ClientSession` is recognized the same way `httpx.Client` is (verb-named methods, tracked whether bound by assignment or `with ... as`). `urllib3.PoolManager` and `http.client.HTTPSConnection`/`HTTPConnection` use `.request("POST", url)` instead — recognized only when the verb is a string literal:

```python
http = urllib3.PoolManager()
http.request("POST", "https://billing.internal/events")
```

`http.request(verb, url)` where `verb` is a variable produces no fact, the same as a dynamic URL would.

```python
resp = requests.post("https://billing.internal/events", json={"kind": "renewed", "amount": amount})
status = resp.json()["status"]
```

`request_fields` becomes `["kind", "amount"]` (both keys are string literals in a dict literal passed directly as `json=`) and `response_fields` becomes `["status"]` (read through `resp`, which is assigned directly from the call and never reassigned before the read, in the same function). Passing the body as a variable (`json=payload`) or reading the response through a second function leaves the respective list empty rather than guessing.

```python
class StripeClient:
    def __init__(self, host):
        self._host = host  # injected via config/environment, not a literal

    def charge(self):
        requests.post(f"{self._host}/v1/charges")
```

`self._host` is never resolved to a value — it can't be, since it only exists at runtime — but the call is still recognized: `url` becomes `/v1/charges` and `host_expr` becomes `"self._host"`. A more complex prefix (`self._build_host()`, `self._host + suffix`) produces no fact, matching the rule against guessing from a dynamic expression.

```python
class StripeClient:
    def _request(self, method, url):
        return requests.request(method, url)

    def charge(self, charge_id):
        return self._request("POST", f"{self._host}/v1/charges/{charge_id}")
```

`_request` is recognized as an HTTP-wrapper method because its one direct call forwards the verb and URL untouched from its own ordinary parameters. The call in `charge` therefore becomes `POST /v1/charges/{param}` with `host_expr == Some("self._host")`, exactly as if the resolved direct request had appeared in `charge`. A bare URL variable at that call site remains unknown. Wrapper recognition is limited to one hop through `self.`/`cls.` inside the same class, one direct HTTP call expression or return (apart from a docstring and optional `await`), and fixed or bare-parameter verb/URL forwarding without transformation.

## OpenAPI specifications

Conventional `openapi.yaml`, `openapi.yml`, and `openapi.json` files are discovered automatically during `ctx index` regardless of configured `languages` or source include paths — normal excludes still apply. Every OpenAPI 3.0/3.1 path operation for `GET`/`POST`/`PUT`/`DELETE`/`PATCH`/`HEAD`/`OPTIONS`/`TRACE` becomes its own `ApiEndpoint`, retaining `operationId`, summary/description, deprecation, tags, effective security and servers, path/query/header/cookie parameters, request-body content and schema, and response content/schema metadata; local `$ref` values are followed. An invalid or unsupported specification (not OpenAPI 3.x, missing `paths`) is reported as a failed file with an explicit reason, never partially parsed.

OpenAPI-derived endpoints are exported as public HTTP contracts even when no product document mentions them (`ctx export`, [Federation](#federation) below).

### Code and OpenAPI describing the same route

When a code handler and an OpenAPI operation both declare the same `(method, path)`, the OpenAPI contract is authoritative: it carries the richer, spec-derived parameter/response metadata the code-derived contract can't. `ctx index`/`ctx impact`/`ctx explain` and `ctx export` all show one merged endpoint, not two — `ctx export`'s manifest keeps the real code handler as the `handler` (so trace tooling still points at callable code) when one exists, falling back to the OpenAPI operation symbol only when no code handler declares the route, and combines evidence from both the code and the OpenAPI edge.

## Where these show up

Endpoints and outbound calls appear in `ctx impact`/`ctx explain`/`ctx context` like any other node, and in `ctx status` counts. `ctx review` reports HTTP contract changes as their own `api_findings` stream (parallel to `schema_findings` for database changes), each classified `added`/`removed`/`contract-modified`, flagged destructive or not, with the same bounded advisory link to the requirements/invariants/tests the changed handler's mapped code implements — an empty list means no mapping is known, not that the change is safe.

## Federation

`ctx export` includes every currently-indexed endpoint (regardless of any document's `visibility`, since an endpoint is structural fact, not a product-context document) in the manifest it writes. `ctx sync` resolves your own outbound `CALLS_EXTERNAL` facts against every synced neighbor's exported endpoints. See [docs/federation.md](federation.md).

## Current limits

- From code: Python only; FastAPI and Flask are the only recognized frameworks. Outbound calls recognize `requests`, `httpx`, `aiohttp.ClientSession`, `urllib3.PoolManager`, and `http.client.HTTP(S)Connection`. `GET`/`POST`/`PUT`/`DELETE`/`PATCH` are the only recognized HTTP methods.
- From OpenAPI: 3.0 and 3.1 documents only (Swagger 2.0 and earlier are rejected, not down-converted); only local `$ref`s are followed, an external/remote `$ref` is left as-is rather than fetched.
- Path/query/body parameter classification from code is heuristic (path-segment name match, then a "looks like a request-body type" check, then query as the default) — not a full FastAPI/Pydantic type-system evaluation.
- A dynamic route or call URL (anything not a string literal or a literal with simple interpolation) yields no fact instead of a guess.
- An HTTP-wrapper method is recognized only one hop deep, only via `self.`/`cls.` within the same class (never a base class, mixin, another class in the file, a module-level function, or an imported wrapper). `self.` is accepted only for an undecorated method led by `self`; `cls.` is accepted only for an exact bare `@classmethod` led by `cls`; static methods and other decorated methods are excluded. Apart from an optional docstring, its body must be exactly one direct HTTP call expression or a return of that call (optionally awaited), and its verb/URL inputs must be fixed literals/method names or ordinary positional-or-keyword parameters forwarded as bare identifiers into the direct client's existing positional verb/URL slots. Dynamic call-site URLs, transformed or reassigned parameters, positional-only/keyword-only/variadic signatures, larger bodies, and multiple HTTP-shaped calls produce no wrapper fact rather than a guess. Request/response field lists on wrapper-derived calls remain unknown because verb/URL forwarding does not prove payload forwarding or response pass-through.
