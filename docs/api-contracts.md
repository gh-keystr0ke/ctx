# HTTP API contracts

Alongside database interactions ([docs/architecture.md](architecture.md#static-database-interactions)), `ctx` deterministically recognizes HTTP endpoints a symbol exposes and outbound HTTP calls it makes, from Python source (FastAPI, Flask, `requests`, `httpx`) and from OpenAPI 3.0/3.1 specifications (language-neutral). Recognized syntax becomes a normal `EXPOSES`/`CALLS_EXTERNAL` `FACT` edge with source evidence, retired like any other fact when the syntax disappears; unrecognized or wholly dynamic syntax produces no fact rather than a guess.

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

- From code: Python only; FastAPI and Flask are the only recognized frameworks, `requests`/`httpx` the only recognized outbound clients. `GET`/`POST`/`PUT`/`DELETE`/`PATCH` are the only recognized HTTP methods.
- From OpenAPI: 3.0 and 3.1 documents only (Swagger 2.0 and earlier are rejected, not down-converted); only local `$ref`s are followed, an external/remote `$ref` is left as-is rather than fetched.
- Path/query/body parameter classification from code is heuristic (path-segment name match, then a "looks like a request-body type" check, then query as the default) — not a full FastAPI/Pydantic type-system evaluation.
- A dynamic route or call URL (anything not a string literal or a literal with simple interpolation) yields no fact instead of a guess.
