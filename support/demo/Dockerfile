FROM python:3.12-slim AS app

# Runtime dependency only: the app (`app/server.py`) and its SQL (`db/*.sql`) run from the
# bind-mounted checkout, so editing either and running `slipfork restart` applies instantly.
RUN pip install --no-cache-dir "psycopg[binary]>=3.2,<3.3"
