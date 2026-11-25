//! `image.build`: compile the `Forkfile`, plan one component, and stream typed build frames.
//!
//! The daemon performs the exact flow the CLI used to: external `COPY --from` pulls, the native
//! builder with `Cache::Cold`/`Warm`, `StepLog` events, and the `built.json` record. Every event
//! leaves as an [`ImageFrame`]; the CLI owns all rendering.

use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;

use serde_json::{Value, json};
use slipfork_forkfile::compile;
use slipfork_image::{
    BuildArgs, BuildEvent, BuildRequest, Cache, NativeImageBuilder, StepLog,
    planned_instruction_width,
};
use slipfork_world::world_plan_with;

use super::{ImageFrame, ImageLab, ImageRequest, client_closed, failure, now_ms, record_built};
use crate::world::HostEnv;
use crate::world::features::WorldFeatures;

/// Builds one declared image component and streams its rows as typed frames.
pub(crate) fn build(
    host: &HostEnv,
    params: &Value,
    emit: &mut dyn FnMut(Value) -> bool,
) -> Result<Option<Value>, super::Failure> {
    let request = ImageRequest::parse(params)?;
    let component = params
        .get("component")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            failure(
                "bad-params",
                "`component` is required",
                "pass the image id declared in `./Forkfile`",
            )
        })?
        .to_owned();
    let target = params
        .get("target")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let no_cache = params.get("no_cache").and_then(Value::as_bool) == Some(true);

    let path = request.project.join("Forkfile");
    let source = match fs::read_to_string(&path) {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(failure(
                "no-forkfile",
                format!("no Forkfile in `{}`", request.project.display()),
                "create a Forkfile with an `image` block, then retry",
            ));
        }
        Err(error) => {
            return Err(failure(
                "forkfile",
                format!("cannot read `{}`: {error}", path.display()),
                "check the Forkfile permissions and retry",
            ));
        }
    };
    let checked = compile(&source).map_err(|diagnostics| {
        let diagnostic = diagnostics.first();
        let message = diagnostic.map_or_else(
            || "the Forkfile is invalid".to_owned(),
            |diagnostic| {
                format!(
                    "Forkfile:{}: {}",
                    diagnostic.span.line(),
                    diagnostic.message
                )
            },
        );
        let next = diagnostic.map_or_else(
            || "run `slipfork fmt` after fixing the first error".to_owned(),
            |diagnostic| diagnostic.next.clone(),
        );
        failure("invalid-forkfile", message, &next)
    })?;
    let doc = &checked.document;
    let features = WorldFeatures::load_resolved(&request.project, doc)
        .map_err(|message| failure("feature", message, WorldFeatures::REMEDY))?;
    let plan = world_plan_with(doc, &request.project, &features).map_err(|error| {
        failure(
            "forkfile-plan",
            error.to_string(),
            "fix the Forkfile, then run `slipfork doctor`",
        )
    })?;
    let Some(node) = plan.images.iter().find(|node| node.id == component) else {
        return Err(failure(
            "unknown-component",
            format!("no image component `{component}` in this Forkfile"),
            "run `slipfork status` to see this world's declared images",
        ));
    };
    if let Some(from) = &node.from {
        return Err(failure(
            "not-buildable",
            format!("image `{component}` is pulled from `{from}`, not built"),
            &format!("run `slipfork docker pull {from}` instead"),
        ));
    }

    let paths = node.paths(&request.project);
    let target = target.or_else(|| doc.eval_image_stage_with(&component, &features));
    let world = crate::world::world_name(&request.project, None)
        .map_or_else(|_| "slipfork".to_owned(), |world| world.as_str().to_owned());
    let mut build_args = BuildArgs::new();
    for (key, value) in doc.eval_image_args_with(&world, "http", &component, &features) {
        build_args.declare(key, value);
    }
    let build_request = BuildRequest {
        context: paths.context,
        dockerfile: paths.dockerfile,
        args: build_args,
        target,
        externals: BTreeMap::new(),
    };
    let lab = ImageLab::for_project(host, &request.project)?;
    let step_host = lab.step_host(host)?;
    let source = fs::read_to_string(&build_request.dockerfile).map_err(|error| {
        failure(
            "dockerfile",
            format!(
                "cannot read `{}`: {error}",
                build_request.dockerfile.display()
            ),
            "create the Dockerfile named by this image, then retry",
        )
    })?;
    let planned_width =
        planned_instruction_width(&source, build_request.target.as_deref()).unwrap_or(0);
    let puller = lab.puller(host);
    let mut closed = false;
    let mut pump = |frame: ImageFrame| {
        if closed {
            return;
        }
        if !emit(frame.value()) {
            closed = true;
        }
    };
    let outcome = {
        let mut bus = |event: BuildEvent<'_>| pump(ImageFrame::from_event(event));
        let mut log = StepLog::with_events(&mut bus);
        log.plan_width(planned_width);
        let pulled =
            super::pull_externals(&source, build_request.target.as_deref(), &puller, &mut log)
                .map_err(|error| {
                    failure(
                        "image-build",
                        format!("pull external image failed: {}", error.cause()),
                        "check the `COPY --from` reference and registry credentials, then retry",
                    )
                })?;
        let build_puller = lab.puller(host).sharing_flights(&puller);
        let builder = NativeImageBuilder::new(
            Arc::clone(&lab.engine),
            build_puller,
            lab.store.clone(),
            lab.architecture,
            step_host,
        )
        .with_cache(if no_cache { Cache::Cold } else { Cache::Warm });
        builder.build(
            &BuildRequest {
                externals: pulled,
                ..build_request.clone()
            },
            &mut log,
            now_ms(),
        )
    };
    if closed {
        return Err(client_closed());
    }
    let report = outcome.map_err(|error| {
        failure(
            "image-build",
            format!("build `{component}` failed: {}", error.cause()),
            "fix the failing Dockerfile instruction and retry, or run `slipfork logs` after `up`",
        )
    })?;
    let cache_key = report.key.as_hex();
    let image_id = format!("sha256:{cache_key}");
    let digest = report.base_digest;
    let index = lab.store.images_root().join("built.json");
    record_built(
        &index,
        &component,
        build_request.target.as_ref(),
        &image_id,
        digest.as_deref(),
    )
    .map_err(|error| {
        failure(
            "image-index",
            format!("cannot record the built image: {error}"),
            "check that the image store is writable, then retry",
        )
    })?;
    if closed {
        return Err(client_closed());
    }
    let mut value = json!({
        "component": component,
        "target": build_request.target,
        "image_id": image_id,
        "cache_key": cache_key,
        "rootfs": report.rootfs.display().to_string(),
        "cache_hits": report.cache_hits,
        "cache_misses": report.cache_misses,
        "no_cache": no_cache,
        "exit": 0,
    });
    if let Some(digest) = &digest {
        value["digest"] = json!(digest);
    }
    Ok(Some(value))
}
