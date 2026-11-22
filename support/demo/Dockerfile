FROM python:3.13-slim AS dev
ENV PYTHONUNBUFFERED=1
RUN python -m pip install --no-cache-dir uv
WORKDIR /project

FROM dev AS prod
COPY pyproject.toml uv.lock ./
RUN uv sync --frozen --no-dev
COPY . .
