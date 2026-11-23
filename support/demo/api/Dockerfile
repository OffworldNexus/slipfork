FROM python:3.13-slim AS dev
ENV PYTHONUNBUFFERED=1
RUN python -m pip install --no-cache-dir uv
WORKDIR /project/api

FROM dev AS prod
WORKDIR /app
COPY pyproject.toml uv.lock ./
RUN uv sync --frozen --no-dev
COPY . .
ENV PATH="/app/.venv/bin:$PATH"
EXPOSE 8000
CMD ["sh", "-c", "uvicorn app.main:app --host 0.0.0.0 --port ${PORT:-8000}"]
