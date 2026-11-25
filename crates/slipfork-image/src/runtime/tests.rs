//! `RUN` environment: colour hints defaulted, explicit image values preserved.

use super::{APT_CONF_TARGET, DEFAULT_PATH, step_env};
use crate::dockerfile::ImageConfig;

fn env_of(declared: &[(&str, &str)]) -> Vec<String> {
    let mut config = ImageConfig::default();
    for (key, value) in declared {
        config.env.insert((*key).to_owned(), (*value).to_owned());
    }
    step_env(&config)
}

fn value<'a>(env: &'a [String], key: &str) -> Option<&'a str> {
    let prefix = format!("{key}=");
    env.iter().find_map(|pair| pair.strip_prefix(&prefix))
}

#[test]
fn run_env_defaults_colour_hints_without_faking_a_tty() {
    let env = env_of(&[]);
    assert_eq!(value(&env, "PATH"), Some(DEFAULT_PATH));
    assert_eq!(value(&env, "TERM"), Some("xterm-256color"));
    assert_eq!(value(&env, "CLICOLOR"), Some("1"));
    assert_eq!(value(&env, "CLICOLOR_FORCE"), Some("1"));
    assert_eq!(value(&env, "COLORTERM"), Some("truecolor"));
    assert_eq!(value(&env, "APT_CONFIG"), Some(APT_CONF_TARGET));
    assert_eq!(value(&env, "FORCE_COLOR"), None);
}

#[test]
fn run_env_keeps_image_values_and_a_declared_no_color_opts_out() {
    let explicit = env_of(&[("TERM", "dumb"), ("CLICOLOR_FORCE", "0")]);
    assert_eq!(value(&explicit, "TERM"), Some("dumb"));
    assert_eq!(value(&explicit, "CLICOLOR_FORCE"), Some("0"));
    assert_eq!(value(&explicit, "COLORTERM"), Some("truecolor"));

    let off = env_of(&[("NO_COLOR", "1")]);
    assert_eq!(value(&off, "NO_COLOR"), Some("1"));
    assert_eq!(value(&off, "TERM"), None);
    assert_eq!(value(&off, "CLICOLOR_FORCE"), None);
}
