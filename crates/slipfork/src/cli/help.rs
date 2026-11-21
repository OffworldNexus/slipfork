//! The shipped help screens: one `-h` page per verb, no ANSI when colour is off.

use crate::render::paint;

use super::surface;
use super::{HelpTopic, VERSION};

pub(super) fn help_text(topic: HelpTopic, color: bool) -> String {
    match topic {
        HelpTopic::Global => {
            let title = paint(&format!("slipfork {VERSION}"), "1;36", color);
            format!(
                "{title}\n\
                 Copy-on-write Btrfs environments for polyglot apps and AI coding agents.\n\
                 \n\
                 Usage: slipfork <command> [options]\n\
                 \n\
                 Commands:\n\
                 {}\
                 \n\
                 Examples:\n\
                 \x20 slipfork\n\
                 \x20 slipfork up\n\
                 \x20 slipfork restart worker\n\
                 \x20 slipfork logs -f api\n\
                 \x20 slipfork exec api -- pytest -q\n\
                 \x20 slipfork daemon install\n\
                 \n\
                 Options:\n\
                 \x20 -h, --help      Print this overview\n\
                 \x20 -V, --version   Print the version\n",
                surface::commands_block()
            )
        }
        HelpTopic::Status => {
            let title = paint("slipfork status", "1;36", color);
            format!(
                "{title}\n\
                 Show every running world as one tree; bare `slipfork` is this verb.\n\
                 \n\
                 Usage: slipfork status [--json] [-v]\n\
                 \n\
                 Options:\n\
                 \x20 --json        Emit the raw ps object with zero ANSI\n\
                 \x20 -v, --verbose Add each world's supervisor pid\n\
                 \x20 -h, --help\n\
                 \n\
                 Example:\n\
                 \x20 slipfork status\n"
            )
        }
        HelpTopic::Ps => {
            let title = paint("slipfork ps", "1;36", color);
            format!(
                "{title}\n\
                 Show only the world or fork rooted at the current directory.\n\
                 \n\
                 Usage: slipfork ps [--json] [-v]\n\
                 \n\
                 Options:\n\
                 \x20 --json        Emit the raw ps object with zero ANSI\n\
                 \x20 -v, --verbose Add the supervisor pid\n\
                 \x20 -h, --help\n\
                 \n\
                 Example:\n\
                 \x20 slipfork ps --json\n"
            )
        }
        HelpTopic::Doctor => {
            let title = paint("slipfork doctor", "1;36", color);
            format!(
                "{title}\n\
                 Check the only three host requirements: Btrfs, namespaces, and runtime.\n\
                 \n\
                 Usage: slipfork doctor [--json]\n\
                 \n\
                 Options:\n\
                 \x20 --json   Emit {{\"ready\",\"checks\"}} with zero ANSI\n\
                 \x20 -h, --help\n\
                 \n\
                 Example:\n\
                 \x20 slipfork doctor\n"
            )
        }
        HelpTopic::DaemonInstall => {
            let title = paint("slipfork daemon install", "1;36", color);
            format!(
                "{title}\n\
                 Write the per-user daemon unit; `--ingress` adds the system :80 socket.\n\
                 \n\
                 Usage: slipfork daemon install [--ingress] [--json]\n\
                 \n\
                 Options:\n\
                 \x20 --ingress   Install slipfork-ingress.socket, its template unit, and AppArmor (sudo)\n\
                 \x20 --json      Emit {{\"installed\",\"ingress\"}} with zero ANSI\n\
                 \x20 -h, --help\n\
                 \n\
                 Example:\n\
                 \x20 sudo slipfork daemon install --ingress\n"
            )
        }
        HelpTopic::Up => {
            let title = paint("slipfork up", "1;36", color);
            format!(
                "{title}\n\
                 Boot images, resources, setup, hooks, and services from ./Forkfile.\n\
                 \n\
                 Usage: slipfork up [--world NAME] [--json]\n\
                 \n\
                 Options:\n\
                 \x20 --world NAME   World name (default: the project directory name)\n\
                 \x20 --json         Stream typed boot events as NDJSON\n\
                 \x20 -h, --help\n\
                 \n\
                 Example:\n\
                 \x20 slipfork up\n"
            )
        }
        HelpTopic::Down => {
            let title = paint("slipfork down", "1;36", color);
            format!(
                "{title}\n\
                 Stop this directory's running world and every pod in it.\n\
                 \n\
                 Usage: slipfork down [--json]\n\
                 \n\
                 Options:\n\
                 \x20 --json   Emit {{\"stopped\":true}} as JSON\n\
                 \x20 -h, --help\n\
                 \n\
                 Example:\n\
                 \x20 slipfork down\n"
            )
        }
        HelpTopic::Restart => {
            let title = paint("slipfork restart", "1;36", color);
            format!(
                "{title}\n\
                 Restart one service from its existing bundle, or every service.\n\
                 \n\
                 Usage: slipfork restart [SERVICE] [--json]\n\
                 \n\
                 Arguments:\n\
                 \x20 SERVICE   Service id to restart (default: every service)\n\
                 \n\
                 Options:\n\
                 \x20 --json   Emit {{\"restarted\":N}} as JSON\n\
                 \x20 -h, --help\n\
                 \n\
                 Example:\n\
                 \x20 slipfork restart worker\n"
            )
        }
        HelpTopic::Fork => {
            let title = paint("slipfork fork", "1;36", color);
            format!(
                "{title}\n\
                 Snapshot the project and its world data, then boot the fork on its own URL.\n\
                 \n\
                 Usage: slipfork fork [NAME] [--json]\n\
                 \n\
                 Arguments:\n\
                 \x20 NAME     Fork and branch name (default: the checkout's git branch)\n\
                 \n\
                 Options:\n\
                 \x20 --json   Emit {{\"fork\",\"project\",\"url\",\"elapsed_ms\"}} as JSON\n\
                 \x20 -h, --help\n\
                 \n\
                 Example:\n\
                 \x20 slipfork fork feat-auth\n"
            )
        }
        HelpTopic::Drop => {
            let title = paint("slipfork drop", "1;36", color);
            format!(
                "{title}\n\
                 Stop a fork and delete its project and world-data snapshots.\n\
                 \n\
                 Usage: slipfork drop NAME [-f] [--json]\n\
                 \n\
                 Arguments:\n\
                 \x20 NAME        Fork to stop and delete\n\
                 \n\
                 Options:\n\
                 \x20 -f, --force Discard uncommitted changes in the fork checkout\n\
                 \x20 --json      Emit {{\"dropped\",\"elapsed_ms\"}} as JSON\n\
                 \x20 -h, --help\n\
                 \n\
                 Example:\n\
                 \x20 slipfork drop feat-auth\n"
            )
        }
        HelpTopic::Logs => {
            let title = paint("slipfork logs", "1;36", color);
            format!(
                "{title}\n\
                 Stream merged stdout + stderr with a colored gutter per service.\n\
                 \n\
                 Usage: slipfork logs [-f] [--json] [SERVICE]\n\
                 \n\
                 Arguments:\n\
                 \x20 SERVICE   Only this service; omit to merge every pod\n\
                 \n\
                 Options:\n\
                 \x20 -f, --follow  Stay attached across restarts (Ctrl-C exits 0)\n\
                 \x20 --json        One {{\"pod\",\"stream\",\"line\",\"kind\"}} per line, then {{\"exit\":0}}\n\
                 \x20 -h, --help\n\
                 \n\
                 Example:\n\
                 \x20 slipfork logs -f api\n"
            )
        }
        HelpTopic::Shell => {
            let title = paint("slipfork shell", "1;36", color);
            format!(
                "{title}\n\
                 Open `sh -i` inside a service or image pod.\n\
                 \n\
                 Usage: slipfork shell [--json] [SERVICE|IMAGE]\n\
                 \n\
                 The shell runs over plain pipes: no PTY is allocated, so prompts and job\n\
                 control are limited, but `cd` and exports behave normally.\n\
                 \n\
                 Options:\n\
                 \x20 --json   Emit events and a final {{\"exit\":n}}\n\
                 \x20 -h, --help\n\
                 \n\
                 Example:\n\
                 \x20 slipfork shell api\n"
            )
        }
        HelpTopic::Exec => {
            let title = paint("slipfork exec", "1;36", color);
            format!(
                "{title}\n\
                 Run a command inside a pod and forward its exit code.\n\
                 \n\
                 Usage: slipfork exec [--json] [SERVICE|IMAGE] -- <CMD...>\n\
                 \n\
                 Options:\n\
                 \x20 --json   Emit events and a final {{\"exit\":n}}\n\
                 \x20 -h, --help\n\
                 \n\
                 Stdin is forwarded to the command on a pipe, so `cat`, `pytest -q`, and scripts\n\
                 read redirected input exactly as they would locally.\n\
                 \n\
                 Example:\n\
                 \x20 slipfork exec api -- pytest -q\n"
            )
        }
        HelpTopic::Db => {
            let title = paint("slipfork db", "1;36", color);
            format!(
                "{title}\n\
                 Open psql against the world's postgres; piped stdin executes SQL.\n\
                 \n\
                 Usage: slipfork db [--json] [DATABASE]\n\
                 \n\
                 Options:\n\
                 \x20 --json   Emit events and a final {{\"exit\":n}}\n\
                 \x20 -h, --help\n\
                 \n\
                 Example:\n\
                 \x20 slipfork db < schema.sql\n"
            )
        }
        HelpTopic::Redis => {
            let title = paint("slipfork redis", "1;36", color);
            format!(
                "{title}\n\
                 Open redis-cli against the world's redis; piped stdin runs commands.\n\
                 \n\
                 Usage: slipfork redis [--json] [ID]\n\
                 \n\
                 Options:\n\
                 \x20 --json   Emit events and a final {{\"exit\":n}}\n\
                 \x20 -h, --help\n\
                 \n\
                 Example:\n\
                 \x20 slipfork redis\n"
            )
        }
        HelpTopic::Fmt => {
            let title = paint("slipfork fmt", "1;36", color);
            format!(
                "{title}\n\
                 Format a Forkfile in place, or report with `--check`.\n\
                 \n\
                 Usage: slipfork fmt [--check] [--json] [PATH]\n\
                 \n\
                 Arguments:\n\
                 \x20 PATH      Forkfile to format (default: Forkfile)\n\
                 \n\
                 Options:\n\
                 \x20 --check   Report whether PATH is formatted; exit 1 when not\n\
                 \x20 --json    Emit {{\"path\",\"formatted\",\"changed\"}} as JSON\n\
                 \x20 -h, --help\n\
                 \n\
                 Example:\n\
                 \x20 slipfork fmt --check Forkfile\n"
            )
        }
        HelpTopic::Completions => {
            let title = paint("slipfork completions", "1;36", color);
            format!(
                "{title}\n\
                 Print a completion script for bash, zsh, or fish.\n\
                 \n\
                 Usage: slipfork completions <bash|zsh|fish>\n\
                 \n\
                 Arguments:\n\
                 \x20 SHELL   bash, zsh, or fish\n\
                 \n\
                 Options:\n\
                 \x20 -h, --help\n\
                 \n\
                 Example:\n\
                 \x20 slipfork completions bash > /etc/bash_completion.d/slipfork\n"
            )
        }
        HelpTopic::Migrate => {
            let title = paint("slipfork migrate", "1;36", color);
            format!(
                "{title}\n\
                 Convert a legacy snow.yml manifest into a Forkfile.\n\
                 \n\
                 Usage: slipfork migrate [--write] [--force] [--json] [PATH]\n\
                 \n\
                 Arguments:\n\
                 \x20 PATH     snow.yml manifest to convert (default: snow.yml)\n\
                 \n\
                 Options:\n\
                 \x20 --write  Save the result as Forkfile next to PATH\n\
                 \x20 --force  Overwrite an existing Forkfile\n\
                 \x20 --json   Emit {{\"path\",\"written\",\"forkfile\"}} as JSON\n\
                 \x20 -h, --help\n\
                 \n\
                 Example:\n\
                 \x20 slipfork migrate --write snow.yml\n"
            )
        }
    }
}
