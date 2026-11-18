FROM python:3.12-slim AS app

RUN pip install --no-cache-dir "psycopg[binary]>=3.2,<3.3"

WORKDIR /app
COPY app/ /app/
COPY db/ /app/db/

CMD ["python3", "/app/server.py"]
