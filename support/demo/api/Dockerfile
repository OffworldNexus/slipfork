FROM python:3.13-slim AS dev
ENV PYTHONUNBUFFERED=1
RUN python -m pip install --no-cache-dir uv
WORKDIR /project/api

# The `uv` binary is inherited from the dev stage because `worker` and `pre-deploy` run
# `uv run --no-sync` against the `/app/.venv` that this stage's own `uv sync` creates. `exec`
# replaces the shell so `SIGTERM` reaches `uvicorn` directly (otherwise `slipfork down`/`restart`
# waits for the kill deadline).
FROM dev AS prod
ENV PYTHONUNBUFFERED=1
WORKDIR /app
COPY pyproject.toml uv.lock ./
RUN uv sync --frozen --no-dev
COPY . .
ENV PATH="/app/.venv/bin:$PATH"
EXPOSE 8000
CMD ["sh", "-c", "exec uvicorn app.main:app --host 0.0.0.0 --port ${PORT:-8000}"]
