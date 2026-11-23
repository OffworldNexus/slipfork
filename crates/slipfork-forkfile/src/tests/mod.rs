use super::*;

mod check;
mod commands;
mod format;
mod parser;

const SAMPLE: &str = r#"# forkfile: 0.1

features {
  default = [dev]
  dev = [mail]
  mail
}

domain main
postgres pg { database app_db }
redis cache
s3 uploads
s3 assets {
  policy public-read
  cdn static-cdn
}

env common {
  DATABASE_URL         = app_db.url
  REDIS_URL            = cache.url
  UPLOADS_URL          = uploads.url
  AWS_S3_CUSTOM_DOMAIN = static-cdn.domain
  PUBLIC_URL           = self.url
  PRIMARY_DOMAIN       = main.name
  SECRET_KEY           = secret(SECRET_KEY, random_hex(32))
  on mail: SMTP_ADDR   = "127.0.0.1:1025"
}

image backend . {
  on dev: stage dev
  use env common

  service api {
port 8000
route /api
pre-deploy: @uv run alembic upgrade head
run: uv run gunicorn app.main:app --bind 0.0.0.0:$port
on dev: run: @uv run uvicorn app.main:app --reload --host 0.0.0.0 --port $port
  }

  service worker {
run: uv run celery -A app.worker worker
on dev: run: @uv run celery -A app.worker worker
  }
}

image frontend ./front {
  arg VITE_CDN_URL = static-cdn.url
  on dev: stage dev

  service front {
port 4173
route /
run: pnpm run preview -- --host 0.0.0.0 --port $port
on dev {
  port 5173
  run: @pnpm run dev -- --host 0.0.0.0 --port $port
}
  }
}

on mail {
  domain mail

  image mailpit from axllent/mailpit:latest {
service mailpit {
  port 8025
  route / domain mail
}
  }
}
"#;

fn expect_render(source: &str) -> String {
    let diagnostics = parse(source).expect_err("expected diagnostics");
    diagnostics.render(source)
}

fn first_image(doc: &Forkfile) -> &Image {
    doc.items
        .items
        .iter()
        .find_map(|item| match item {
            TopItem::Image(image) => Some(image),
            _ => None,
        })
        .expect("expected an image")
}

fn first_service(image: &Image) -> &Service {
    image
        .body
        .items
        .iter()
        .find_map(|item| match item {
            ImageItem::Service(service) => Some(service),
            _ => None,
        })
        .expect("expected a service")
}

fn service_routes(service: &Service) -> Vec<&Route> {
    service
        .body
        .items
        .iter()
        .filter_map(|item| match item {
            ServiceItem::Route(route) => Some(route),
            _ => None,
        })
        .collect()
}
