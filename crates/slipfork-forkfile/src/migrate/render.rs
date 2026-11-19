//! `Forkfile` emitter: consumes the already-validated [`super::snow::Snow`] and produces the
//! document. Top-level blocks are separated by one blank line and the file leads with a single
//! provenance comment.

use std::fmt::Write as _;

use super::snow::Snow;
use super::template::quote_text;

/// Render the validated model. Deterministic: identical input yields identical bytes.
pub(super) fn render(snow: &Snow) -> String {
    let mut blocks: Vec<String> = Vec::new();
    if !snow.features.is_empty() {
        let mut block = String::from("features {\n");
        for feature in &snow.features {
            let _ = writeln!(block, "    {feature}");
        }
        block.push('}');
        blocks.push(block);
    }
    for (cluster, version) in &snow.pg_clusters {
        let mut block = format!("postgres {cluster} {{\n    version {version};\n");
        for database in snow
            .pg_databases
            .iter()
            .filter(|database| &database.cluster == cluster)
        {
            if database.pool {
                let _ = writeln!(block, "    database {};", database.id);
            } else {
                let _ = writeln!(block, "    database {} {{ pool = false }}", database.id);
            }
        }
        block.push('}');
        blocks.push(block);
    }
    for (id, version) in &snow.redis {
        blocks.push(format!("redis {id} {{\n    version {version}\n}}"));
    }
    for bucket in &snow.s3 {
        blocks.push(format!("s3 {bucket}"));
    }
    for domain in &snow.domains {
        blocks.push(format!("domain {domain}"));
    }
    for (group, vars) in &snow.env_groups {
        if vars.is_empty() {
            continue;
        }
        let mut block = format!("env {group} {{\n");
        for (key, expression) in vars {
            let _ = writeln!(block, "    {key} = {expression}");
        }
        block.push('}');
        blocks.push(block);
    }
    // Mode-scoped groups stay feature-gated, and precede the services that `use` them.
    for (mode, group, vars) in &snow.mode_envs {
        if vars.is_empty() {
            continue;
        }
        let mut block = format!("on {mode} {{\n    env {group} {{\n");
        for (key, expression) in vars {
            let _ = writeln!(block, "        {key} = {expression}");
        }
        block.push_str("    }\n}");
        blocks.push(block);
    }
    for component in &snow.components {
        blocks.push(render_service(snow, component));
    }
    let mut out = String::from("# migrated from snow.yml\n\n");
    out.push_str(&blocks.join("\n\n"));
    out.push('\n');
    out
}

fn render_service(snow: &Snow, component: &super::snow::Component) -> String {
    let image = snow
        .images
        .iter()
        .find(|image| image.id == component.image)
        .expect("validated: every component names a declared image");
    let mut block = match &image.context {
        Some(context) => format!("service {} ./{context} {{\n", component.id),
        None => format!("service {} from {} {{\n", component.id, image.reference),
    };
    if image.dockerfile != "Dockerfile" {
        let _ = writeln!(block, "    dockerfile {}", image.dockerfile);
    }
    // Build-only keys: upstream image groups may also be used at runtime by the same service, and
    // a `Forkfile` service cannot declare a key in both `build_env` and `env`.
    let runtime_keys: Vec<&String> = component
        .envs
        .iter()
        .filter_map(|group| snow.env_groups.get(group))
        .flat_map(|vars| vars.keys())
        .collect();
    for group in &image.build_envs {
        let Some(vars) = snow.env_groups.get(group) else {
            continue;
        };
        let build_only: Vec<(&String, &String)> = vars
            .iter()
            .filter(|(key, _expression)| !runtime_keys.contains(key))
            .collect();
        if build_only.is_empty() {
            continue;
        }
        block.push_str("    build_env {\n");
        for (key, expression) in build_only {
            let _ = writeln!(block, "        {key} = {expression}");
        }
        block.push_str("    }\n");
    }
    for group in &component.envs {
        let _ = writeln!(block, "    use env {group}");
    }
    if let Some(port) = component_port(snow, &component.id) {
        let _ = writeln!(block, "    port {port}");
    }
    if !component.internal_ports.is_empty() {
        let ports: Vec<String> = component
            .internal_ports
            .iter()
            .map(u16::to_string)
            .collect();
        let _ = writeln!(block, "    internal_ports [{}]", ports.join(", "));
    }
    if component.size_hint != "*" {
        let _ = writeln!(block, "    size {}", quote_text(&component.size_hint));
    }
    for ingress in snow
        .ingresses
        .iter()
        .filter(|ingress| ingress.component == component.id)
    {
        let _ = writeln!(block, "    route {}", ingress.prefix);
    }
    for hook in snow
        .hooks
        .iter()
        .filter(|hook| hook.component == component.id)
    {
        let _ = writeln!(block, "    {}: {}", hook.phase, render_argv(&hook.argv));
    }
    // Directives apply top-to-bottom: the unconditional `run` first, then each `on <mode>` override
    // (a later `run` would silently overwrite the mode command).
    let _ = writeln!(block, "    run {}", render_argv(&component.run));
    for (mode, argv) in &component.mode_runs {
        let _ = writeln!(block, "    on {mode}: run {}", render_argv(argv));
    }
    block.push('}');
    block
}

fn component_port(snow: &Snow, component: &str) -> Option<u16> {
    snow.ingresses
        .iter()
        .find(|ingress| ingress.component == component)
        .map(|ingress| ingress.port)
}

fn render_argv(argv: &[String]) -> String {
    argv.iter()
        .map(|word| quote_text(word))
        .collect::<Vec<_>>()
        .join(" ")
}
