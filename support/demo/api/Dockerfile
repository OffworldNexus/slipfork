# The `api` service image. `server.py` and `migrate.py` are bind-mounted from the
# checkout, so editing them and running `slipfork restart` needs no rebuild.
FROM python:3.12-slim AS api

RUN pip install --no-cache-dir "psycopg[binary]>=3.2,<3.3"
